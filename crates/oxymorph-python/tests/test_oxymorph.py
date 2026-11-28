"""Tests of the `oxymorph` Python package: the native `#[pyclass]` object model (decisions/0012).

Run with `just test-python`.
"""

import copy
import hashlib
import importlib.machinery
import importlib.util
import inspect
import json
import pickle
import unittest
from pathlib import Path

import oxymorph

_case = unittest.TestCase()


def raises(exception, match=None):
    """`assertRaises`, or `assertRaisesRegex` (a `re.search`) when `match` is given."""
    if match is None:
        return _case.assertRaises(exception)
    return _case.assertRaisesRegex(exception, match)


KF_DATA = Path(__file__).resolve().parents[2] / "oxymorph-netlist" / "tests" / "data" / "kf"

RICH_SPICE = """* rich deck
.include models.lib
.param w=1u
.subckt inv a y w=1u
M1 y a 0 0 nmos W={w*2} L=100n
.ends
X1 in out inv
V1 in 0 DC 0.9 PULSE(0 1.8 1n 10p 10p 1n 2n)
.tran 1n 100n
.end
"""


def build_two_terminal_circuit():
    """gfpex `netlist_build_two_terminal_circuit`: one resistor on two nodes, at the top level."""
    top = oxymorph.module("__top__")
    top.add(oxymorph.module_reference("R0", "resistor", value=2.0, properties={"net": "vdd"}))
    top.connect("vdd_34_0_0", ("R0", "p"), ("R0", "p"))
    top.connect("vdd_34_0_1", ("R0", "n"), ("R0", "n"))
    device = oxymorph.ext_module(
        "resistor", ports=[oxymorph.port("p"), oxymorph.port("n")], properties={"spice_prefix": "R"}
    )
    return oxymorph.Circuit(name="vdd_resistance", top_module="__top__").add_ext(device).add(top)


# ---------------------------------------------------------------------------
# Brief tests
# ---------------------------------------------------------------------------


def test_nested_mutation_is_written():
    circuit = oxymorph.Circuit(name="n", top_module="__top__")
    circuit.add_ext(oxymorph.ext_module("resistor", ports=[oxymorph.port("p"), oxymorph.port("n")]))
    circuit.ext_modules[0].properties["spice_prefix"] = "R"
    circuit.add(oxymorph.module("__top__"))
    top = circuit.modules[0]
    top.module_references.append(oxymorph.module_reference("R0", "resistor", value=2))
    top.connections.extend(
        [oxymorph.connection("a", ("R0", "p"), ("R0", "p")), oxymorph.connection("b", ("R0", "n"), ("R0", "n"))]
    )
    assert "R0 a b 2" in oxymorph.write_spice(circuit)


def test_top_module_written_without_subckt():
    written = oxymorph.write_spice(build_two_terminal_circuit())
    assert ".subckt" not in written.lower()
    assert "R0 vdd_34_0_0 vdd_34_0_1 2" in written


def test_deepcopy_is_independent():
    original = build_two_terminal_circuit()
    copied = copy.deepcopy(original)
    assert copied == original
    copied.modules[0].module_references.clear()
    copied.properties["backannotated"] = "true"
    assert len(original.modules[0].module_references) == 1
    assert "backannotated" not in original.properties
    assert copied != original


def test_equality_by_value():
    assert build_two_terminal_circuit() == build_two_terminal_circuit()
    assert oxymorph.port("p", domain=1) == oxymorph.port("p", domain=oxymorph.SignalDomain.ELECTRICAL)
    changed = build_two_terminal_circuit()
    changed.modules[0].module_references[0].properties["net"] = "gnd"
    assert changed != build_two_terminal_circuit()
    assert oxymorph.Circuit.from_yaml(changed.to_yaml()) == changed
    assert oxymorph.Circuit.from_json(changed.to_json()) == changed
    assert oxymorph.Circuit.from_proto_bytes(changed.to_proto_bytes()) == changed


