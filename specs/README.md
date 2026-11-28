# Specs

What the code **already does**, in the present tense. A spec never describes
something that is not built yet. Rationale lives in
[`../decisions/`](../decisions/); plans live in [`../ROADMAP.md`](../ROADMAP.md).

Ledgers (e.g. a known-issue table) are allowed here: they describe the current
state, not a plan.

## Relationship to decisions

- `decisions/` records **why** we chose something, once, and is frozen.
- `specs/` records **what** is true now, and evolves.

## Conventions

- One spec per bounded area, named `<area>.md`.
- Requirements are numbered (`R1`, `R2`, …).
- Update the spec in the same commit as the code it describes.

## Index

| Spec | Covers |
|------|--------|
| [architecture.md](architecture.md) | Crate layout and dependency direction |
| [parsing.md](parsing.md) | Engines, preprocessing, error boundary, coverage |
| [ir.md](ir.md) | IR types, serialization, builder, protobuf, paths |
| [parity.md](parity.md) | The equivalence contract, corpus, verification |
| [bindings.md](bindings.md) | The `oxymorph` Python module |
