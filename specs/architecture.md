# Architecture

Crate-only workspace (no root `src/`), per `decisions/0003`.

| Crate | Layer | Contents |
|---|---|---|
| `oxymorph-schema` | data | IR types (serde), enums, `IndexMap` maps, JSON/YAML, builder (`_api` mirror). No parsing. |
| `oxymorph-netlist` | parsing | embedded `.lark` grammars, `lark_rs` engine behind a trait boundary, tree-transform harness, helpers, HDL nets (`hdl.rs`), preprocessing, the seven formats (each a cargo feature, all in `default`). |
| `oxymorph` | facade | re-exports `oxymorph-schema` and `oxymorph-netlist`. |
| `oxymorph-python` | bindings | PyO3 module `oxymorph` (excluded from the default workspace; built by maturin). |

Dependency direction: `oxymorph-schema` ← `oxymorph-netlist` ← `oxymorph`
← `oxymorph-python`. `oxymorph-schema` never depends on parsing.

The engine's tree/error types do not leak: `oxymorph_netlist::Error` is the
public error type and the per-format `parse_*` functions return
`Result<Circuit, Error>`.
