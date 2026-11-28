//! Writer dialects ported from kfnetlist 5924917, 9766e84, 1adb071, 5671e52 and d94dc38.
//! from: kfnetlist tests/morphnet/test_spice_writer.py, test_spectre_writer.py,
//! test_xyce_writer.py and test_spectre_parser.py (TestIncludeSection)

use oxymorph_netlist::expression::parse_expression;
use oxymorph_netlist::{
    parse_spectre, parse_xyce, write_hspice, write_spectre, write_spice, write_vams, write_xyce,
};
use oxymorph_schema::{
    Circuit, Connection, Directive, DirectiveKind, ExternalModule, Module, ModuleReference,
    Parameter, ParameterValue, Port, PortDirection, PortReference, Properties, SignalDomain,
    Siprefix,
};

fn make_port(name: &str) -> Port {
    Port {
        name: name.to_string(),
        direction: PortDirection::Inout,
        domain: SignalDomain::Electrical,
        ..Default::default()
    }
}

fn make_pv(value: f64, prefix: Siprefix) -> ParameterValue {
    ParameterValue::prefixed(value, prefix)
}

fn expr(text: &str) -> ParameterValue {
    ParameterValue {
        expression: Some(parse_expression(text).unwrap()),
        ..Default::default()
    }
}

fn parameter(name: &str, value: ParameterValue) -> Parameter {
    Parameter {
        name: name.to_string(),
        default_value: Some(value),
        ..Default::default()
    }
}

fn make_conn(
    net: &str,
    source_instance: &str,
    source_port: &str,
    target_instance: &str,
    target_port: &str,
) -> Connection {
    Connection {
        name: net.to_string(),
        source: Some(PortReference {
            instance_name: source_instance.to_string(),
            port_name: source_port.to_string(),
        }),
        target: Some(PortReference {
            instance_name: target_instance.to_string(),
            port_name: target_port.to_string(),
        }),
        ..Default::default()
    }
}

fn properties(pairs: &[(&str, &str)]) -> Properties {
    let mut properties = Properties::new();
    for (key, value) in pairs {
        properties.insert(key.to_string(), value.to_string());
    }
    properties
}

fn reference(name: &str, module_name: &str, parameters: Vec<Parameter>) -> ModuleReference {
    ModuleReference {
        name: name.to_string(),
        module_name: module_name.to_string(),
        parameters,
        ..Default::default()
    }
}

fn module(name: &str, ports: &[&str]) -> Module {
    Module {
        name: name.to_string(),
        ports: ports.iter().map(|port| make_port(port)).collect(),
        ..Default::default()
    }
}

fn circuit(modules: Vec<Module>) -> Circuit {
    Circuit {
        name: "t".to_string(),
        modules,
        ..Default::default()
    }
}

/// The `_prim_netlist` helper of the kfnetlist writer tests: module `test` with one
/// primitive instance between nets `a` and `b`.
struct Primitive<'a> {
    instance: &'a str,
    parameters: Vec<Parameter>,
    nets: (&'a str, &'a str),
    netlist_properties: &'a [(&'a str, &'a str)],
    instance_properties: &'a [(&'a str, &'a str)],
    module_name: &'a str,
}

impl Default for Primitive<'_> {
    fn default() -> Self {
        Self {
            instance: "R1",
            parameters: Vec::new(),
            nets: ("a", "b"),
            netlist_properties: &[],
            instance_properties: &[],
            module_name: "resistor",
        }
    }
}

impl Primitive<'_> {
    fn circuit(self) -> Circuit {
        let resistor = ExternalModule {
            name: self.module_name.to_string(),
            ports: vec![make_port("p"), make_port("n")],
            properties: properties(&[("spice_prefix", "R")]),
            ..Default::default()
        };
        let mut instance = reference(self.instance, self.module_name, self.parameters);
        instance.properties = properties(self.instance_properties);
        let test = Module {
            module_references: vec![instance],
            connections: vec![
                make_conn(self.nets.0, "", "a", self.instance, "p"),
                make_conn(self.nets.1, "", "b", self.instance, "n"),
            ],
            ..module("test", &["a", "b"])
        };
        Circuit {
            top_module: "test".to_string(),
            ext_modules: vec![resistor],
            properties: properties(self.netlist_properties),
            ..circuit(vec![test])
        }
    }
}

