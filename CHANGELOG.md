# Changelog

<!-- towncrier release notes start -->

## oxymorph 0.1.0 (2026-11-28)

### Features

- A Python package: a mutable object model with the IR's field names and defaults, enums, builders, pickling and deep copy, bounded `repr`, and a catch-all typing stub (`oxymorph.pyi`, `py.typed`; every name is `Any`) so type checkers resolve the imports.
- A wasm32 build and an in-browser playground in the docs.
- Expressions carried as trees and written in each format's dialect, macros written by every writer, stimuli and PWL sources, path resolution, and numeric subcircuit ports.
- Parse and write SPICE, HSPICE, Xyce, Spectre, Spectre-SPICE and Verilog-AMS netlists through one intermediate representation, equivalent to upstream morphnet across the oracle corpus (357 parse cases, 2142 writer outputs); each format is a cargo feature.
- Protobuf interchange with morphnet's `circuit.proto` and `simulation.proto` (`to_proto_bytes` / `from_proto_bytes`), alongside JSON and YAML.
- SystemVerilog parsing with bit-level HDL nets, and `.gsch` schematics.
- Untrusted netlists are bounded: one expansion budget per HDL parse, capped port widths in the writers, one expression depth bound (`MAX_EXPRESSION_DEPTH`) that every format reads back, and duplicate header ports rejected.
- abi3 wheels for CPython 3.9+ on Linux (glibc and musl, x86_64 and arm64), macOS (Intel and Apple Silicon) and Windows (x64 and arm64), each tested on its own machine before the tag-gated PyPI release, plus a tested sdist.

### Documentation

- Documentation site with the API reference, examples, the generated Equivalence page and the playground; the design decisions behind the port are recorded in the repository's `decisions/`.
