# Decisions (ADRs)

Architecture Decision Records for oxymorph. Each record captures **one** decision:
the context that forced it, the options weighed, the choice, and the consequences.

This is deliberately separate from [`../specs/`](../specs/), which describes what
the code **already does**. A decision here may describe something that is not
built yet; a spec never does. Plans and sequencing live in
[`../ROADMAP.md`](../ROADMAP.md).

ADRs are append-only. To change a decision, supersede it with a new record and
link both ways.

## Conventions

- File name: `NNNN-short-kebab-title.md` (zero-padded, monotonic, never reused).
- Status, one of:
  - **proposed** — under discussion, not yet accepted.
  - **decided** — accepted; may or may not be implemented yet.
  - **implemented** — in the code, and covered by a spec.
  - **superseded** — replaced by a later record (link it).
  - **withdrawn** — built and then reverted; kept for the record.
- Decisions that affect observable behaviour must link to the spec(s) they
  govern under `specs/`, or state the spec they owe.

## Promoting a decision to a spec

When a decision ships, promote it in the same commit:

1. Change the record's status to **implemented** and link the spec(s) that now
   describe the behaviour.
2. Make the matching spec describe what actually shipped, correcting it where
   reality diverged from the decision.
3. Keep the record. It is the rationale, not the specification.

## Template

```markdown
# NNNN. <Title>

- Status: proposed
- Date: YYYY-MM-DD
- Supersedes: -
- Superseded by: -
- Related specs: `specs/<file>.md`

## Context

What problem are we solving? What forces (constraints, goals, non-goals) apply?

## Decision

The choice, stated in one sentence, then the specifics.

## Options considered

- **Option A** — pros / cons.
- **Option B** — pros / cons.

## Consequences

What becomes easier, what becomes harder, what we now owe (work, docs, tests).

## Verification

How we will know the decision is honored (commands, tests, checks).
```

## Index

| # | Title | Status |
|---|-------|--------|
| [0001](0001-rust-port-of-morphnet.md) | Rust port of the vendored morphnet library | implemented |
| [0002](0002-parser-engine.md) | Parser engine and lexing (lark-rs in-process) | implemented |
| [0003](0003-workspace-layout.md) | Crate workspace and layering | implemented |
| [0004](0004-parity-policy.md) | Parity policy and upstream divergence log | implemented |
| [0005](0005-ir-and-protobuf.md) | IR representation and protobuf strategy | implemented (prost-first step superseded by 0006) |
| [0006](0006-ir-representation.md) | IR representation: hand-written serde types | implemented |
| [0007](0007-oracle-corpus-testing.md) | Oracle-corpus testing strategy | implemented |
| [0008](0008-python-bindings-and-docs.md) | Python bindings and documentation site | implemented ("bindings are thin" superseded by 0012) |
| [0009](0009-wasm-and-playground.md) | WebAssembly target and docs playground | implemented |
| [0010](0010-protobuf-interchange.md) | Protobuf interchange implemented | implemented |
| [0011](0011-extend-beyond-upstream-morphnet.md) | Extend beyond upstream morphnet | implemented |
| [0012](0012-mutable-python-object-model.md) | Mutable Python object model (supersedes part of 0008) | implemented |