def test_connect_chains_pairs():
    module = oxymorph.module("m")
    endpoint_a, endpoint_b, endpoint_c = ("X1", "a"), ("X2", "b"), ("", "VDD")
    assert module.connect("VDD", endpoint_a, endpoint_b, endpoint_c) is module
    pairs = [
        (
            (connection.source.instance_name, connection.source.port_name),
            (connection.target.instance_name, connection.target.port_name),
        )
        for connection in module.connections
    ]
    assert pairs == [(endpoint_a, endpoint_b), (endpoint_b, endpoint_c)]
    assert {connection.name for connection in module.connections} == {"VDD"}
    with raises(ValueError, match="need ≥ 2 endpoints, got 1"):
        module.connect("VDD", endpoint_a)
    with raises(ValueError, match="got 0"):
        module.connect("VDD")
    assert len(module.connections) == 2


def test_enums_have_members():
    assert oxymorph.SIPrefix.MICRO == 9
    assert oxymorph.SignalDomain.WAVEGUIDE == 2
    assert oxymorph.PortDirection.INPUT == 1
    assert [member.name for member in oxymorph.SIPrefix][:2] == ["UNSPECIFIED", "QUECTO"]
    assert oxymorph.SIPrefix.QUETTA == 24
    assert len(oxymorph.SIPrefix) == 25 and oxymorph.SIPrefix.RONNA == 23
    assert [member.name for member in oxymorph.SignalDomain] == ["UNSPECIFIED", "ELECTRICAL", "WAVEGUIDE"]
    for enum_class in (
        oxymorph.DirectiveKind,
        oxymorph.ExternalModuleKind,
        oxymorph.AnalysisKind,
        oxymorph.OutputRequestKind,
        oxymorph.WaveformKind,
        oxymorph.Operator,
    ):
        assert enum_class(0).name == "UNSPECIFIED"
    parsed = oxymorph.parse_spice(".subckt cell a\n.ends\n")
    assert parsed.modules[0].ports[0].direction is oxymorph.PortDirection.INOUT


def test_to_json_is_stable_for_hashing():
    first, second = build_two_terminal_circuit(), build_two_terminal_circuit()
    assert first.to_json() == second.to_json()
    assert first.to_json() == first.to_json()
    assert json.loads(first.to_json())["top_module"] == "__top__"
    second.modules[0].module_references[0].parameters[0].default_value.prefixed_value.double_value = 3.0
    assert first.to_json() != second.to_json()


@unittest.skipUnless(importlib.util.find_spec("deepdiff"), "deepdiff is not installed")
def test_deephash_is_stable():
    import deepdiff

    first, second = build_two_terminal_circuit(), build_two_terminal_circuit()
    first_hash = deepdiff.DeepHash(first)[first]
    assert first_hash == deepdiff.DeepHash(second)[second]
    second.modules[0].module_references[0].properties["net"] = "gnd"
    assert first_hash != deepdiff.DeepHash(second)[second]


# ---------------------------------------------------------------------------
# Object model
# ---------------------------------------------------------------------------


def test_type_error_names_the_path():
    circuit = build_two_terminal_circuit()
    circuit.modules[0].module_references.append("R9")
    with raises(TypeError, match=r"modules\[0\]\.module_references\[1\]: expected ModuleReference, got str"):
        oxymorph.write_spice(circuit)
    circuit = build_two_terminal_circuit()
    circuit.properties["count"] = 3
    with raises(TypeError, match=r"properties\['count'\]: expected str, got int"):
        circuit.to_json()


def test_cycle_is_a_value_error():
    expression = oxymorph.Expression()
    expression.unary = oxymorph.UnaryOp(op=oxymorph.Operator.NEG, operand=expression)
    with raises(ValueError, match="nested deeper than 128"):
        oxymorph.format_expression(expression, "spectre")
    circuit = build_two_terminal_circuit()
    value = oxymorph.ParameterValue(model_ref=oxymorph.ModelReference(model_interface_name="m"))
    value.model_ref.arguments["self"] = value
    circuit.modules[0].module_references[0].parameters.append(oxymorph.parameter("loop", default_value=value))
    with raises(ValueError, match=r"^modules\[0\]\.module_references\[0\]\.parameters\[1\]\.default_value"):
        oxymorph.write_spice(circuit)
    with raises(ValueError):
        circuit.to_json()