// ---------------------------------------------------------------------------
// test_spice_writer.py
// ---------------------------------------------------------------------------

#[test]
fn spice_expr_is_positional_value_and_kv() {
    let netlist = Primitive {
        parameters: vec![
            parameter("value", expr("DC 1 AC 1")), // raw
            parameter("w", expr("{w*1e-6}")),
        ],
        ..Default::default()
    }
    .circuit();
    assert!(write_spice(&netlist).contains("R1 a b DC 1 AC 1 w={w * 1e-6}"));
}

fn inner_and_outer() -> Circuit {
    let inner = Module {
        parameters: vec![parameter("k", expr("2*x"))],
        ..module("inner", &["p"])
    };
    let outer = Module {
        module_references: vec![reference(
            "X1",
            "inner",
            vec![parameter("k", expr("{3*y}"))],
        )],
        connections: vec![make_conn("a", "", "a", "X1", "p")],
        ..module("outer", &["a"])
    };
    circuit(vec![inner, outer])
}

#[test]
fn spice_expr_on_header_and_call_params() {
    let out = write_spice(&inner_and_outer());
    assert!(out.contains(".subckt inner p k={2 * x}"), "{out}");
    assert!(out.contains("X1 a inner k={3 * y}"), "{out}");
}

#[test]
fn spice_each_dialect_wraps_its_own_way() {
    let netlist = Primitive {
        parameters: vec![parameter("w", expr("w*2"))],
        ..Default::default()
    }
    .circuit();
    assert!(write_spice(&netlist).contains("w={w * 2}"));
    assert!(write_xyce(&netlist).contains("w={w * 2}"));
    assert!(write_hspice(&netlist).contains("w='w * 2'"));
}

#[test]
fn spice_a_bare_reference_or_literal_is_not_wrapped() {
    let netlist = Primitive {
        parameters: vec![parameter("w", expr("wmin")), parameter("l", expr("2.2u"))],
        ..Default::default()
    }
    .circuit();
    assert!(write_spice(&netlist).contains("w=wmin l=2.2u"));
}

#[test]
fn spice_prefix_prepended_when_missing() {
    let netlist = Primitive {
        instance: "load",
        ..Default::default()
    }
    .circuit();
    assert!(write_spice(&netlist).contains("\nRload a b"));
}

#[test]
fn spice_prefix_not_duplicated_case_insensitive() {
    let netlist = Primitive {
        instance: "r_load",
        ..Default::default()
    }
    .circuit();
    let out = write_spice(&netlist);
    assert!(out.contains("\nr_load a b"));
    assert!(!out.contains("Rr_load"));
}

#[test]
fn spice_defined_circuit_gets_x() {
    let inner = module("inner", &["p"]);
    let outer = Module {
        module_references: vec![reference("amp", "inner", vec![])],
        connections: vec![make_conn("a", "", "a", "amp", "p")],
        ..module("outer", &["a"])
    };
    assert!(write_spice(&circuit(vec![inner, outer])).contains("\nXamp a inner"));
}

#[test]
fn spice_ground_is_node_zero_header_unchanged() {
    let block = Module {
        module_references: vec![reference(
            "R1",
            "resistor",
            vec![parameter("value", expr("1k"))],
        )],
        connections: vec![
            make_conn("GND", "", "GND", "R1", "p"),
            make_conn("b", "", "b", "R1", "n"),
        ],
        ..module("blk", &["GND", "b"])
    };
    let resistor = ExternalModule {
        name: "resistor".to_string(),
        ports: vec![make_port("p"), make_port("n")],
        properties: properties(&[("spice_prefix", "R")]),
        ..Default::default()
    };
    let netlist = Circuit {
        ext_modules: vec![resistor],
        properties: properties(&[("ground_net", "GND")]),
        ..circuit(vec![block])
    };
    let out = write_spice(&netlist);
    assert!(out.contains(".subckt blk GND b"));
    assert!(out.contains("R1 0 b 1k"));
}

