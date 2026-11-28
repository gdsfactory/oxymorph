#!/usr/bin/env python3
"""Compare oxymorph with kfnetlist (the second oracle, `decisions/0011`) on the corpus inputs.

Run from the repo root; uv installs kfnetlist `e596bb5` in a throwaway environment, and
oxymorph's outputs come from `cargo run --release -p oxymorph-netlist --example dump_corpus`:

    uv run --isolated \\
        --with "kfnetlist @ git+https://github.com/gdsfactory/kfnetlist@e596bb5c7e925bb94fd1750052b25941a4874af6" \\
        python tools/compare_kfnetlist.py [--corpus tests/corpus/cases.json] [--changed-only] [--case FORMAT/NAME ...]

For every case it parses the input with both libraries, then prints the IR paths and
the writer outputs where oxymorph differs from kfnetlist. kfnetlist names are mapped
to oxymorph's (`decisions/0011`, plus the unported #29 rename `terminals` /
`terminal_name` -> `ports` / `port_name`); a field kfnetlist omits equals a null;
connections compare as a multiset, so a connection only one side has is reported once.

`--changed-only` keeps the cases where oxymorph no longer matches the original corpus
(`json` or any `writes[fmt]`): the cases a new R4 row must explain. Exit status 1 when
any reported case differs. Not run by `cargo test`.
"""

from __future__ import annotations

import argparse
import difflib
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

import kfnetlist.morphnet.netlist as kfnetlist_netlist

FORMATS = ["spice", "hspice", "xyce", "spectre", "spectre_spice", "vams", "systemverilog"]
KFNETLIST_TO_OXYMORPH_KEYS = {
    "top_circuit": "top_module",
    "circuits": "modules",
    "ext_circuits": "ext_modules",
    "subcircuits": "module_references",
    "circuit_name": "module_name",
    "terminals": "ports",
    "terminal_name": "port_name",
}
REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CORPUS = REPO_ROOT / "tests" / "corpus" / "cases.json"


def map_kfnetlist_names(value):
    if isinstance(value, dict):
        return {KFNETLIST_TO_OXYMORPH_KEYS.get(key, key): map_kfnetlist_names(item) for key, item in value.items()}
    if isinstance(value, list):
        return [map_kfnetlist_names(item) for item in value]
    return value


def canonical(value) -> str:
    return json.dumps(value, sort_keys=True)


def find_ir_differences(ours, theirs, path: str = "") -> list[str]:
    if isinstance(ours, dict) and isinstance(theirs, dict):
        differences = []
        for key in sorted(set(ours) | set(theirs)):
            differences += find_ir_differences(ours.get(key), theirs.get(key), f"{path}.{key}")
        return differences
    if isinstance(ours, list) and isinstance(theirs, list):
        if path.endswith(".connections"):
            return find_connection_differences(ours, theirs, path)
        if len(ours) != len(theirs):
            return [f"{path}: {len(ours)} items in oxymorph, {len(theirs)} in kfnetlist"]
        differences = []
        for index, (our_item, their_item) in enumerate(zip(ours, theirs)):
            differences += find_ir_differences(our_item, their_item, f"{path}[{index}]")
        return differences
    if ours != theirs:
        return [f"{path}: oxymorph {json.dumps(ours)[:80]} != kfnetlist {json.dumps(theirs)[:80]}"]
    return []


def find_connection_differences(ours: list, theirs: list, path: str) -> list[str]:
    our_connections = Counter(canonical(connection) for connection in ours)
    their_connections = Counter(canonical(connection) for connection in theirs)
    differences = []
    for connection in sorted((our_connections - their_connections).elements()):
        differences.append(f"{path}: only in oxymorph {connection}")
    for connection in sorted((their_connections - our_connections).elements()):
        differences.append(f"{path}: only in kfnetlist {connection}")
    return differences


def read_oxymorph_outputs(corpus: Path) -> list[dict]:
    command = ["cargo", "run", "-q", "--release", "-p", "oxymorph-netlist", "--example", "dump_corpus", "--", str(corpus)]
    completed = subprocess.run(command, cwd=REPO_ROOT, check=True, capture_output=True, text=True)
    return json.loads(completed.stdout)


def read_kfnetlist_outputs(circuit_text: str, format_name: str) -> tuple[dict, dict[str, str]]:
    circuit = getattr(kfnetlist_netlist, f"parse_{format_name}")(circuit_text)
    writes = {writer: getattr(kfnetlist_netlist, f"write_{writer}")(circuit) for writer in FORMATS}
    return json.loads(circuit.to_json()), writes


def drop_nulls(value):
    if isinstance(value, dict):
        return {key: drop_nulls(item) for key, item in value.items() if item is not None}
    if isinstance(value, list):
        return [drop_nulls(item) for item in value]
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--changed-only", action="store_true")
    parser.add_argument("--case", action="append", default=[], help="FORMAT/NAME; repeatable")
    arguments = parser.parse_args()

    cases = json.loads(arguments.corpus.read_text())
    oxymorph_outputs = read_oxymorph_outputs(arguments.corpus.resolve())
    reported = differing = 0
    for case, oxymorph_output in zip(cases, oxymorph_outputs):
        case_key = f"{case['format']}/{case['name']}"
        if arguments.case and case_key not in arguments.case:
            continue
        if "error" in oxymorph_output:
            reported += 1
            differing += 1
            print(f"== {case_key}: oxymorph failed: {oxymorph_output['error']}")
            continue
        our_ir, our_writes = oxymorph_output["json"], oxymorph_output["writes"]
        if arguments.changed_only:
            changed_writes = [writer for writer, text in case["writes"].items() if our_writes[writer] != text]
            if our_ir == case["json"] and not changed_writes:
                continue
        reported += 1
        try:
            their_ir, their_writes = read_kfnetlist_outputs(case["input"], case["format"])
        except Exception as error:
            differing += 1
            print(f"== {case_key}: kfnetlist failed: {type(error).__name__}: {error}")
            continue
        differences = find_ir_differences(drop_nulls(our_ir), drop_nulls(map_kfnetlist_names(their_ir)))
        for writer in FORMATS:
            if our_writes[writer] != their_writes[writer]:
                diff = difflib.unified_diff(
                    our_writes[writer].splitlines(),
                    their_writes[writer].splitlines(),
                    "oxymorph",
                    "kfnetlist",
                    lineterm="",
                    n=1,
                )
                differences.append(f"write[{writer}]:\n" + "\n".join(diff))
        if differences:
            differing += 1
            print(f"== {case_key}")
            for difference in differences:
                print(f"  {difference}")
    print(f"{reported} cases compared, {differing} differ from kfnetlist")
    return 1 if differing else 0


if __name__ == "__main__":
    sys.exit(main())