def nested_calls(depth):
    """`f(f(...f(1e-6)...))`: `depth` nodes, calls (the costliest node) over a literal leaf."""
    return "f(" * (depth - 1) + "1e-6" + ")" * (depth - 1)


def deck_with(text):
    return f"* t\n.param a=1\nR1 x y r={{{text}}}\n.end\n"


def assert_round_trips(circuit):
    assert oxymorph.Circuit.from_json(circuit.to_json()) == circuit
    assert oxymorph.Circuit.from_yaml(circuit.to_yaml()) == circuit
    assert oxymorph.Circuit.from_proto_bytes(circuit.to_proto_bytes()) == circuit
    assert copy.deepcopy(circuit) == circuit
    assert oxymorph.parse_spice(oxymorph.write_spice(circuit)) == circuit


def test_expression_at_the_bound_round_trips():
    depth = oxymorph.MAX_EXPRESSION_DEPTH
    assert oxymorph.MAX_NESTING >= depth
    at_bound = oxymorph.parse_spice(deck_with(nested_calls(depth)))
    value = at_bound.modules[0].module_references[0].parameters[0].default_value
    assert value.expression.call is not None
    assert_round_trips(at_bound)
    # where the IR nests a value deepest: a model reference argument
    holder = oxymorph.ParameterValue(model_ref=oxymorph.ModelReference(model_interface_name="m"))
    holder.model_ref.arguments["w"] = copy.deepcopy(value)
    at_bound.modules[0].module_references[0].parameters.append(oxymorph.parameter("m", default_value=holder))
    assert oxymorph.Circuit.from_json(at_bound.to_json()) == at_bound
    assert oxymorph.Circuit.from_proto_bytes(at_bound.to_proto_bytes()) == at_bound
    assert copy.deepcopy(at_bound) == at_bound
    # one over the bound, and the review's 70-term sum, stay raw text and round-trip too
    for text in (nested_calls(depth + 1), "+".join(["a"] * 70)):
        over = oxymorph.parse_spice(deck_with(text))
        value = over.modules[0].module_references[0].parameters[0].default_value
        assert value.expression.raw == f"{{{text}}}"
        assert_round_trips(over)


def test_oneof_is_checked_on_write():
    circuit = build_two_terminal_circuit()
    both = oxymorph.ParameterValue(
        prefixed_value=oxymorph.PrefixedValue(double_value=1.0), expression=oxymorph.Expression(reference="k")
    )
    circuit.modules[0].module_references[0].parameters.append(oxymorph.parameter("r", default_value=both))
    path = r"^modules\[0\]\.module_references\[0\]\.parameters\[1\]\.default_value: ParameterValue is a oneof"
    for convert in (oxymorph.write_spice, oxymorph.Circuit.to_json, copy.deepcopy):
        with raises(ValueError, match=path):
            convert(circuit)
    circuit = build_two_terminal_circuit()
    node = oxymorph.Expression(reference="a", raw="a")
    value = oxymorph.ParameterValue(expression=oxymorph.Expression(unary=oxymorph.UnaryOp(op=oxymorph.Operator.NEG, operand=node)))
    circuit.modules[0].module_references[0].parameters.append(oxymorph.parameter("w", default_value=value))
    with raises(ValueError, match=r"default_value\.expression\.unary\.operand: Expression is a oneof"):
        oxymorph.write_spice(circuit)


def test_pickle_round_trip():
    circuit = build_two_terminal_circuit()
    restored = pickle.loads(pickle.dumps(circuit))
    assert type(restored) is oxymorph.Circuit and restored == circuit
    assert restored.to_json() == circuit.to_json()
    port = oxymorph.port("o1", domain=oxymorph.SignalDomain.WAVEGUIDE)
    assert pickle.loads(pickle.dumps(port)) == port
    assert pickle.loads(pickle.dumps(oxymorph.SIPrefix.NANO)) is oxymorph.SIPrefix.NANO
    assert copy.copy(circuit) == circuit and copy.copy(circuit) is not circuit


