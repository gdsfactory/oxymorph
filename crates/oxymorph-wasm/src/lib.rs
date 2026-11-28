//! WebAssembly bindings for oxymorph (wasm-bindgen).
//!
//! Thin layer: parse text into the IR and write it back out, for the docs
//! playground and any browser/Node consumer.

use wasm_bindgen::prelude::*;

use oxymorph_netlist::formats::{parse_dispatch, write_dispatch};

/// The supported netlist formats.
#[wasm_bindgen]
pub fn formats() -> Vec<String> {
    vec![
        "spice".to_string(),
        "hspice".to_string(),
        "xyce".to_string(),
        "spectre".to_string(),
        "spectre_spice".to_string(),
        "vams".to_string(),
        "systemverilog".to_string(),
        "gsch".to_string(),
    ]
}

/// Parse `text` in `in_format` and write it back in `out_format`.
#[wasm_bindgen]
pub fn convert(text: &str, in_format: &str, out_format: &str) -> Result<String, JsValue> {
    let circuit = parse_dispatch(in_format, text).map_err(|e| JsValue::from_str(&e.to_string()))?;
    write_dispatch(out_format, &circuit).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Parse `text` and return the IR as JSON.
#[wasm_bindgen]
pub fn to_json(text: &str, in_format: &str) -> Result<String, JsValue> {
    let circuit = parse_dispatch(in_format, text).map_err(|e| JsValue::from_str(&e.to_string()))?;
    circuit
        .to_json()
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// The crate version.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