#[test]
fn spice_ground_at_top() {
    let mut netlist = Primitive {
        nets: ("GND", "b"),
        netlist_properties: &[("ground_net", "GND")],
        ..Default::default()
    }
    .circuit();
    netlist.modules[0].name = "__top__".to_string();
    assert!(write_spice(&netlist).contains("R1 0 b"));
}

#[test]
fn spice_params_on_header_not_body() {
    let inner = Module {
        parameters: vec![parameter("r", make_pv(1.0, Siprefix::Kilo))],
        ..module("inner", &["p", "q"])
    };
    let out = write_spice(&circuit(vec![inner]));
    assert!(out.contains(".subckt inner p q r=1k"));
    assert!(!out.contains(".param"));
}

#[test]
fn spice_sim_skip_instance_omitted() {
    let netlist = Primitive {
        instance_properties: &[("sim_skip", "1")],
        ..Default::default()
    }
    .circuit();
    assert!(!write_spice(&netlist).contains("R1"));
}

#[test]
fn spice_lib_path_section() {
    let netlist = Circuit {
        directives: vec![Directive {
            kind: DirectiveKind::Lib,
            value: "m.lib tt".to_string(),
            ..Default::default()
        }],
        ..circuit(vec![])
    };
    assert!(write_spice(&netlist).contains(".lib m.lib tt"));
}