def test_nested_scalar_mutation_persists():
    width = oxymorph.parameter(
        "w", default_value=oxymorph.ParameterValue(prefixed_value=oxymorph.PrefixedValue(double_value=1.0))
    )
    width.default_value.prefixed_value.double_value = 2
    width.default_value.prefixed_value.prefix = oxymorph.SIPrefix.MICRO
    assert width.default_value.prefixed_value.double_value == 2.0
    assert width.default_value.prefixed_value.prefix is oxymorph.SIPrefix.MICRO
    circuit = build_two_terminal_circuit()
    reference = circuit.modules[0].module_references[0]
    assert reference.parameters[0].default_value is reference.parameters[0].default_value
    reference.parameters[0].default_value.prefixed_value.double_value = 7
    reference.properties["net"] = "gnd"
    assert "R0 vdd_34_0_0 vdd_34_0_1 7" in oxymorph.write_spice(circuit)
    assert json.loads(circuit.to_json())["modules"][0]["module_references"][0]["properties"] == {"net": "gnd"}
    assert circuit.modules[0].connections is circuit.modules[0].connections
    assert circuit.properties is circuit.properties


def test_list_attribute_reassignment_is_written():
    circuit = build_two_terminal_circuit()
    top = circuit.modules[0]
    top.connections = (
        oxymorph.connection(name, (instance, port_name), (instance, port_name))
        for name, instance, port_name in [("x", "R0", "p"), ("y", "R0", "n")]
    )
    assert isinstance(top.connections, list)
    assert "R0 x y 2" in oxymorph.write_spice(circuit)
    replacement = [
        oxymorph.connection("u", ("R0", "p"), ("R0", "p")),
        oxymorph.connection("v", ("R0", "n"), ("R0", "n")),
    ]
    top.connections = replacement
    replacement.clear()
    assert "R0 u v 2" in oxymorph.write_spice(circuit)
    circuit.ext_modules[0].properties = {"spice_prefix": "R", "extra": "1"}.items()
    assert circuit.ext_modules[0].properties == {"spice_prefix": "R", "extra": "1"}
    top.module_references[0].properties.update(net="gnd")
    assert json.loads(circuit.to_json())["modules"][0]["module_references"][0]["properties"] == {"net": "gnd"}
    with raises(TypeError):
        top.connections = 3


def test_every_ir_field_round_trips():
    circuit = oxymorph.parse_spice(RICH_SPICE)
    assert circuit.simulation.stimuli and circuit.directives and circuit.simulation.analyses
    assert any(
        parameter.default_value is not None and parameter.default_value.expression is not None
        for module in circuit.modules
        for reference in module.module_references
        for parameter in reference.parameters
    )
    data = json.loads(circuit.to_json())
    assert data["simulation"]["stimuli"] and data["directives"]
    assert oxymorph.Circuit.from_json(circuit.to_json()).to_json() == circuit.to_json()
    circuit.macros.append(oxymorph.Macro(name="m", language="spice", body=".param r=1", properties={"k": "v"}))
    circuit.modules[0].macros.append(oxymorph.Macro(name="n", language="spectre", body="x"))
    assert oxymorph.Circuit.from_json(circuit.to_json()) == circuit
    assert json.loads(circuit.to_json())["macros"][0]["body"] == ".param r=1"
    for name in oxymorph.__all__:
        assert hasattr(oxymorph, name), name


def test_unknown_ir_field_is_rejected():
    with raises(TypeError, match="colour"):
        oxymorph.Circuit(name="x", colour="red")
    with raises(AttributeError):
        oxymorph.Circuit(name="x").colour = "red"


def test_repr_is_bounded():
    cycle = oxymorph.Expression()
    cycle.unary = oxymorph.UnaryOp(op=oxymorph.Operator.NEG, operand=cycle)
    assert repr(cycle) == "Expression(unary=UnaryOp(op=Operator.NEG, operand=Expression(...)))"
    module = oxymorph.module("m")
    module.connections.append(module)
    assert repr(module) == "Module(name='m', connections=[Module(...)])"
    chain = oxymorph.Expression(reference="leaf")
    # ponytail: well past MAX_NESTING, but freeing a #[pyclass] chain recurses natively and
    # ~2000 levels overflow Windows' 1 MB stack; an iterative Drop would lift that limit
    for _ in range(4 * oxymorph.MAX_NESTING):
        chain = oxymorph.Expression(unary=oxymorph.UnaryOp(op=oxymorph.Operator.NEG, operand=chain))
    text = repr(chain)
    assert text.startswith("Expression(unary=UnaryOp(") and "Expression(...)" in text
    assert text.count("Expression(") <= oxymorph.MAX_NESTING + 1


