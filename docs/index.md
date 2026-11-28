# oxymorph

**oxymorph** is a Rust port of the
[morphnet](https://github.com/das-dias/morphnet) netlist parser. It
bidirectionally parses SPICE-family and Verilog-AMS netlists through a shared
intermediate representation, and ships as:

- a pure-Rust workspace (`oxymorph-schema`, `oxymorph-netlist`, `oxymorph`), and
- a thin [PyO3](https://pyo3.rs) binding exposed as the Python module `oxymorph`.

The port tracks upstream morphnet and is verified **bug-for-bug equivalent**
against it on a committed oracle corpus — see [Equivalence](equivalence.md).

## Install

```bash
pip install oxymorph
```

Building from source requires Rust and [maturin](https://maturin.rs):

```bash
git clone https://github.com/gdsfactory/oxymorph
cd oxymorph
git submodule update --init --recursive
uv venv && uv pip install maturin
maturin develop
```

## Quick start

Parse a Spectre netlist and write Verilog-AMS:

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

## Formats

| Format | Parse | Write |
|---|---|---|
| `spice` | `parse_spice` | `write_spice` |
| `hspice` | `parse_hspice` | `write_hspice` |
| `xyce` | `parse_xyce` | `write_xyce` |
| `spectre` | `parse_spectre` | `write_spectre` |
| `spectre_spice` | `parse_spectre_spice` | `write_spectre_spice` |
| `vams` | `parse_vams` | `write_vams` |

## The intermediate representation

Every parser produces a `Circuit`, and every writer consumes one. The IR mirrors
upstream's Pydantic models field-for-field, so `to_json()` / `to_yaml()` output
is interchangeable with upstream. See the [API reference](api.md).