#[test]
fn spice_an_empty_expression_is_not_written() {
    let netlist = Primitive {
        parameters: vec![
            parameter("w", expr("")),
            Parameter {
                name: "n".to_string(),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
    .circuit();
    // no `w=`
    assert!(write_spice(&netlist).contains("R1 a b\n"));
}

// ---------------------------------------------------------------------------
// test_spectre_writer.py (test_vacask_subckt_params_are_a_parameters_line comes from
// the .gsch commit 78c0483, Task 7)
// ---------------------------------------------------------------------------

#[test]
fn spectre_expr_verbatim_not_dropped() {
    let netlist = Primitive {
        parameters: vec![
            parameter("r", expr("{w*1e-6}")),
            parameter("dc", expr("1.8")),
        ],
        ..Default::default()
    }
    .circuit();
    assert!(write_spectre(&netlist).contains("R1 (a b) resistor r=w * 1e-6 dc=1.8"));
}

#[test]
fn spectre_expr_on_subckt_header() {
    let inner = Module {
        parameters: vec![parameter("k", expr("2*x"))],
        ..module("inner", &["p"])
    };
    assert!(write_spectre(&circuit(vec![inner])).contains("subckt inner (p) k=2 * x"));
}

#[test]
fn spectre_ground_net_is_node_zero() {
    let netlist = Primitive {
        nets: ("GND", "b"),
        netlist_properties: &[("ground_net", "GND")],
        ..Default::default()
    }
    .circuit();
    let out = write_spectre(&netlist);
    assert!(out.contains("R1 (0 b) resistor"));
    assert!(out.contains("subckt test (a b)"));
}

#[test]
fn spectre_ground_statement_only_for_vacask() {
    let netlist = Primitive {
        netlist_properties: &[("ground_net", "GND"), ("dialect", "vacask")],
        ..Default::default()
    }
    .circuit();
    let out = write_spectre(&netlist);
    assert!(out.lines().take(3).any(|line| line == "ground 0"));
    let netlist = Primitive {
        netlist_properties: &[("ground_net", "GND")],
        ..Default::default()
    }
    .circuit();
    assert!(!write_spectre(&netlist).contains("ground 0"));
}

#[test]
fn spectre_lowercase_instance_names() {
    let netlist = Primitive {
        instance: "R_Load",
        netlist_properties: &[("dialect", "vacask")],
        ..Default::default()
    }
    .circuit();
    assert!(write_spectre(&netlist).contains("\nr_load (a b)"));
    let netlist = Primitive {
        instance: "R_Load",
        ..Default::default()
    }
    .circuit();
    assert!(write_spectre(&netlist).contains("\nR_Load (a b)"));
}

#[test]
fn spectre_value_maps_to_device_param() {
    let netlist = Primitive {
        parameters: vec![parameter("value", expr("1k")), parameter("tc1", expr("0"))],
        netlist_properties: &[("dialect", "vacask")],
        ..Default::default()
    }
    .circuit();
    assert!(write_spectre(&netlist).contains("r1 (a b) resistor r=1k tc1=0"));
}

#[test]
fn spectre_value_kept_without_vacask() {
    let netlist = Primitive {
        parameters: vec![parameter("value", expr("1k"))],
        ..Default::default()
    }
    .circuit();
    assert!(write_spectre(&netlist).contains("R1 (a b) resistor value=1k"));
}

#[test]
fn spectre_source_params_pass_through() {
    let netlist = Primitive {
        parameters: vec![parameter("dc", expr("1")), parameter("mag", expr("1"))],
        module_name: "vsource",
        netlist_properties: &[("dialect", "vacask")],
        ..Default::default()
    }
    .circuit();
    assert!(write_spectre(&netlist).contains("r1 (a b) vsource dc=1 mag=1"));
}

#[test]
fn spectre_sim_skip_instance_omitted() {
    let netlist = Primitive {
        instance_properties: &[("sim_skip", "1")],
        ..Default::default()
    }
    .circuit();
    assert!(!write_spectre(&netlist).contains("R1"));
}

fn with_directive(kind: DirectiveKind, value: &str, name: &str) -> Circuit {
    Circuit {
        directives: vec![Directive {
            kind,
            name: name.to_string(),
            value: value.to_string(),
        }],
        ..circuit(vec![])
    }
}

#[test]
fn spectre_lib_with_section() {
    let out = write_spectre(&with_directive(DirectiveKind::Lib, "m.scs tt", ""));
    assert!(out
        .lines()
        .any(|line| line == "include \"m.scs\" section=tt"));
}

#[test]
fn spectre_lib_with_lang() {
    let out = write_spectre(&with_directive(
        DirectiveKind::Lib,
        "m.lib tt",
        "lang=ngspice",
    ));
    assert!(out.contains("include \"m.lib\" section=tt lang=ngspice"));
}

#[test]
fn spectre_include_with_lang() {
    let out = write_spectre(&with_directive(
        DirectiveKind::Include,
        "m.lib",
        "lang=ngspice",
    ));
    assert!(out
        .lines()
        .any(|line| line == "include \"m.lib\" lang=ngspice"));
}

// ---------------------------------------------------------------------------
// test_xyce_writer.py
// ---------------------------------------------------------------------------

fn sub_call_netlist() -> Circuit {
    let inner = Module {
        parameters: vec![parameter("w", make_pv(1.0, Siprefix::Unspecified))],
        ..module("sub", &["p", "q"])
    };
    let plain = module("plain", &["p"]);
    let outer = Module {
        module_references: vec![
            reference(
                "X1",
                "sub",
                vec![parameter("w", make_pv(2.0, Siprefix::Unspecified))],
            ),
            reference("X2", "plain", vec![]),
        ],
        connections: vec![
            make_conn("a", "", "a", "X1", "p"),
            make_conn("b", "", "b", "X1", "q"),
            make_conn("a", "", "a", "X2", "p"),
        ],
        ..module("outer", &["a", "b"])
    };
    Circuit {
        top_module: "outer".to_string(),
        ..circuit(vec![inner, plain, outer])
    }
}

fn module_named<'a>(circuit: &'a Circuit, name: &str) -> &'a Module {
    circuit
        .modules
        .iter()
        .find(|module| module.name == name)
        .unwrap_or_else(|| panic!("no module {name}"))
}

fn names_of_parameters(parameters: &[Parameter]) -> Vec<&str> {
    parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect()
}

#[test]
fn xyce_subckt_header_params() {
    let out = write_xyce(&sub_call_netlist());
    assert!(out.contains(".subckt sub p q PARAMS: w=1"), "{out}");
    assert!(out.contains(".subckt plain p\n"), "{out}");
}

#[test]
fn xyce_call_params() {
    let out = write_xyce(&sub_call_netlist());
    assert!(out.contains("X1 a b sub PARAMS: w=2"), "{out}");
    assert!(out.contains("X2 a plain\n"), "{out}");
}

#[test]
fn xyce_parse_accepts_params_keyword() {
    let netlist = parse_xyce(&write_xyce(&sub_call_netlist())).unwrap();
    let outer = module_named(&netlist, "outer");
    assert_eq!(outer.module_references[0].module_name, "sub");
    assert_eq!(
        names_of_parameters(&outer.module_references[0].parameters),
        ["w"]
    );
    let sub = module_named(&netlist, "sub");
    let port_names: Vec<&str> = sub.ports.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(port_names, ["p", "q"]);
    assert_eq!(names_of_parameters(&sub.parameters), ["w"]);
}

#[test]
fn xyce_parse_x_call_with_params_keyword() {
    let netlist = parse_xyce(
        ".subckt sub p q PARAMS: w=1\n.ends sub\n\
         .subckt top a b\nX1 a b sub PARAMS: w=2\n.ends top\n",
    )
    .unwrap();
    let x1 = &netlist.modules[1].module_references[0];
    assert_eq!(x1.module_name, "sub");
    assert_eq!(names_of_parameters(&x1.parameters), ["w"]);
}

#[test]
fn xyce_round_trip_stable() {
    let first = write_xyce(&parse_xyce(&write_xyce(&sub_call_netlist())).unwrap());
    assert_eq!(write_xyce(&parse_xyce(&first).unwrap()), first);
    assert!(first.contains("PARAMS:"));
}

#[test]
fn xyce_spice_and_hspice_keep_no_keyword() {
    for out in [
        write_spice(&sub_call_netlist()),
        write_hspice(&sub_call_netlist()),
    ] {
        assert!(!out.contains("PARAMS:"), "{out}");
        assert!(out.contains(".subckt sub p q w=1"), "{out}");
    }
}

// ---------------------------------------------------------------------------
// test_spectre_parser.py TestIncludeSection
// ---------------------------------------------------------------------------

fn only_directive(text: &str) -> (DirectiveKind, String, String) {
    let netlist = parse_spectre(text).unwrap();
    let [directive] = netlist.directives.as_slice() else {
        panic!("one directive in {text:?}");
    };
    (
        directive.kind,
        directive.value.clone(),
        directive.name.clone(),
    )
}

fn directive_fields(
    kind: DirectiveKind,
    value: &str,
    name: &str,
) -> (DirectiveKind, String, String) {
    (kind, value.to_string(), name.to_string())
}

#[test]
fn spectre_include_section_is_lib_directive() {
    assert_eq!(
        only_directive("include \"m.scs\" section=tt\n"),
        directive_fields(DirectiveKind::Lib, "m.scs tt", "")
    );
}

#[test]
fn spectre_include_section_lang() {
    assert_eq!(
        only_directive("include \"m.lib\" section=tt lang=ngspice\n"),
        directive_fields(DirectiveKind::Lib, "m.lib tt", "lang=ngspice")
    );
}

#[test]
fn spectre_include_lang_only() {
    assert_eq!(
        only_directive("include \"m.lib\" lang=ngspice\n"),
        directive_fields(DirectiveKind::Include, "m.lib", "lang=ngspice")
    );
}

#[test]
fn spectre_plain_include_unchanged() {
    assert_eq!(
        only_directive("include \"m.scs\"\n"),
        directive_fields(DirectiveKind::Include, "m.scs", "")
    );
}

#[test]
fn spectre_include_round_trip() {
    for line in [
        "include \"m.scs\" section=tt",
        "include \"m.lib\" section=tt lang=ngspice",
        "include \"m.lib\" lang=ngspice",
        "include \"m.scs\"",
    ] {
        let out = write_spectre(&parse_spectre(&format!("{line}\n")).unwrap());
        assert!(
            out.lines().any(|written| written == line),
            "{line} not in {out}"
        );
    }
}

// ---------------------------------------------------------------------------
// d94dc38 (no kfnetlist test outside .gsch): role=source params
// ---------------------------------------------------------------------------

#[test]
fn source_params_are_skipped_and_resolved() {
    let mut width = parameter("width", make_pv(2.0, Siprefix::Unspecified));
    width.properties = properties(&[("role", "source")]);
    let netlist = Primitive {
        parameters: vec![width, parameter("w", expr("width*1e-6"))],
        ..Default::default()
    }
    .circuit();
    let spice = write_spice(&netlist);
    assert!(spice.contains("R1 a b w={2 * 1e-6}\n"), "{spice}");
    assert!(!spice.contains("width"), "{spice}");
    let spectre = write_spectre(&netlist);
    assert!(
        spectre.contains("R1 (a b) resistor w=2 * 1e-6\n"),
        "{spectre}"
    );
    let vams = write_vams(&netlist);
    assert!(vams.contains("resistor #(.w(2 * 1e-6)) R1"), "{vams}");
}

// ── VACASK subckt params (78c0483) ─────────────────────────────────────────────

#[test]
fn vacask_subckt_params_are_a_parameters_line() {
    let inner = Module {
        parameters: vec![parameter("k", expr("2*x"))],
        ..module("inner", &["p"])
    };
    let mut netlist = circuit(vec![inner]);
    netlist.properties = properties(&[("dialect", "vacask")]);
    let text = write_spectre(&netlist);
    let lines: Vec<&str> = text.lines().collect();
    let index = lines
        .iter()
        .position(|line| *line == "subckt inner (p)")
        .unwrap();
    assert_eq!(lines[index + 1], "parameters k=2 * x");
}

// ── macros (e82518b), from test_gsch_export.py ─────────────────────────────────

fn macro_netlist(macros: &[(&str, &str)], dialect: &str) -> Circuit {
    let mut circuit_macros = Vec::new();
    for (name, language) in macros {
        circuit_macros.push(oxymorph_schema::Macro {
            name: name.to_string(),
            language: language.to_string(),
            body: format!("{name} body"),
            ..Default::default()
        });
    }
    let cell = Module {
        macros: vec![oxymorph_schema::Macro {
            name: "local".to_string(),
            language: "spice".to_string(),
            body: "R1 a b 1k".to_string(),
            ..Default::default()
        }],
        ..module("cell", &[])
    };
    let mut netlist = circuit(vec![cell, module("__top__", &[])]);
    netlist.top_module = "__top__".to_string();
    netlist.macros = circuit_macros;
    if !dialect.is_empty() {
        netlist.properties = properties(&[("dialect", dialect)]);
    }
    netlist
}

#[test]
fn spectre_wraps_spice_macros_in_simulator_lang() {
    let netlist = macro_netlist(&[("sp", "ngspice"), ("sc", "spectre")], "");
    let text = write_spectre(&netlist);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..4],
        [
            "simulator lang=spice",
            "sp body",
            "simulator lang=spectre",
            "sc body"
        ]
    );
    // a circuit's own macros go inside its subckt
    assert_eq!(
        lines[4..8],
        [
            "subckt cell ()",
            "simulator lang=spice",
            "R1 a b 1k",
            "simulator lang=spectre"
        ]
    );
    let spice = write_spice(&macro_netlist(&[("sp", "xyce")], ""));
    assert_eq!(spice.lines().nth(1), Some("sp body"));
}

