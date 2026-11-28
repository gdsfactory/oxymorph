# oxymorph

[![CI](https://github.com/gdsfactory/oxymorph/actions/workflows/ci.yml/badge.svg)](https://github.com/gdsfactory/oxymorph/actions/workflows/ci.yml)
[![Tests](https://github.com/gdsfactory/oxymorph/actions/workflows/tests.yml/badge.svg)](https://github.com/gdsfactory/oxymorph/actions/workflows/tests.yml)
[![Release](https://github.com/gdsfactory/oxymorph/actions/workflows/release.yml/badge.svg)](https://github.com/gdsfactory/oxymorph/actions/workflows/release.yml)

A Rust port of [morphnet](https://github.com/das-dias/morphnet): bidirectional
SPICE-family and Verilog-AMS netlist parsing through a shared intermediate
representation.

oxymorph is **provably equivalent** to upstream morphnet: upstream Python is the
oracle, its verdict is captured into a committed corpus, and the parity test
replays that corpus offline. It parses and writes all six formats identically to
upstream across the corpus (357 parse cases, 2142 writer outputs) — see the
generated [Equivalence](https://gdsfactory.github.io/oxymorph/equivalence/) page.

## Formats

| Flavor | Parse | Write |
|---|---|---|
| SPICE | `parse_spice` | `write_spice` |
| HSPICE | `parse_hspice` | `write_hspice` |
| Xyce | `parse_xyce` | `write_xyce` |
| Spectre | `parse_spectre` | `write_spectre` |
| Spectre-SPICE | `parse_spectre_spice` | `write_spectre_spice` |
| Verilog-AMS | `parse_vams` | `write_vams` |

## Install

```bash
pip install oxymorph
```

## Quick start

```python
import oxymorph

circuit = oxymorph.parse_spectre("""\
subckt rc_lowpass (in_ out gnd)
R1 (in_ out) resistor value=1k
C1 (out gnd) capacitor value=100p
ends rc_lowpass
""")

print(oxymorph.write_vams(circuit))
```

```verilog
module rc_lowpass (in_, out, gnd);
  inout in_, out, gnd;
  resistor #(.value(1k)) R1 (.p(in_), .n(out));
  capacitor #(.value(100p)) C1 (.p(out), .n(gnd));
endmodule
```

There is also a programmatic builder that mirrors upstream's `morphnet._api`:

```python
import oxymorph

inv = oxymorph.module("inverter", parameters=["l", "w"])
inv.add_terminal("inp", direction=oxymorph.PortDirection.INPUT)
inv.add_terminal("out", direction=oxymorph.PortDirection.OUTPUT)
inv.add(oxymorph.module_reference("mn", "nfet", l=100e-9, w=500e-9))
inv.connect("net_out", ("mn", "drain"), ("", "out"))

circuit = oxymorph.circuit("ring")
circuit.add(inv)
```

## WebAssembly

The same core compiles to `wasm32-unknown-unknown`
([`crates/oxymorph-wasm`](crates/oxymorph-wasm)) and powers the
[playground](https://gdsfactory.github.io/oxymorph/playground/): paste a netlist,
pick input and output flavors, convert in the browser.

```js
import init, { convert } from "oxymorph_wasm.js";
await init();
convert("subckt rc (a b)\nR1 (a b) resistor value=1k\nends rc\n", "spectre", "vams");
```

Rebuild the playground module with `tools/build_playground.sh`.

## Documentation

Published with [zensical](https://zensical.org) to GitHub Pages:

- [Home](https://gdsfactory.github.io/oxymorph/)
- [Examples](https://gdsfactory.github.io/oxymorph/examples/)
- [Equivalence](https://gdsfactory.github.io/oxymorph/equivalence/)
- [Playground](https://gdsfactory.github.io/oxymorph/playground/)
- [API reference](https://gdsfactory.github.io/oxymorph/api/)

## Repository layout

```
crates/oxymorph-schema    # the IR (serde types, builder) — no parsing
crates/oxymorph-netlist   # grammars, parsers, writers
crates/oxymorph           # Rust facade
crates/oxymorph-python    # PyO3 module `oxymorph`
crates/oxymorph-wasm      # wasm-bindgen module
docs/                     # zensical site
tests/corpus/             # committed oracle corpus
vendor/morphnet           # upstream (read-only submodule)
```

This project is decision-driven and spec-driven: see [`decisions/`](decisions/)
(why), [`specs/`](specs/) (what), and [`ROADMAP.md`](ROADMAP.md) (what's next).

## Development

```bash
git submodule update --init --recursive
cargo test --workspace          # parity corpus + tests, no Python needed
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
maturin develop                 # build the Python module
```

## License

Apache-2.0.
