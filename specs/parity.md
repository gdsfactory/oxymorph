# Parity

Living spec for functional equivalence between oxymorph and the vendored Python
morphnet. Authoritative for what "functionally the same" means, how it is
verified, and which upstream defects we knowingly reproduce.

Related decisions: `decisions/0001` (port, exact match), `decisions/0002`
(parser engine), `decisions/0004` (bug-for-bug parity policy), `decisions/0011`
(divergence for ported kfnetlist features).
The plan for reaching this contract lives in `ROADMAP.md` §Testing plan.

## Scope

Parity is defined per **surface**, not globally. A surface (and, for
parse/write, each format) counts as "ported" only when its requirements hold.

| Surface | Input → output |
|---|---|
| preprocess | raw text → cleaned text / preprocess result |
| parse | text → IR |
| write | IR → text |
| round-trip | text → IR → text (and IR → text → IR) |
| builder (`_api`) | programmatic construction → IR |
| helpers | value/net/simulation helpers (`value_utils`, `net_utils`, `simulation_data`) |
| interchange | IR ↔ protobuf bytes; IR ↔ JSON/YAML |
| errors | malformed input → error kind + position |

Formats: `spice`, `hspice`, `xyce`, `spectre`, `spectre-spice`, `vams`
(`spectre_spice` reuses the spice and spectre parsers/writers).

## R1. Definition of equivalence

- **R1.1 Parse parity** — for a given input, the Rust IR and the Python IR are
  **message-equal** after canonicalization. Canonical form is the deterministic
  protobuf encoding of the IR; comparison is on the decoded message, not on
  source text. (Wire bytes need not be identical; see Q3.)
- **R1.2 Write parity** — for a given IR, the Rust writer output equals the
  Python writer output. Upstream's own tests assert writer output **byte-equal**
  to committed golden netlists, so the corpus bar is byte-exact, not merely
  canonicalized. Where no golden exists, compare canonicalized text (whitespace
  and line-ending normalization only).
- **R1.3 Round-trip parity** — for the corpus: `write(parse(text))` is stable
  (`write(parse(text)) == write(parse(write(parse(text))))`) and matches the
  Python round-trip under R1.1/R1.2.
- **R1.4 Preprocess parity** — for a given raw input, the Rust preprocess result
  equals Python's (string-equal cleaned text, and equal directive/analog-block
  lists where the Python result is a record).
- **R1.5 Helper parity** — every public helper in `value_utils`, `net_utils` and
  `simulation_data` returns the same value as Python for the same input
  (including SI-value parsing/formatting and net maps).
- **R1.6 Builder parity** — the programmatic `_api` (`circuit`, `module`,
  `port`, `connection`, `parameter`, `module_reference`, `ext_module`,
  `external_library`, `model_interface`) produces the same IR as Python for the
  same calls, including defaults and validation.
- **R1.7 Interchange parity** — JSON/YAML dumps are shape-equal to Python's for
  the corpus, and alias/oneof semantics are reproduced. Protobuf: decoding
  upstream's `to_proto_bytes()` reproduces our IR message-equal for all corpus
  cases (`decisions/0010`). Wire byte-exactness is not required (map ordering
  differs).
- **R1.8 Error parity** — where Python raises for malformed input, Rust returns
  an error of the equivalent kind at the equivalent position. Exact message text
  is **not** required. (Open: which error kinds must correspond; see Q2.)

## R2. Corpus

- **R2.1 Sources** (provenance recorded per fixture — upstream commit + path +
  test name):
  - `tests/netlists/{spectre,hspice,vams}/*.{scs,sp,vams}` — netlist inputs and
    **writer goldens** (upstream asserts byte-equality against these).
  - `tests/netlists/*.yaml` — expected IR as Pydantic YAML
    (`rc_lowpass`, `resistor_divider`, `hierarchical_subckt`,
    `inverter_with_models`).
  - Inline netlists in `tests/test_*_parser.py`, `test_*_writer.py`,
    `test_*_roundtrip.py`, `test_*_simulation.py`, `test_value_utils.py`,
    `test_circuit_models.py` (extracted by AST walk).
  - `examples/inverter.py`, `examples/mzi.py` — builder-API scripts.
  - kfnetlist ports (`decisions/0011`) — the second oracle: tests and fixtures
    ported from `kfnetlist.morphnet` (kfnetlist `e596bb5` plus `f020af6`) pin the
    features added beyond the vendored morphnet. Ported fixtures live under
    `crates/oxymorph-netlist/tests/data/kf/**`, with names mapped to oxymorph's per
    `decisions/0011` (YAML/JSON keys only; netlist text is copied byte-for-byte).
  - R4 transforms (`TRANSFORMS` in `crates/oxymorph-netlist/tests/parity.rs`) —
    rewrite the expected IR of every case for a systematic ported fix; each is
    keyed by an R4 row.
  - `tests/corpus/overrides.json` — replaces the expected `json` / `writes[fmt]`
    of named original-corpus cases that a case-specific ported fix changes; every
    entry names an R4 row.