#[test]
fn a_macro_in_another_language_is_a_comment() {
    // kfnetlist also warns ("skipped macro va (verilog-a-ish)"); oxymorph has no warnings
    type Write = fn(&Circuit) -> String;
    let writers: [(Write, &str, &str); 4] = [
        (write_spice, "", "*"),
        (write_hspice, "", "*"),
        (write_spectre, "vacask", "//"),
        (oxymorph_netlist::write_systemverilog, "", "//"),
    ];
    for (write_format, dialect, comment) in writers {
        let text = write_format(&macro_netlist(&[("va", "verilog-a-ish")], dialect));
        let note = format!("{comment} skipped macro va (verilog-a-ish)");
        assert!(text.lines().any(|line| line == note), "{text}");
        assert!(!text.contains("va body"), "{text}");
    }
}

#[test]
fn verilog_a_carries_foreign_code_in_an_attribute() {
    let text = write_vams(&macro_netlist(&[("va", "verilog-a-ish")], ""));
    assert!(text.contains("(* spice_code = "), "{text}"); // nothing is lost
    let back = oxymorph_netlist::parse_vams(&text).unwrap();
    assert_eq!(back.macros.len(), 1);
    assert_eq!(
        (
            back.macros[0].name.as_str(),
            back.macros[0].language.as_str()
        ),
        ("va", "verilog-a-ish")
    );
}

