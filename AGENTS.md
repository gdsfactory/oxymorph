# AGENTS.md

oxymorph is decision-driven and spec-driven. Follow this flow on every task.

1. **Orient.** Before editing, read the [`decisions/`](decisions/README.md),
   [`specs/`](specs/README.md) and [`ROADMAP.md`](ROADMAP.md) that govern the
   paths you are touching.
2. **Decide before building.** A substantial feature or design change gets a
   decision record in `decisions/` (status **decided**, indexed in
   `decisions/README.md`) *before* it is implemented. Bug fixes, refactors and
   small tweaks do not need one. If unsure, ask.
3. **Specs in the same commit.** Any change to implemented behaviour updates
   the matching spec in the same commit. Specs are present tense and describe
   what the code does, never what it will do.
4. **Promote on implementation.** When a decision ships, change its status to
   **implemented** and link the spec that now describes it (see
   `decisions/README.md`).
5. **Never edit a decided record's substance.** Supersede it with a new record
   and link both ways.

## What this project is

A Rust port of the vendored Python `morphnet` library: bidirectional
SPICE-family / Verilog-AMS netlist parsing through a shared Protocol Buffers IR.
`vendor/morphnet` is a **read-only reference** — never edit it, never patch it.

## Upstream is the oracle, including its bugs

- Parity with the vendored Python is the acceptance criterion. See
  [`specs/parity.md`](specs/parity.md) and
  [`decisions/0004`](decisions/0004-parity-policy.md).
- **Do not fix upstream bugs.** Log them in `specs/parity.md` §R4 and reproduce
  the buggy behaviour. A fix is a deliberate divergence and needs an ADR first
  ([`decisions/0011`](decisions/0011-extend-beyond-upstream-morphnet.md) covers
  fixes ported from kfnetlist, via `tests/corpus/overrides.json`).
- Code that intentionally deviates carries `// parity: <id>` resolving to an R4
  row.

## Hard invariants

- **No root `src/`.** Workspace members live in `crates/` (see
  [`decisions/0003`](decisions/0003-workspace-layout.md)).
- **One-way layering:** `oxymorph-schema` ← `oxymorph-netlist` ← `oxymorph`
  ← bindings. `oxymorph-schema` must never depend on parsing.
- **The IR is Pydantic-normative** ([`decisions/0005`](decisions/0005-ir-and-protobuf.md)).
  When the proto and the Pydantic models disagree, Python's behaviour wins. For
  types added by the kfnetlist port, the kfnetlist Pydantic models at `e596bb5`
  are the normative spec (read, not executed;
  [`decisions/0011`](decisions/0011-extend-beyond-upstream-morphnet.md)).
- **The protobuf schema is Rust, in `crates/oxymorph-schema/src/proto.rs`**
  ([`decisions/0006`](decisions/0006-ir-representation.md)): hand-written
  encode/decode, no `.proto` compiled, no `prost` or `protoc`. Field numbers are
  upstream's plus kfnetlist's, unchanged. `vendor/morphnet/protos/` stay the
  reference for the original corpus (whose oracle bytes must still decode
  message-equal).
- **Grammars are verbatim `.lark` files from kfnetlist `f020af6`** (upstream
  morphnet's until the port reaches it), embedded via `include_lark!`
  ([`decisions/0002`](decisions/0002-parser-engine.md)). Do not translate them to
  another parser DSL, and do not edit them to make a test pass.
- **The parser is behind our own trait.** Generated tree/error types must not
  leak past `oxymorph-netlist`; map `String` errors to our structured error type.
- **Core tests need no Python.** The golden corpus is committed; the
  differential harness is a separate, non-blocking job.
- **The bindings hold no netlist logic.** `oxymorph-python` is the Python package
  `oxymorph`: a graph of mutable `#[pyclass]` objects that mirrors the IR and is
  converted to it in `convert.rs`
  ([`decisions/0012`](decisions/0012-mutable-python-object-model.md)). The Rust IR
  is the only schema, so do not define classes or fields in Python. The package
  is not API-compatible with `morphnet`. Its tests run with `just test-python`.

## Committing and pushing

This is early-stage development. Committing and pushing directly to `main` is
allowed and encouraged (Floris Laporte, 2026-02-11). Branches and pull requests
are not required.

**Commit and push often.** Prefer small, frequent commits over large ones — one
coherent change each, with the `decisions/`/`specs/` updates in the same commit.
Push after each working step (green checks below) rather than batching a
session's work. Do not leave the tree dirty at the end of a task.

## Checks

Before pushing, all of these must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p oxymorph --no-default-features --features spectre  # one per format
cargo build --workspace --target wasm32-unknown-unknown           # wasm crate
```

`cargo test` on the workspace must not require Python, `maturin` or a WASM
target; binding crates are excluded from default members.