def test_object_model_is_native():
    for class_name in ("Circuit", "Module", "ModuleReference", "Connection", "Expression", "Waveform"):
        model_class = getattr(oxymorph, class_name)
        assert not hasattr(model_class, "__dataclass_fields__"), class_name
        assert inspect.ismethoddescriptor(model_class.__dict__.get("__deepcopy__")), class_name
        assert model_class.__module__ == "oxymorph", class_name
    assert oxymorph.oxymorph.__file__.endswith(tuple(importlib.machinery.EXTENSION_SUFFIXES))
    with raises(TypeError):
        hash(oxymorph.port("p"))
    assert repr(oxymorph.port("p", domain=1)) == "Port(name='p', domain=SignalDomain.ELECTRICAL)"


# ---------------------------------------------------------------------------
# Consumer surfaces
# ---------------------------------------------------------------------------


def test_gfpex_surface():
    reference = oxymorph.Circuit(name="load", top_module="load")
    nmos = oxymorph.ext_module("nmos", ports=[oxymorph.port(name) for name in "dgsb"], properties={"spice_prefix": "M"})
    top = oxymorph.module("load")
    top.ports.extend([oxymorph.port("IN"), oxymorph.port("OUT")])
    top.ports.append(oxymorph.port("VSS"))
    top.add(oxymorph.module_reference("M1", "nmos")).add(oxymorph.module_reference("M2", "nmos"))
    top.connect("IN", ("", "IN"), ("M1", "g"))
    top.connect("OUT", ("M1", "d"), ("M2", "g"), ("", "OUT"))
    top.connect("VSS", ("M1", "s"), ("M1", "b"), ("M2", "s"), ("M2", "b"), ("M2", "d"), ("", "VSS"))
    assert reference.add_ext(nmos).add(top) is reference

    netlist = copy.deepcopy(reference)
    top_module = next(module for module in netlist.modules if module.name == netlist.top_module)
    parasitic = build_two_terminal_circuit()
    top_module.module_references.extend(parasitic.modules[0].module_references)
    top_module.connections.extend(parasitic.modules[0].connections)
    existing = {ext_module.name for ext_module in netlist.ext_modules}
    for ext_module in parasitic.ext_modules:
        if ext_module.name not in existing:
            netlist.add_ext(ext_module)
    endpoints = {
        (port_reference.instance_name, port_reference.port_name)
        for connection in top_module.connections
        if connection.name == "OUT"
        for port_reference in (connection.source, connection.target)
        if port_reference is not None
    }
    assert ("", "OUT") in endpoints and ("M1", "d") in endpoints
    top_module.connections = [connection for connection in top_module.connections if connection.name != "OUT"]
    top_module.connect("OUT", ("M1", "d"), ("M1", "d"))
    netlist.properties.update({"backannotated": "true"})
    netlist.properties["unmatched_terminals"] = "OUT:I0/M3/d"

    extracted = [reference for reference in netlist.modules[0].module_references if reference.properties.get("net")]
    assert [reference.name for reference in extracted] == ["R0"]
    written = oxymorph.write_spice(netlist)
    assert "R0 vdd_34_0_0 vdd_34_0_1 2" in written
    assert "M1 OUT" in written
    assert len(reference.modules[0].module_references) == 2