#[test]
fn hdl_writers_write_verilog_macros_as_body_text() {
    let vams = write_vams(&macro_netlist(&[("va", "verilog-a")], ""));
    assert!(vams.lines().any(|line| line == "va body"), "{vams}");
    assert!(!vams.contains("spice_code"), "{vams}");
    let netlist = macro_netlist(&[("sv", "systemverilog")], "");
    let systemverilog = oxymorph_netlist::write_systemverilog(&netlist);
    assert!(
        systemverilog.lines().any(|line| line == "sv body"),
        "{systemverilog}"
    );
    // a module's own spice macro: a comment in SystemVerilog, an attribute in Verilog-A
    assert!(
        systemverilog.contains("// skipped macro local (spice)"),
        "{systemverilog}"
    );
}

#[test]
fn skipped_macro_metadata_cannot_inject_lines() {
    let mut netlist = macro_netlist(&[], "");
    netlist.macros = vec![oxymorph_schema::Macro {
        name: "x\n.control\nshell rm\n.endc".to_string(),
        language: "lua\r\n.end".to_string(),
        body: "never written".to_string(),
        ..Default::default()
    }];
    for text in [
        write_spice(&netlist),
        write_spectre(&netlist),
        oxymorph_netlist::write_systemverilog(&netlist),
    ] {
        let mut comments = Vec::new();
        for line in text.lines() {
            if line.contains("skipped macro x")
                || line.contains(".control")
                || line.contains("shell")
            {
                comments.push(line);
            }
        }
        assert_eq!(comments.len(), 1, "{text}");
        assert!(comments[0].contains("skipped macro x .control shell rm .endc (lua  .end)"));
        assert!(
            !text
                .lines()
                .any(|line| line == ".control" || line == ".endc"),
            "{text}"
        );
    }

    // a trailing `\` would continue the comment onto the next line the writer makes
    netlist.macros[0].name = "y\\".to_string();
    netlist.macros[0].language = "lua\\".to_string();
    for (text, next) in [
        (write_spice(&netlist), ".subckt cell"),
        (write_spectre(&netlist), "subckt cell ()"),
    ] {
        let lines: Vec<&str> = text.lines().collect();
        let index = lines
            .iter()
            .position(|line| line.contains("skipped macro y"))
            .unwrap();
        assert!(!lines[index].contains('\\'), "{text}");
        assert_eq!(lines[index + 1], next, "{text}");
    }
}

#[test]
fn macro_bodies_drop_nul_characters() {
    let mut netlist = macro_netlist(&[], "");
    netlist.macros = vec![oxymorph_schema::Macro {
        name: "n".to_string(),
        language: "spice".to_string(),
        body: ".param a=1\0\n.param b=2".to_string(),
        ..Default::default()
    }];
    let text = write_spice(&netlist);
    assert!(text.lines().any(|line| line == ".param a=1"), "{text}");
    assert!(!text.contains('\0'));
}
