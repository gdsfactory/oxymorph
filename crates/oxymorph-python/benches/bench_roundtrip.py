"""Time a gfpex-sized circuit through the object model: build, `write_spice`, `to_json`.

10 000 resistors on a ladder of nodes plus 10 000 lone-terminal self-connections: 10k module references and 30k
connections. Run after `just test-python` (which builds the release package):

    uv run --no-sync python crates/oxymorph-python/benches/bench_roundtrip.py
"""

import copy
import time

import oxymorph

REFERENCE_COUNT = 10_000


def build_circuit() -> oxymorph.Circuit:
    top = oxymorph.module("__top__")
    for index in range(REFERENCE_COUNT):
        name = f"R{index}"
        top.add(oxymorph.module_reference(name, "resistor", value=float(index + 1), properties={"net": "vdd"}))
        top.connect(f"n{index}", (name, "p"), (f"R{index - 1}" if index else name, "n" if index else "p"))
        top.connect(f"m{index}", (name, "n"), (name, "n"))
        top.connections.append(oxymorph.connection(f"x{index}", (name, "p"), (name, "p")))
    resistor = oxymorph.ext_module(
        "resistor", ports=[oxymorph.port("p"), oxymorph.port("n")], properties={"spice_prefix": "R"}
    )
    return oxymorph.Circuit(name="ladder", top_module="__top__").add_ext(resistor).add(top)


def timed(label: str, function):
    start = time.perf_counter()
    result = function()
    print(f"{label:<12} {time.perf_counter() - start:8.3f} s")
    return result


def main() -> None:
    circuit = timed("build", build_circuit)
    top = circuit.modules[0]
    assert len(top.module_references) == REFERENCE_COUNT and len(top.connections) == 3 * REFERENCE_COUNT
    spice = timed("write_spice", lambda: oxymorph.write_spice(circuit))
    text = timed("to_json", circuit.to_json)
    timed("from_json", lambda: oxymorph.Circuit.from_json(text))
    timed("deepcopy", lambda: copy.deepcopy(circuit))
    timed("==", lambda: circuit == circuit)
    print(f"{len(spice.splitlines())} SPICE lines, {len(text) / 1e6:.1f} MB of JSON")


if __name__ == "__main__":
    main()