def test_gflvs_surface():
    nano = oxymorph.SIPrefix.NANO

    def width(value, prefix=oxymorph.SIPrefix.UNSPECIFIED):
        return oxymorph.parameter(
            "width",
            default_value=oxymorph.ParameterValue(
                prefixed_value=oxymorph.PrefixedValue(double_value=value, prefix=prefix)
            ),
        )

    top = oxymorph.module("top", class_name="top")
    top.add(
        oxymorph.module_reference("r0", "resistor", class_name="resistor", parameters=[width(500, nano)]),
        oxymorph.module_reference("r1", "resistor", parameters=[("width", 0.5)], properties={"k": "v"}),
    )
    assert top.add_terminal("out", domain=oxymorph.SignalDomain.WAVEGUIDE) is top
    top.connect("vdd", ("r0", "pin1"), ("r1", "pin1"), domain=oxymorph.SignalDomain.ELECTRICAL)
    netlist = oxymorph.Circuit(name="reference")
    netlist.add(top)
    assert netlist.top_module == "top"
    netlist.add_ext(
        oxymorph.ext_module(
            "resistor",
            class_name="resistor",
            ports=[oxymorph.port("pin1", domain=oxymorph.SignalDomain.ELECTRICAL)],
            parameters=[width(0.5)],
        )
    )
    reference = netlist.modules[0].module_references[0]
    assert reference.module_name == "resistor" and reference.class_name == "resistor"
    prefixed = reference.parameters[0].default_value.prefixed_value
    assert (prefixed.double_value, prefixed.prefix) == (500.0, nano)
    assert top.ports[0].domain == oxymorph.SignalDomain.WAVEGUIDE
    assert [connection.domain for connection in top.connections] == [oxymorph.SignalDomain.ELECTRICAL]
    assert oxymorph.Circuit.from_yaml(netlist.to_yaml()) == netlist
    assert oxymorph.Circuit(name="inverter").to_json() == oxymorph.Circuit(name="inverter").to_json()
    assert oxymorph.Circuit(name="inverter") != oxymorph.Circuit(name="buffer")

    spectre = oxymorph.write_spectre(netlist)
    reparsed = oxymorph.parse_spectre(spectre)
    assert reparsed.modules and oxymorph.write_spectre(reparsed)
    assert oxymorph.parse_spice("R1 a b 1k\n.end\n").modules
    assert oxymorph.parse_spectre_spice("simulator lang=spice\nR1 a b 1k\n").modules


def test_gfpex_flow():
    """gfpex `netlist_backannotate_extracted_netlist`: a deep copy of the reference, the IN net rewired onto its pins'
    nodes, the extracted resistors merged into the top module, written as SPICE."""
    nmos = oxymorph.ext_module("nmos", ports=[oxymorph.port(name) for name in "dgsb"], properties={"spice_prefix": "M"})
    top = oxymorph.module("load")
    top.ports.extend([oxymorph.port("IN"), oxymorph.port("OUT"), oxymorph.port("VSS")])
    top.add(oxymorph.module_reference("M1", "nmos"))
    top.connect("IN", ("", "IN"), ("M1", "g"))
    top.connect("OUT", ("M1", "d"), ("", "OUT"))
    top.connect("VSS", ("M1", "s"), ("M1", "b"), ("", "VSS"))
    reference = oxymorph.Circuit(name="load", top_module="load").add_ext(nmos).add(top)
    reference_spice = oxymorph.write_spice(reference)

    annotated = copy.deepcopy(reference)
    top_module = next(module for module in annotated.modules if module.name == annotated.top_module)
    top_module.connections = [connection for connection in top_module.connections if connection.name != "IN"]
    top_module.connect("IN", ("", "IN"), ("", "IN")).connect("M1/g", ("M1", "g"), ("M1", "g"))
    resistors = oxymorph.module("__top__")
    for name, node_p, node_n, value in [("RIN_0", "IN", "IN_34_0_0", 1.0), ("RIN_1", "IN_34_0_0", "M1/g", 2.0)]:
        resistors.add(oxymorph.module_reference(name, "resistor", value=value, properties={"net": "IN"}))
        resistors.connect(node_p, (name, "p"), (name, "p")).connect(node_n, (name, "n"), (name, "n"))
    top_module.module_references.extend(resistors.module_references)
    top_module.connections.extend(resistors.connections)
    annotated.add_ext(
        oxymorph.ext_module(
            "resistor", ports=[oxymorph.port("p"), oxymorph.port("n")], properties={"spice_prefix": "R"}
        )
    )
    annotated.properties.update({"backannotated": "true"})

    assert oxymorph.write_spice(reference) == reference_spice
    spice = oxymorph.write_spice(annotated).splitlines()
    assert "M1 OUT M1/g VSS VSS nmos" in spice, spice
    assert [line for line in spice if line.startswith("R")] == ["RIN_0 IN IN_34_0_0 1", "RIN_1 IN_34_0_0 M1/g 2"]
    assert annotated != reference and json.loads(annotated.to_json())["properties"] == {"backannotated": "true"}