- **R2.2 Extraction** — a one-time step per re-vendor (`tools/extract_oracle.py`),
  run with `uv` against the vendored Python, emitting committed fixtures under
  `tests/corpus/` and a provenance manifest. Core tests then run offline with no
  Python.
- **R2.3 Coverage** — the corpus is **the** definition of covered behavior. An
  input not in the corpus is untested, not implicitly conformant. Coverage is
  tracked in R5.

## R3. Verification mechanisms

- **R3.1 Golden fixtures** — required for merge. Rust tests assert R1 against the
  committed corpus.
- **R3.2 Differential harness** — non-blocking CI job that runs the vendored
  Python (`uv`) and Rust on the same inputs and diffs them; catches drift the
  corpus misses. Covers randomized/fuzzed inputs, not just fixed cases.
- **R3.3 Known-issue allow-list** — both mechanisms may xfail entries recorded in
  R4. An entry without an enabling ADR is a review blocker.
- **R3.4 Upstream test replay** — non-blocking, stretch: run upstream's own
  `pytest` suite against `oxymorph-python` through a thin `morphnet`-shaped shim,
  converting upstream's assertions into an independent oracle we did not write.
  Optional because it depends on upstream's private API shape and the
  not-API-compatible decision (`decisions/0001`).

## R4. Known upstream issues

Policy: reproduce upstream behavior exactly, including bugs
(`decisions/0004`). The exception is a ported kfnetlist fix that changes an
original-corpus expectation (`decisions/0011`): it is a `diverged` row, and the
code carrying it is marked `// parity: <id>`. Such ids are `kf-<short-slug>`.

- **Systematic divergences are transforms.** A fix that changes many cases the
  same way is a named function in `TRANSFORMS` (`tests/parity.rs`), keyed by its
  R4 id. It rewrites the expected IR of every case, both the corpus `json` and the
  IR decoded from the oracle protobuf, by calling the same helper the parser uses.
- **Case-specific divergences are overrides.** An entry in
  `tests/corpus/overrides.json` replaces the corpus `json` and/or `writes[fmt]` of
  one case. The parsed IR is compared with the overridden `json`.

The oracle protobuf of every case that has one is decoded and compared, after the
transforms, with the oracle `json` (never an override: the protobuf encodes the
oracle IR). This checks decoding on every case; parsing is checked against the
expected `json`, and our own encoding by the protobuf round-trip of the parsed IR.

`overrides_reference_r4_rows_and_name_existing_cases` fails if an override names
an id that is not in this table or a case that is not in the corpus;
`transforms_reference_r4_rows` fails if a transform's id is not in this table.
Each new row is checked against kfnetlist with `tools/compare_kfnetlist.py`
(runs kfnetlist `e596bb5` in an isolated `uv` env and reports per-case IR and
writer diffs against oxymorph, names mapped; not run by `cargo test`).

