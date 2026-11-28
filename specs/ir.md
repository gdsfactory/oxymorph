# IR

The intermediate representation (`decisions/0005`, `decisions/0006`).

## R1. Types

Hand-written Rust types mirror the upstream Pydantic models field-for-field:
`Circuit`, `Module`, `Port`, `PortReference`, `Connection`, `Bus`,
`ModuleReference`, `Parameter`, `ParameterValue`, `PrefixedValue`,
`ModelReference`, `ModelInterface`, `Directive`, `ExternalModule`,
`ExternalLibrary`, and the simulation types `Simulation`, `Analysis`,
`OutputRequest`, `Measurement`, `InitialCondition`.

Types and fields added by the kfnetlist port mirror kfnetlist's Pydantic models
at `e596bb5` (`decisions/0011`):

- `Expression` (one optional field per node: `literal`, `reference`, `unary`,
  `binary`, `call`, `ternary`, `raw`) with `UnaryOp`, `BinaryOp`, `Call`,
  `Ternary` and the `Operator` enum; `Expression::literal(f64)` and
  `Expression::reference(&str)` build the leaves. `raw` is dialect text no
  grammar parsed.
- `ParameterValue.expression`, a third oneof variant beside `prefixed_value` and
  `model_ref`.
- `Macro { name, language, body, properties }`, a verbatim block in a simulator
  language, on `Module.macros` and `Circuit.macros`.
- `Stimulus { name, dc, ac_magnitude, ac_phase, waveform }` on
  `Simulation.stimuli`, named `"<module>/<instance>"` after the source element
  whose `properties["stimulus"]` points at it; `Waveform { kind, args, points,
  file }` with `WaveformKind` (`Pulse`, `Sin`, `Exp`, `Sffm`, `Pwl`).

`validate()` mirrors the Pydantic validators, and is called explicitly:
`ParameterValue` and `Expression` (whole tree) reject more than one variant set
(`"… is a oneof: …"`); `Waveform` rejects `points` together with a `file`, and a
`file` on a non-PWL waveform. `check_oneof()` is the `ParameterValue` / `Expression`
check of one node alone (the Python binding runs it on every node it converts).

`MAX_EXPRESSION_DEPTH` (32) is the deepest expression tree the parsers build, counted
in nodes from the root to the deepest leaf. A tree that deep, held where the IR nests
an expression deepest (a model reference argument of an instance parameter), reads
back from JSON and YAML (127 nested levels at most), protobuf (100 nested messages)
and the Python converter (`MAX_NESTING`, 128 steps); the constant's doc gives the
arithmetic. The IR itself does not enforce it: a deeper tree built in Rust or Python
is written, and fails only where a reader's own limit is reached.

## R2. Serialization

- Field names match the Python field names (`ports`, `port_name`, …).
- Enums serialize as integers (Pydantic `IntEnum`).
- Maps are insertion-ordered (`IndexMap`), matching Python dicts; writer output
  can depend on this order.
- `to_json` / `to_yaml` / `from_json` / `from_yaml` round-trip, and `Circuit`
  implements `PartialEq` on the IR.
- Fields added by the kfnetlist port are left out when unset or empty (`None`
  options, empty `macros` / `stimuli`), as kfnetlist's `to_json` does; the
  original fields keep morphnet's `null`s, which the pre-fork corpus pins.
  `Expression` JSON is flat (`{"binary": {"op": 3, "lhs": …, "rhs": …}}`).
- `Waveform.file` is stored POSIX (`paths::posix_file`) on read and write; `""`
  is no file.

## R3. Builder

`oxymorph_schema::builder` mirrors `morphnet/_api.py` (`circuit`, `module`,
`module_reference`, `ext_module`, `port`, `connection`, `parameter`,
`external_library`, `model_interface`), including mixed parameter specs
(`Parameter`, name, or `(name, default)`) and `module_reference` float kwargs.

## R4. Protobuf

Implemented (`decisions/0010`). Like the IR (`decisions/0006`), the wire schema is
hand-written Rust: `crates/oxymorph-schema/src/proto.rs` encodes each IR type to the
wire format and decodes it back, naming every field number. No `.proto` is compiled
and the crate has no protobuf dependency. The field numbers are upstream's
(`vendor/morphnet/protos/`, the reference for the original corpus) plus the fields
kfnetlist added (`decisions/0011`). Wire names that differ from the IR's are mapped
there (`ports` are `terminals`, `port_name` is `terminal_name`), as are the
`ParameterValue` and `Expression` oneofs and the enums.

`Circuit::to_proto_bytes()` writes fields in field-number order and leaves default
scalars out; maps are written in the IR's order and read back in wire order.
`Circuit::from_proto_bytes()` skips unknown fields, accepts `temperatures` packed or
not, keeps the last value of a repeated singular field and the last member of a
oneof, and returns an error, never a panic, for malformed input or nesting deeper
than `proto::MAX_MESSAGE_DEPTH` (100) messages or map entries.

Model libraries go through the wire with their index intact: `Circuit.libraries`
(45, `map<string, ExternalLibrary>`: `path` 2, `model_type` 3,
`external_model_names` 4) and, per external module, `library_name` (4) and
`group_name` (5), the group within the library, usually the corner. A corner chosen
when parsing (`.lib path section`, gsch `corners`) is a `LIB` `Directive`.

`from_proto_bytes(upstream.to_proto_bytes())` equals the parsed IR for the whole
corpus (message-equality).

## R5. Paths

`oxymorph_schema::paths` (`posix_file`, `is_absolute`, `resolve_file`) gives one
answer on every OS, the same as kfnetlist's: `\` and `/` both separate, `.` and
`..` resolve lexically, `/x`, `C:/x`, `C:x` and `\\server\share` are absolute,
and `..` never climbs above a root (`/`, a drive, a share). The shared case table
is `crates/oxymorph-schema/tests/data/path_cases.json`.