def test_gflvs_flow():
    """gflvs `netlist_from_layout_to_netlist`: devices as references with unprefixed parameters, nets chained with
    their domain, a lone terminal with passthrough devices made a port; then a YAML round trip and a stable hash."""
    devices = [("R0", "resistor", [("width", 0.5)]), ("R1", "resistor", [("width", 0.25)]), ("WG0", "waveguide", [])]
    nets = [
        ("vdd", oxymorph.SignalDomain.ELECTRICAL, [("R0", "pin1"), ("R1", "pin1")], False),
        ("out", oxymorph.SignalDomain.ELECTRICAL, [("R0", "pin2")], True),
        ("o1", oxymorph.SignalDomain.WAVEGUIDE, [("WG0", "o1"), ("R1", "pin2")], False),
    ]

    def build_netlist():
        top = oxymorph.module("top", class_name="top")
        top.add(
            *(
                oxymorph.module_reference(name, class_name, class_name=class_name, parameters=parameters)
                for name, class_name, parameters in devices
            )
        )
        for net_name, domain, endpoints, has_passthrough in nets:
            endpoints = list(endpoints)
            if len(endpoints) == 1 and has_passthrough:
                top.add_terminal(net_name, domain=domain)
                endpoints.append(("", net_name))
            if len(endpoints) > 1:
                top.connect(net_name, *endpoints, domain=domain)
        netlist = oxymorph.Circuit(name="top")
        netlist.add(top)
        netlist.add_ext(
            oxymorph.ext_module(
                "resistor", class_name="resistor", ports=[oxymorph.port("pin1"), oxymorph.port("pin2")]
            ),
            oxymorph.ext_module("waveguide", ports=[oxymorph.port("o1", domain=oxymorph.SignalDomain.WAVEGUIDE)]),
        )
        return netlist

    netlist = build_netlist()
    top = netlist.modules[0]
    assert netlist.top_module == "top"
    assert [(port.name, port.domain) for port in top.ports] == [("out", oxymorph.SignalDomain.ELECTRICAL)]
    assert [connection.domain for connection in top.connections] == [1, 1, 2]
    assert isinstance(top.connections[2].domain, oxymorph.SignalDomain)
    width = top.module_references[0].parameters[0].default_value.prefixed_value
    assert (width.double_value, width.prefix) == (0.5, oxymorph.SIPrefix.UNSPECIFIED)
    width.prefix = oxymorph.SIPrefix.MICRO
    assert netlist.modules[0].module_references[0].parameters[0].default_value.prefixed_value.prefix == 9

    assert oxymorph.Circuit.from_yaml(netlist.to_yaml()) == netlist
    assert (
        hashlib.sha256(netlist.to_json().encode()).hexdigest()
        == hashlib.sha256(oxymorph.Circuit.from_yaml(netlist.to_yaml()).to_json().encode()).hexdigest()
    )
    rebuilt = build_netlist()
    rebuilt.modules[0].module_references[0].parameters[0].default_value.prefixed_value.prefix = oxymorph.SIPrefix.MICRO
    assert rebuilt.to_json() == netlist.to_json()
    rebuilt.modules[0].connections[0].domain = oxymorph.SignalDomain.UNSPECIFIED
    assert rebuilt.to_json() != netlist.to_json()


# ---------------------------------------------------------------------------
# Parsers, writers, expressions, paths
# ---------------------------------------------------------------------------


def test_every_writer_writes():
    circuit = oxymorph.parse_spice(RICH_SPICE)
    for name in ("spice", "hspice", "xyce", "spectre", "spectre_spice", "vams", "systemverilog"):
        assert getattr(oxymorph, f"write_{name}")(circuit), name
    assert oxymorph.parse_hspice("R1 a b 1k\n.end\n").modules
    assert oxymorph.parse_xyce("R1 a b 1k\n.end\n").modules


