# 0011. Extend beyond upstream morphnet

- Status: implemented
- Date: 2026-11-25
- Supersedes: -
- Superseded by: -
- Related decisions: `0001-rust-port-of-morphnet.md`, `0002-parser-engine.md`, `0004-parity-policy.md`, `0007-oracle-corpus-testing.md`
- Related specs: `specs/parity.md` (the overrides and R4 rows), `specs/parsing.md`
  (R5–R9, the ported features), `specs/ir.md` (the added IR types),
  `specs/bindings.md`

## Context

oxymorph ports the vendored Python `morphnet`, and `decisions/0004` makes that
vendored copy the oracle, bugs included. The user has since developed new features
and fixes in a fork, `kfnetlist.morphnet` (Python; `upstream/main` at `e596bb5`,
plus `f020af6` on `fix/numeric-subckt-ports`). oxymorph must gain those features.
Some of them change outputs that the committed corpus pins, so "match the
vendored oracle everywhere" can no longer hold.

## Decision

**oxymorph extends beyond the vendored morphnet by porting kfnetlist's features.
The oracle splits in two.**

- **Pre-fork behaviour** stays pinned by the committed corpus
  (`tests/corpus/cases.json`), generated from `vendor/morphnet`.
- **Ported features** are pinned by the ported kfnetlist tests and fixtures
  (`crates/oxymorph-netlist/tests/data/kf/**`).
- **A ported fix that changes an original-corpus expectation** is a deliberate
  divergence. It gets an entry in `tests/corpus/overrides.json` and an R4 row with
  status `diverged` in `specs/parity.md`, and the code carrying it is marked
  `// parity: <R4 id>`. This ADR is the enabling ADR required by `decisions/0004`
  for those rows.

**Overrides.** `tests/corpus/overrides.json` has the shape
`{"<format>/<case name>": {"r4": "<R4 id>", "json"?: <IR>, "writes"?: {"<fmt>": "<text>"}}}`.
`parse_and_write_match_the_oracle` replaces the named case's `json` and
`writes[<fmt>]` before comparing. `cases.json` is never edited. The parity tests
fail if an override names an R4 id absent from the R4 table, or a case absent from
the corpus. R4 ids for these divergences look like `kf-<short-slug>`.

**Names stay oxymorph's.** kfnetlist names map back when ported (code, tests,
fixtures, protos); protobuf field numbers stay kfnetlist's.

| kfnetlist | oxymorph |
|---|---|
| `Netlist` | `Circuit` |
| `Circuit` | `Module` |
| `SubCircuit` | `ModuleReference` |
| `ExternalCircuit` | `ExternalModule` |
| `ExternalCircuitKind` | `ExternalModuleKind` |
| `top_circuit`, `circuits`, `ext_circuits`, `subcircuits`, `circuit_name` | `top_module`, `modules`, `ext_modules`, `module_references`, `module_name` |
| `circuit()`, `subcircuit()`, `ext_circuit()` | `module()`, `module_reference()`, `ext_module()` |

**Grammar provenance.** The `.lark` grammars are verbatim kfnetlist `f020af6`
(byte-identical once the port completes), replacing "verbatim upstream".

**IR provenance.** For types added by this port, the kfnetlist Pydantic models at
`e596bb5` are the normative spec; they are read, not executed. Protos for the new
IR types move to `crates/oxymorph-schema/proto/` (kfnetlist-derived, names mapped
back); the vendored protos remain the reference for the original corpus.

**AGENTS.md** is amended accordingly: grammars are verbatim from kfnetlist
`f020af6`, and for added types the kfnetlist models at `e596bb5` are normative.

**Delivery.** This work lands as stacked pull requests (the user's choice). The
direct-push guidance in AGENTS.md stays for other work.

## Options considered

- **Keep bug-for-bug parity with the vendored copy** — rejected: the fork's
  behaviour is the goal, and the corpus cannot pin both.
- **Re-vendor kfnetlist as the oracle** — rejected: the Rust names, the module
  layout and the dropped bridge/stash code differ, so a second corpus would not
  be the same contract.
- **Overrides beside the corpus (chosen)** — the original corpus stays untouched
  and every divergence is explicit and tied to an R4 row.

## Consequences

Easier: features and fixes land without silently moving the oracle; every
divergence is a reviewable JSON entry plus an R4 row.

Harder / owed: each task that changes an original-corpus output adds an override
and an R4 row; `vendor/morphnet` is still never edited.

## Verification

`cargo test -p oxymorph-netlist --test parity` runs `overrides_reference_r4_rows_and_name_existing_cases`
against the real overrides, specs and corpus.