| id | upstream | symptom | our behavior | repro | status | ADR |
|---|---|---|---|---|---|---|
| kf-signal-domains | kfnetlist 1deb1e0 (#34) | every connection has domain `UNSPECIFIED` | a connection takes its net's domain: `WAVEGUIDE` when the net touches an `o<N>` terminal, else `ELECTRICAL` (`helpers::domain_of`); ports named `o<N>` are `WAVEGUIDE` | applied as a transform in `tests/parity.rs` (changes the IR of 191 corpus cases) | diverged | 0011 |
| kf-device-pin-template | kfnetlist 1deb1e0 (#34), 0a5184b (#36) | a built-in device with no external-module declaration is written `M1 ? ? ...` (SPICE) or in inferred pin order (Spectre) | SPICE writes the device pin template order (`d g s b` for `M`); Spectre does too when the instance has every template pin, else infers the order | override of `vams/netlist/vams/inverter_with_models.vams` (`writes` for spice, hspice, xyce, spectre, spectre_spice) | diverged | 0011 |
| kf-expressions | kfnetlist d0daf3e, c12a71e, 9766e84, 1adb071, d94dc38 | a `{…}`, `'…'` or bare-identifier parameter value (and a Spectre bare expression) parses to an empty `ParameterValue`, written `0` | the value is an `Expression` (`expression::parameter_value`: a number stays `prefixed_value`, an identifier is a `reference`, unparseable text is `raw`); writers print it in their dialect (`{…}` ngspice/Xyce, `'…'` HSPICE, bare Spectre/VACASK/VAMS) | overrides of `inline/52`, `inline/53`, `inline/54` for spice, hspice, xyce and spectre_spice (`json` and all 6 `writes`; their SPICE-family writes also carry kf-spice-writer-dialects) | diverged | 0011 |
| kf-spice-writer-dialects | kfnetlist 5924917, 5671e52, 9766e84, d94dc38 | subckt params are written as `.param` body lines; HSPICE, Xyce and Spectre-SPICE output is the ngspice writer's | params on the `.subckt` header (Xyce: after `PARAMS:`, also on X calls); `write_hspice` / `write_xyce` print their dialects; the device letter (`X` for a defined subckt, else `spice_prefix`) is prepended when missing; `ground_net` is node `0`; `sim_skip` instances and `role=source` params are not written | overrides of hspice/inline/26, vams/inline/101, 107, 108, 109, 110, 123 (`writes` for spice, hspice, xyce, spectre_spice); xyce/inline/12 and xyce/inline/48 carry it under kf-bare-values, vams/inline/117 under kf-vams-sources | diverged | 0011 |
| kf-one-pin-nets | kfnetlist 30a1621, 6db1e11, 5559e43 | a net with one pin (a port no element uses, a dangling instance pin) is dropped, and a pin with no net is written `?` | a one-pin net is a source-only connection; a pin with no net is written on its own node `nc_<instance>_<pin>` (SPICE: the instance name as written, with its device letter), which a parser reads back as no net (a one-pin net named `nc_<instance>_…`) | applied as a transform in `tests/parity.rs` for ports no element uses (changes the IR of 52 corpus cases); overrides of 56 cases whose dangling instance pins the oracle IR has no net name for (55 `json`, 334 `writes`) | diverged | 0011 |
| kf-top-elements | kfnetlist b4dc1ec, 6db1e11, 650763b | the top module is the last module, so a deck with subcircuits and top-level elements names a subcircuit the top | `__top__` (the deck's top-level elements) is the top whenever it exists; Spectre-SPICE, and Spectre with `simulator lang=` sections, merge the top-level elements of every section into one `__top__` | applied as a transform in `tests/parity.rs` (changes the IR of 5 corpus cases); the merge of spectre_spice/inline/80 is in its kf-one-pin-nets override | diverged | 0011 |
| kf-bare-values | kfnetlist b4dc1ec, 8f0a4b4 | a bare word, `{…}` or `'…'` after an R/C/L's nodes (`R1 a b rload`) is the model name | it is the `value` param (an identifier a `reference`, `{…}`/`'…'` an expression) unless a `.model` of the deck has that name, a value precedes it (`R1 a b 1k rpoly`) or `k=v` params follow it; then it is the model | overrides of xyce/inline/12 and xyce/inline/48 (`json` and all 6 `writes`; they also carry kf-spice-writer-dialects) | diverged | 0011 |
| kf-stimuli | kfnetlist 75c630f, b1153f6, b4dc1ec, 6db1e11, 8f0a4b4, 7970b4f, b26cd6b, d5b82b2 | a V/I source's value tokens become a model name and a value; `PULSE(…)` does not parse; Spectre source params stay device params; a Spectre quoted string is an empty value | every V/I source is a `Stimulus` in `Simulation.stimuli` (`stimulus.rs`), named `<module>/<instance>` and pointed to by `properties["stimulus"]`; SPICE writes `DC d AC m p FUNC(args)` or `PWL FILE "path"`, Spectre `dc= mag= phase= type= …` (VACASK in InSpice's forms), VAMS an LRM Annex E primitive; a PWL file is stored POSIX; an unknown SPICE source function is `Error::Validation`; a Spectre quoted string is kept as raw text | no corpus case changes (the corpus has no V/I source; pinned by `tests/kf_stimuli.rs`) | diverged | 0011 |
| kf-vams-sources | kfnetlist 650763b, 8f0a4b4 (the parts that need no #35 grammar) | a Verilog-A identifier or string param is an empty value and `1M` is milli; a device model instance is written as its primitive and a source as `vsource` | identifier params are `reference`s, strings `raw`; numbers take Verilog-A scale factors both ways (`M` mega); `vpulse`, `isine`, … read as stimuli; `paramset <model> <primitive>` makes `<model>` instances that primitive with `model_name`; the writer names a device by its `model_name` and declares those `paramset`s, writes a source as its Annex E primitive, and writes the circuit's `ground_net` in a module that uses it | overrides of 41 cases (46 `writes`, vams/inline/117 also `json`) | diverged | 0011 |
| kf-spectre-writer-dialects | kfnetlist 5924917, d94dc38 | Spectre writes no `LIB` directive and cannot parse `include … section=`; one dialect | `include "p" section=s [lang=…]` written for `LIB` and parsed back; `properties["dialect"] == "vacask"` lowercases instance names, names the R/C/L value `r`/`c`/`l` and writes `ground 0`; `ground_net` is node `0`; `sim_skip` / `role=source` as in SPICE | no corpus case changes (pinned by `tests/kf_writers.rs`) | diverged | 0011 |
| kf-hdl-nets | kfnetlist 28b80e0, 4c98724 (#35), 650763b, b26cd6b | Verilog-AMS nets are names: a bit-select is a net of its own, a positional pin of a defined module is `p<i>`, a net keeps no discipline, names are never escaped; no SystemVerilog | nets are bits (`hdl.rs`): a bus port is one `Port` with `width` and `properties["range"]`, its bits `in[3]` … `in[0]` the members; instance arrays expand; a positional pin takes the definition's port name; a connection records its net's declared type as `properties["discipline"]`; `HdlOptions` (`defines`, `disciplines`, `pin_domains`) drive `` `ifdef `` and domain propagation up the hierarchy; Verilog-A attributes, nodesets and `pwl_file`; the VAMS writer escapes names with `sv_name` (`\0 `) and writes an open pin `.p()`; `parse_systemverilog` / `write_systemverilog` | applied as a transform in `tests/parity.rs` (the `discipline` property of 18 VAMS cases); the `kf-one-pin-nets` overrides of spice, hspice, xyce and spectre_spice `inline/45` carry its `.gnd(\0 )` VAMS writes | diverged | 0011 |

No upstream defect required reproduction: the port matched upstream on all 357
corpus cases and all 2142 writer outputs before the kfnetlist divergences above.

Status values: `open` (reproduced, awaiting upstream fix), `fixed-upstream`
(upstream fixed it; port tracks the fix), `diverged` (we intentionally differ;
ADR required).

## R5. Coverage ledger

Maps upstream's test surface to Rust corpus cases and flags uncovered behavior.
Rows are added during extraction and updated as the port progresses.

| format | parse cases | writer outputs | status |
|---|---|---|---|---|
| spice | 67 | 402 | all identical after R4 |
| hspice | 78 | 468 | all identical after R4 |
| xyce | 70 | 420 | all identical after R4 |
| spectre | 27 | 162 | all identical after R4 |
| spectre_spice | 74 | 444 | all identical after R4 |
| vams | 41 | 246 | all identical after R4 |
| **total** | **357** | **2142** | **100% identical after R4** |

Protobuf interchange (the decoded oracle protobuf message-equal to the oracle
`json`, both after R4 transforms): **345 / 345** cases that carry a protobuf (the 12
`inline/0` and `inline/1` cases have none); the protobuf round-trip of the parsed IR
holds on the same 345.

"After R4": the expected IR of every case passes through the R4 transforms
(`kf-signal-domains` changes 191 cases, `kf-one-pin-nets` 52, `kf-top-elements`
5, `kf-hdl-nets` 18), and 119 cases compare against `tests/corpus/overrides.json`: 70 IR and 497
writer outputs (`kf-one-pin-nets` 56 cases, `kf-vams-sources` 41,
`kf-expressions` 12, `kf-spice-writer-dialects` 7, `kf-bare-values` 2,
`kf-device-pin-template` 1).

The writer count is each parsed circuit written back in every format. Generated by
`tools/extract_oracle.py`; the live binding check
(`tools/generate_equivalence_docs.py`) reports the same tally.

## Invariants

- No surface/format is "ported" while its R1 requirements are unverified.
- Every intentional deviation in Rust carries `// parity: <id>` resolving to a
  row in R4.
- No corpus case may be deleted to make a test pass; it is removed only with a
  recorded reason.

## Open questions

- **Q1** — Is byte-exact writer output required everywhere, or only where a
  committed golden exists (R1.2)? Affects how much numeric-formatting work is
  owed.
- **Q2** — Which Python error kinds must map to which Rust error kinds for R1.8,
  and do parse errors need position parity?
- **Q3** — Resolved for now: message-equality after decode is the bar; wire
  byte-exactness differs only in map field ordering.