def test_hdl_parsers_take_keywords():
    text = (KF_DATA / "systemverilog" / "switch_2x2.sv").read_text()
    default = oxymorph.parse_systemverilog(text)
    simulated = oxymorph.parse_systemverilog(
        text, defines={"IS_SIM": "1"}, disciplines={"connection": oxymorph.SignalDomain.WAVEGUIDE}
    )
    port_domains = {port.name: port.domain for port in simulated.modules[-1].ports}
    assert port_domains["in"] == oxymorph.SignalDomain.WAVEGUIDE
    assert default != simulated
    assert oxymorph.write_systemverilog(simulated)
    pinned = oxymorph.parse_systemverilog(text, pin_domains={"MZM_1X2": {"in0": oxymorph.SignalDomain.WAVEGUIDE}})
    assert pinned.modules
    vams = (KF_DATA / "vams" / "switch_2x2.vams").read_text()
    assert oxymorph.parse_vams(vams, defines={}, disciplines={"optical": oxymorph.SignalDomain.WAVEGUIDE}).modules
    assert oxymorph.parse_vams(vams).modules


def test_gsch_parse_read_write():
    gsch_dir = KF_DATA / "gsch"
    models = json.loads((gsch_dir / "models.nyanlib").read_text())
    text = (gsch_dir / "ihp_rc_filter.gsch").read_text()
    circuit = oxymorph.parse_gsch(text, models=models, sim="ngspice", corners="mos_tt", name="rc")
    assert circuit.modules[0].name == "rc"
    assert oxymorph.write_spice(circuit)
    assert json.loads(oxymorph.write_gsch(circuit))
    read = oxymorph.read_gsch(gsch_dir / "ihp_hier_top.gsch", project_root=gsch_dir, models=models, corners=["mos_tt"])
    assert len(read.modules) >= 1
    with raises(ValueError):
        oxymorph.parse_gsch(text, sim="no-such-simulator")


def test_expressions():
    expression = oxymorph.parse_expression("{a*2}")
    assert isinstance(expression, oxymorph.Expression)
    assert expression.binary.op == oxymorph.Operator.MUL
    assert expression.binary.lhs.reference == "a"
    assert oxymorph.format_expression(expression, "ngspice") == "{a * 2}"
    assert oxymorph.format_expression(expression, "hspice") == "'a * 2'"
    assert oxymorph.format_expression(expression, dialect="spectre") == "a * 2"
    assert oxymorph.parse_expression(oxymorph.format_expression(expression, "xyce")) == expression
    raw = oxymorph.Expression(raw="foo bar")
    assert oxymorph.format_expression(raw, "ngspice", named=True) == "{foo bar}"
    assert oxymorph.format_expression(raw, "ngspice") == "foo bar"
    with raises(ValueError):
        oxymorph.format_expression(expression, "no-such-dialect")


def test_paths():
    assert oxymorph.posix_file("a\\b\\..\\c.txt") == "a/c.txt"
    assert oxymorph.is_absolute("C:\\models\\x.pwl") and oxymorph.is_absolute("\\\\srv\\share\\x")
    assert not oxymorph.is_absolute("data/w.txt")
    assert oxymorph.resolve_file("w.txt", "decks") == "decks/w.txt"
    assert oxymorph.resolve_file(Path("w.txt"), Path("decks")) == "decks/w.txt"
    assert oxymorph.resolve_file("/abs/w.txt", "decks") == "/abs/w.txt"
    assert oxymorph.resolve_file("w.txt") == "w.txt"


def test_parse_errors_are_value_errors():
    with raises(ValueError):
        oxymorph.parse_vams("module (;")


class Tests(unittest.TestCase):
    """Collects the `test_*` functions above for `python -m unittest`."""


for _name in [name for name in globals() if name.startswith("test_")]:
    setattr(Tests, _name, staticmethod(globals()[_name]))


if __name__ == "__main__":
    unittest.main()
