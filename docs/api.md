# API reference

The installed package is named `oxymorph`. It exposes the functionality of
upstream `morphnet` and kfnetlist through its own API, which is *not*
API-compatible with either (`decisions/0012`).

## Object model

The IR types are native classes, with the IR's field names, order and defaults:

- `Circuit`, `Module`, `ModuleReference`, `ExternalModule`, `ExternalLibrary`;
- `Port`, `PortReference`, `Connection`, `Bus`;
- `Parameter`, `ParameterValue`, `PrefixedValue`, `ModelReference`, `ModelInterface`;
- `Directive`, `Macro`;
- `Simulation`, `Analysis`, `OutputRequest`, `Measurement`, `InitialCondition`,
  `Stimulus`, `Waveform`;
- `Expression`, `UnaryOp`, `BinaryOp`, `Call`, `Ternary`.

The constructors take keywords only, e.g. `Circuit(name="x", top_module="top")`.
Every attribute can be read and written, and edits persist:

- A list or dict attribute returns the same list or dict each time. Changing it
  in place changes what is written.
- Assigning a list or dict attribute stores a new list or dict built from any
  iterable or mapping.
- A nested object is shared, so `p.default_value.prefixed_value.double_value = 2`
  changes `p`.

```python
top = circuit.modules[0]
top.module_references.append(oxymorph.module_reference("R0", "resistor", value=2.0))
top.connections = [c for c in top.connections if c.name != "OUT"]
circuit.properties.update({"backannotated": "true"})
```

- **Equality.** `==` is by value: it compares the IR values. Objects are mutable
  and unhashable.
- **Copying and pickling.** `copy.copy`, `copy.deepcopy` and `pickle` work.
  deepdiff `DeepHash` walks the attributes. On large circuits, hashing
  `to_json()` is much faster.
- **JSON.** Every class has `to_json()` and `Cls.from_json(text)`, using the IR's
  JSON.
- **Type checks.** A wrongly typed list or dict item raises `TypeError` at the
  next parse, write, serialization or comparison, and names its path, e.g.
  `modules[0].module_references[3]: expected ModuleReference, got str`. A scalar
  or nested-object attribute checks its type when it is set. A graph nested
  deeper than `oxymorph.MAX_NESTING` (128) steps, such as a cycle, raises
  `ValueError`.

`Circuit` also serializes to YAML and protobuf, through the Rust IR:

```python
Circuit.to_json() -> str          # canonical: deterministic, equal for equal circuits
Circuit.to_json_pretty() -> str
Circuit.to_yaml() -> str
Circuit.to_proto_bytes() -> bytes
Circuit.from_json(text) -> Circuit
Circuit.from_yaml(text) -> Circuit
Circuit.from_proto_bytes(data) -> Circuit
```

Use `to_json()` as a cache key when you hash a circuit.

## Enums

The enums are `enum.IntEnum`s with the IR's values, so they compare equal to
those integers. An enum attribute reads back as the member, and accepts any int
that is a member's value:

- `PortDirection`: INOUT, INPUT, OUTPUT.
- `SignalDomain`: UNSPECIFIED, ELECTRICAL, WAVEGUIDE.
- `SIPrefix`: UNSPECIFIED, then QUECTO … QUETTA.
- `DirectiveKind`, `ExternalModuleKind`, `AnalysisKind`, `OutputRequestKind`,
  `WaveformKind` and the expression `Operator`.

## Parsers

Each parser returns a `Circuit` and raises `ValueError` on failure.

```python
oxymorph.parse_spice(text) / parse_hspice / parse_xyce / parse_spectre / parse_spectre_spice
oxymorph.parse_systemverilog(text, defines=None, disciplines=None, pin_domains=None)
oxymorph.parse_vams(text, defines=None, disciplines=None, pin_domains=None)
oxymorph.parse_gsch(text, *, models=None, sim="ngspice", corners=None, name=None)
oxymorph.read_gsch(path, *, project_root=None, models=None, sim="ngspice", corners=None)
```

The HDL options are:

- `defines`: drives `` `ifdef ``, e.g. `{"IS_SIM": "1"}`. With none, the
  `` `else `` branches are taken.
- `disciplines`: maps a net type to its `SignalDomain`, e.g.
  `{"connection": SignalDomain.WAVEGUIDE}`.
- `pin_domains`: maps cell → pin → domain, and wins over `disciplines`.

For `.gsch`, `models` is the nyanlib JSON and `corners` is a section name or a
list of them.

## Writers

Each writer takes a `Circuit`, returns the netlist text, and raises `ValueError`
on failure:

```python
oxymorph.write_spice(circuit) / write_hspice / write_xyce / write_spectre
oxymorph.write_spectre_spice / write_vams / write_systemverilog / write_gsch
```

## Builder

The builder mirrors kfnetlist's `morphnet._api`, with oxymorph names.

```python
oxymorph.circuit(name, *, top_module="", properties=None) -> Circuit
oxymorph.module(name, *, class_name="", parameters=None, properties=None) -> Module
oxymorph.module_reference(name, module_name, *, class_name="", parameters=None,
                          properties=None, **param_kwargs) -> ModuleReference
oxymorph.ext_module(name, *, class_name="", library_name="", group_name="",
                    ports=None, parameters=None, properties=None) -> ExternalModule
oxymorph.port(name, *, direction=None, domain=None, width=0, discipline="",
              properties=None) -> Port        # INOUT, UNSPECIFIED when None
oxymorph.connection(name, source, target, *, domain=None,
                    properties=None) -> Connection
oxymorph.parameter(name, *, default_value=None, description="", properties=None) -> Parameter
oxymorph.external_library(path, *, model_type="", external_model_names=None) -> ExternalLibrary
oxymorph.model_interface(name, *, function_name="", parameters=None, properties=None) -> ModelInterface
```

`parameters` accepts a mix of `Parameter` objects, plain names (`"l"`), and
`(name, default)` tuples. Keyword arguments (`value=2.0`) are also parameters.
`properties` dicts are copied.

## Chaining methods

Each of these methods returns `self`.

```python
Module.add_terminal(name, *, direction=None, domain=None, width=0, discipline="", properties=None)
Module.add(*module_references)
Module.connect(net_name, *endpoints, domain=None, properties=None)
Circuit.add(*modules)        # top_module defaults to the first module added
Circuit.add_ext(*ext_modules)
Circuit.add_library(name, library)
```

`Module.connect` chains its endpoints into connections, so endpoints A, B, C give
A→B and B→C. An endpoint is `(instance_name, port_name)`. Instance `""` is the
module's own port. `connect` needs at least two endpoints and raises `ValueError`
with fewer; to name the net of a single terminal, pass it twice.

## Expressions and paths

```python
oxymorph.parse_expression(text) -> Expression       # Expression(raw=text) when it does not parse
oxymorph.format_expression(expression, dialect, *, named=False) -> str
oxymorph.posix_file(path) -> str
oxymorph.is_absolute(path) -> bool
oxymorph.resolve_file(path, base=None) -> str
```

`dialect` is one of ngspice, xyce, hspice, spectre, vacask, systemverilog, vams
or gsch.

The path helpers accept `str` or `os.PathLike` and return the stored form:
POSIX, with `.` and `..` resolved lexically.

## Tests

`just test-python` builds the package with uv and maturin and runs
`crates/oxymorph-python/tests` with the standard library's `unittest`.
