//! gdsfactory+ `.gsch` schematics (nyancir JSON), ported from kfnetlist
//! `netlist/gsch/{parser,sim,writer}.py` at e596bb5 (#37).
//!
//! A `.gsch` file is a flat `{key: entry}` JSON map, so there is no Lark grammar. The
//! parser resolves devices the way nyancad's netlister does (`sim`) and stashes what the IR
//! has no field for, so [`write_gsch`] can restore it.
//!
//! The stash is JSON text written as Python's `json.dumps` writes it, and JSON values are
//! compared and printed with Python's semantics (`python_*` below), so the IR and the
//! written file are the ones kfnetlist produces.

pub mod parser;
pub mod sim;
pub mod writer;

use serde_json::{Map, Value};

pub use parser::parse_gsch;
#[cfg(not(target_arch = "wasm32"))]
pub use parser::read_gsch;
pub use writer::write_gsch;

/// How [`parse_gsch`] / [`read_gsch`] resolve devices.
#[derive(Clone, Debug, PartialEq)]
pub struct GschOptions {
    /// nyanlib `{"models:<fqn>": {...}}` (`models.nyanlib`); an inline top-level `"models"`
    /// key of the document is merged under it. `read_gsch` defaults to the project's
    /// `build/models.nyanlib`, else `models.nyanlib`.
    pub models: Option<Value>,
    /// `ngspice`, `xyce` or `vacask` (any case).
    pub sim: String,
    /// Preferred library sections; each library takes the first of these it has, else its
    /// first section.
    pub corners: Vec<String>,
    /// The circuit's module name (`parse_gsch` only); `__top__`, which writers write flat,
    /// when unset.
    pub name: Option<String>,
}

impl Default for GschOptions {
    fn default() -> Self {
        Self {
            models: None,
            sim: "ngspice".to_string(),
            corners: Vec::new(),
            name: None,
        }
    }
}

/// A JSON object, in its document order.
pub type JsonObject = Map<String, Value>;

/// The object `value` is, None for anything else.
pub(crate) fn object(value: Option<&Value>) -> Option<&JsonObject> {
    value.and_then(Value::as_object)
}

/// Python's truthiness of an optional JSON value (missing is `None`).
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
    }
}

/// Python's `a or b or …`: the first truthy value, else the last one.
pub(crate) fn first_truthy<'a>(values: &[Option<&'a Value>]) -> Option<&'a Value> {
    for value in values {
        if truthy(*value) {
            return *value;
        }
    }
    values.last().copied().flatten()
}

/// `field` of an entry when it is a string ("" otherwise).
pub(crate) fn string_field<'a>(entry: &'a JsonObject, field: &str) -> &'a str {
    entry.get(field).and_then(Value::as_str).unwrap_or("")
}

/// Python's `repr` of a float: shortest round-trip digits, `1e-06`, `1e+16`, `2.0`.
pub(crate) fn python_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if (-4..16).contains(&exponent) {
        let fixed = format!("{value}");
        if fixed.contains('.') {
            fixed
        } else {
            format!("{fixed}.0")
        }
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    }
}

/// Python's `repr` of a JSON number (`int` or `float`).
fn python_number(number: &serde_json::Number) -> String {
    if number.is_f64() {
        python_float(number.as_f64().unwrap_or_default())
    } else {
        number.to_string()
    }
}

/// Python's `str` of a JSON value as `json.loads` returns it (`True`, `None`, `0.15`, …).
pub(crate) fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => python_number(number),
        Value::String(text) => text.clone(),
        // ponytail: Python prints a list/dict with single quotes; JSON is close enough for
        // the prop values nyanlib expressions read (numbers and strings)
        Value::Array(_) | Value::Object(_) => python_json(value, None),
    }
}

/// Python's `repr` of a string: single quotes unless it holds one and no double quote.
pub(crate) fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut quoted = String::from(quote);
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            _ if character == quote => {
                quoted.push('\\');
                quoted.push(character);
            }
            _ => quoted.push(character),
        }
    }
    quoted.push(quote);
    quoted
}

/// Python's `==` on JSON values: `1 == 1.0`, `True == 1`.
pub(crate) fn python_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(_) | Value::Bool(_), Value::Number(_) | Value::Bool(_)) => {
            numeric(left) == numeric(right)
        }
        (Value::Array(left_items), Value::Array(right_items)) => {
            left_items.len() == right_items.len()
                && left_items
                    .iter()
                    .zip(right_items)
                    .all(|(left_item, right_item)| python_eq(left_item, right_item))
        }
        (Value::Object(left_fields), Value::Object(right_fields)) => {
            left_fields.len() == right_fields.len()
                && left_fields.iter().all(|(key, left_value)| {
                    right_fields
                        .get(key)
                        .is_some_and(|right_value| python_eq(left_value, right_value))
                })
        }
        _ => left == right,
    }
}

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

/// Python's `json.dumps(value)` (`indent=None`: `", "` / `": "` separators) or
/// `json.dumps(value, indent=n)`; non-ASCII as `\uXXXX`.
pub(crate) fn python_json(value: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    write_json(value, indent, 0, &mut out);
    out
}

fn write_json(value: &Value, indent: Option<usize>, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&python_number(number)),
        Value::String(text) => out.push_str(&crate::helpers::json_string(text)),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                write_separator(index, indent, depth + 1, out);
                write_json(item, indent, depth + 1, out);
            }
            write_closing(indent, depth, out);
            out.push(']');
        }
        Value::Object(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (index, (key, item)) in fields.iter().enumerate() {
                write_separator(index, indent, depth + 1, out);
                out.push_str(&crate::helpers::json_string(key));
                out.push_str(": ");
                write_json(item, indent, depth + 1, out);
            }
            write_closing(indent, depth, out);
            out.push('}');
        }
    }
}

fn write_separator(index: usize, indent: Option<usize>, depth: usize, out: &mut String) {
    match indent {
        Some(width) => {
            if index > 0 {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(width * depth));
        }
        None => {
            if index > 0 {
                out.push_str(", ");
            }
        }
    }
}

fn write_closing(indent: Option<usize>, depth: usize, out: &mut String) {
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * depth));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_json_matches_json_dumps() {
        let value = json!({"a": [1, 2.5, 1e-6, 1e16, "é"], "b": {}, "c": [], "d": null});
        assert_eq!(
            python_json(&value, None),
            "{\"a\": [1, 2.5, 1e-06, 1e+16, \"\\u00e9\"], \"b\": {}, \"c\": [], \"d\": null}"
        );
        assert_eq!(
            python_json(&json!({"x": [1, {"y": true}]}), Some(2)),
            "{\n  \"x\": [\n    1,\n    {\n      \"y\": true\n    }\n  ]\n}"
        );
        assert_eq!(python_float(20.0), "20.0");
        assert_eq!(python_float(0.15), "0.15");
        assert_eq!(python_float(1e15), "1000000000000000.0");
        assert_eq!(python_repr("width * 1e-6"), "'width * 1e-6'");
        assert!(python_eq(&json!(1), &json!(1.0)));
        assert!(!python_eq(&json!("1"), &json!(1)));
    }
}
