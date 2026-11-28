//! Print oxymorph's IR and writer outputs for every case of a corpus file as a JSON
//! list of `{"json": <IR>, "writes": {<format>: <text>}}`, in corpus order.
//! Used by `tools/compare_kfnetlist.py`:
//! `cargo run --release -p oxymorph-netlist --example dump_corpus -- tests/corpus/cases.json`.

use oxymorph_netlist::{
    parse_dispatch, write_hspice, write_spectre, write_spectre_spice, write_spice,
    write_systemverilog, write_vams, write_xyce,
};
use oxymorph_schema::Circuit;

type Writer = fn(&Circuit) -> String;

const WRITERS: [(&str, Writer); 7] = [
    ("spice", write_spice),
    ("hspice", write_hspice),
    ("xyce", write_xyce),
    ("spectre", write_spectre),
    ("spectre_spice", write_spectre_spice),
    ("vams", write_vams),
    ("systemverilog", write_systemverilog),
];

fn dump_case(case: &serde_json::Value) -> serde_json::Value {
    let format = case["format"].as_str().expect("case has a format");
    let input = case["input"].as_str().expect("case has an input");
    let circuit = match parse_dispatch(format, input) {
        Ok(circuit) => circuit,
        Err(error) => return serde_json::json!({ "error": error.to_string() }),
    };
    let mut writes = serde_json::Map::new();
    for (writer_name, writer) in WRITERS {
        writes.insert(writer_name.to_string(), writer(&circuit).into());
    }
    let json: serde_json::Value = serde_json::from_str(&circuit.to_json().unwrap()).unwrap();
    serde_json::json!({ "json": json, "writes": writes })
}

fn main() {
    let corpus_path = std::env::args()
        .nth(1)
        .expect("usage: dump_corpus <cases.json>");
    let corpus_text = std::fs::read_to_string(&corpus_path).expect("corpus is readable");
    let cases: Vec<serde_json::Value> = serde_json::from_str(&corpus_text).expect("corpus is JSON");
    let mut outputs = Vec::new();
    for case in &cases {
        outputs.push(dump_case(case));
    }
    println!("{}", serde_json::Value::Array(outputs));
}
