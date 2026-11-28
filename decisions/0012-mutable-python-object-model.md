# 0012. Mutable Python object model

- Status: implemented
- Date: 2026-11-26
- Supersedes: the "bindings are thin" part of `0008-python-bindings-and-docs.md`
- Superseded by: -
- Related decisions: `0008-python-bindings-and-docs.md`, `0011-extend-beyond-upstream-morphnet.md`
- Related specs: `specs/bindings.md` (R1, R2: the object model as shipped)

## Context

`decisions/0008` made `oxymorph-python` a thin PyO3 layer: read-only `#[pyclass]`
wrappers whose getters clone the IR. gfpex and gflvs, which move from
`kfnetlist.morphnet` to oxymorph, change netlists in place:

- `module.module_references.extend(...)`, `module.connections = [...]`;
- `circuit.properties.update(...)` and `circuit.properties[key] = value`;
- `p.default_value.prefixed_value.double_value = 2`;
- `copy.deepcopy(circuit)`;
- content hashing with deepdiff `DeepHash`.

With getters that clone, every one of these silently changes a copy. The
bindings also lack the kfnetlist surface: expressions, macros, stimuli,
SystemVerilog, `.gsch`, and the path helpers.

## Decision

**The object model is a graph of mutable Rust `#[pyclass]` objects, one class per
IR type, converted to the `oxymorph_schema` IR at each call.** The only schema is
the Rust IR. No class or field is defined in Python.

- **Layout.** The code in `crates/oxymorph-python/src/` is split by concern:
  - `convert.rs`: the graph ↔ IR conversion, in one place. It also holds the
    `model!` macro, which declares a class from its IR fields.
  - `model.rs`: the classes, with the IR's field names, order and defaults;
  - `enums.rs`: the enums;
  - `builders.rs`: the `morphnet._api` builders;
  - `io.rs`: the parse, write, expression and path functions.

  The package is pure native (maturin `module-name = "oxymorph"`), with no Python
  source.
- **Fields.**
  - Scalars (`str`, `float`, `int`, optional `str`) are plain Rust fields with
    `get` and `set`.
  - Lists and dicts are stored as `Py<PyList>` / `Py<PyDict>`. The getter returns
    the stored object, so `append`, `extend`, item assignment and `update`
    persist. The setter takes any iterable or mapping and stores a new list or
    dict.
  - A nested object is stored as `Py<T>`. So
    `p.default_value.prefixed_value.double_value = 2` persists.
  - An enum field holds the IR enum. It can be set from any int that is a
    member's value, and it reads back as the member.
- **Conversion.** `Model::to_ir` walks the graph, and `Model::from_ir` builds a
  new one. The `model!` macro writes both as struct literals over every field. If
  a field is added to or removed from the IR, the class does not compile until it
  follows.
  - A list or dict item of the wrong type raises `TypeError` and names its path,
    e.g. `modules[0].module_references[3]: expected ModuleReference, got str`.
  - A `ParameterValue` or `Expression` with more than one variant set raises
    `ValueError` and names its path, as Pydantic's oneof validators would.
  - The walk is bounded at `MAX_NESTING` (128) steps. A cycle, or deeper
    nesting, raises `ValueError`, not a stack overflow.
  - `MAX_NESTING` is at least what the IR legitimately holds. The parsers build
    expression trees at most `oxymorph_schema::MAX_EXPRESSION_DEPTH` (32) nodes
    deep, and such a tree under the deepest container (a model reference
    argument of an instance parameter) takes 106 steps. The binding asserts this
    at compile time. The same tree also reads back from serde_json (127 levels),
    YAML and prost (100 messages), so every value a parser returns converts,
    serializes and compares.
- **Value semantics go through the IR.**
  - `==` compares the converted IR values with Rust's `PartialEq`. Dicts compare
    as maps, as in Python and Pydantic.
  - `to_json` is the IR's serde JSON, which is deterministic.
  - `__deepcopy__` converts to the IR and back.
  - `__reduce__` returns `(cls.from_json, (self.to_json(),))`, so `pickle` and
    `copy.copy` work.
  - Every class has `to_json` and `from_json`. `Circuit` also has
    `to_json_pretty`, `to_yaml` and `from_yaml`, and `to_proto_bytes` and
    `from_proto_bytes`.
  - Objects are unhashable, because they are mutable. deepdiff `DeepHash` hashes
    their attributes.
- **Enums.** At import, the binding builds `enum.IntEnum` classes from a Rust
  table of `(name, members)`. Members compare equal to the IR integers. This
  works with abi3-py39.
- **Garbage collection.** Each class implements `__traverse__` and `__clear__`,
  so reference cycles are collected. Examples are an expression that contains
  itself, or a list that holds its owner.
- **Builders and chaining.** `Circuit.add`, `Module.connect` and the `module`,
  `port` and `connection` builders are small Rust functions in the binding.
  `oxymorph_schema::builder` works on IR values, so it cannot build the graph.

### Considered and rejected

- **Dataclasses over a JSON bridge** (commits `8b6a0aa` and `cec0d8f`). Each IR
  type was a Python dataclass, and the native module only moved IR JSON in and
  out. It passed the tests, but **the user rejected it.** The dataclasses are a
  second schema, and the two must be kept in sync by hand. The user had already
  chosen "mutable Rust classes" over "Python models over a JSON bridge" to avoid
  that cost.
- **Read-only wrappers that clone** (`decisions/0008`). Every in-place edit would
  silently change a copy.

## Consequences

Easier:

- There is one schema. A new IR field is one line in `model.rs`, and the
  compiler points to it.
- gfpex and gflvs keep their mutation, copying and hashing code.
- Parse and write calls do not pay for a JSON encode and decode.

Harder:

- Each write, `to_json` or `==` converts the whole graph. The cost is linear:
  10 000 references and 30 000 connections take about 20 ms
  (`crates/oxymorph-python/benches/bench_roundtrip.py`).
- deepdiff `DeepHash` walks the attributes of every object. On large circuits
  this is slow: about 6 s for 40 000 objects. Hashing `to_json()` is the fast
  path.
- An expression built in Python deeper than the converter's bound does not
  convert; serde_json's 127 levels limit `Circuit::from_json` in Rust the same
  way. The parsers stay below both (`MAX_EXPRESSION_DEPTH`): deeper text is kept
  as `raw`.

The binding tests run with pytest (`just test-python`), separately from
`cargo test`, which stays Python-free; the CI job `python` runs them.

## Verification

- `crates/oxymorph-python/tests/test_oxymorph.py`, run with `just test-python`:
  - `test_nested_mutation_is_written`, `test_nested_scalar_mutation_persists`
    and `test_list_attribute_reassignment_is_written`;
  - `test_type_error_names_the_path`, `test_cycle_is_a_value_error` and
    `test_oneof_is_checked_on_write`;
  - `test_expression_at_the_bound_round_trips`;
  - `test_deepcopy_is_independent`, `test_equality_by_value` and
    `test_pickle_round_trip`;
  - `test_to_json_is_stable_for_hashing` and `test_deephash_is_stable`;
  - `test_object_model_is_native`;
  - `test_gfpex_flow` and `test_gflvs_flow`, plus the `*_surface` consumer tests.
- `cargo clippy --manifest-path crates/oxymorph-python/Cargo.toml -- -D warnings`.
