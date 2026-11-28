//! Simulation view of `.gsch` devices, with nyancad's netlister semantics, ported from
//! kfnetlist `netlist/gsch/sim.py`.
//!
//! Mirrors `nyancad/netlist.py` in gdsfactoryplus (`_add_nyancad_element`,
//! `_select_model_entry`, `_select_corner`, `_eval_params`, `_add_source` and the library
//! includes of `NyanCircuit`), resolved into the IR fields the SPICE and Spectre writers
//! read: module name, pin order, `model_name`, expression params and LIB/INCLUDE directives.

use std::sync::OnceLock;

use indexmap::IndexMap;
use oxymorph_schema::{
    Directive, DirectiveKind, Macro, ParameterValue, Stimulus, Waveform, WaveformKind,
};
use regex::{Captures, Regex};
use serde_json::Value;

use super::{first_truthy, object, python_str, string_field, truthy, JsonObject};
use crate::error::Error;
use crate::expression::{parameter_value, to_float, value_text, Dialect};
use crate::helpers::{
    device_port_template, device_prefix_to_module, is_si_number, one_line, parse_si_number,
};

/// The net nyancad writes as node 0 (case-sensitive, like nyancad).
pub const GROUND: &str = "GND";

/// Simulator -> the language of its native model entries.
const SIM_LANGUAGES: [(&str, &str); 3] = [
    ("ngspice", "spice"),
    ("xyce", "spice"),
    ("vacask", "spectre"),
];

/// Mosaic built-in symbol -> device letter.
fn builtin_letter(kind: &str) -> &'static str {
    match kind {
        "resistor" => "R",
        "capacitor" => "C",
        "inductor" => "L",
        "diode" => "D",
        "vsource" => "V",
        "isource" => "I",
        "nmos" | "pmos" => "M",
        "npn" | "pnp" => "Q",
        _ => "",
    }
}

/// The primary value prop of an R/C/L.
fn value_prop(letter: &str) -> Option<&'static str> {
    match letter {
        "R" => Some("resistance"),
        "C" => Some("capacitance"),
        "L" => Some("inductance"),
        _ => None,
    }
}

/// InSpice's VACASK master of an R/C/L (its `sp_*` OSDI model).
fn vacask_master(letter: &str) -> Option<&'static str> {
    match letter {
        "R" => Some("sp_resistor"),
        "C" => Some("sp_capacitor"),
        "L" => Some("sp_inductor"),
        _ => None,
    }
}

/// The InSpice default of a `tran` field.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    /// InSpice fails without it.
    Required,
    /// May be left off at the end.
    Omit,
    Default(&'static str),
}

/// `(prop, stimulus arg, InSpice default)` of a `tran` variant in SPICE positional order;
/// nyancad's `_TRAN_PARAM_MAP` names the props.
fn tran_fields(variant: &str) -> &'static [(&'static str, &'static str, Slot)] {
    use Slot::{Default, Omit, Required};
    match variant {
        "sin" => &[
            ("offset", "sinedc", Default("0")),
            ("amplitude", "ampl", Default("1")),
            ("frequency", "freq", Default("50")),
            ("delay", "delay", Default("0")),
            ("damping", "theta", Default("0")),
        ],
        "pulse" => &[
            ("initial", "val0", Required),
            ("pulsed", "val1", Required),
            ("delay", "delay", Default("0")),
            ("rise", "rise", Default("0")),
            ("fall", "fall", Default("0")),
            ("width", "width", Required),
            ("period", "period", Required),
        ],
        "exp" => &[
            ("initial", "val0", Required),
            ("pulsed", "val1", Required),
            ("rise-delay", "delay", Default("0")),
            ("rise-tau", "tau1", Omit),
            ("fall-delay", "td2", Omit),
            ("fall-tau", "tau2", Omit),
        ],
        "sffm" => &[
            ("offset", "sinedc", Required),
            ("amplitude", "ampl", Required),
            ("carrier-freq", "freq", Required),
            ("mod-index", "modindex", Required),
            ("signal-freq", "modfreq", Required),
        ],
        _ => &[],
    }
}

const WAVEFORM_KINDS: [(&str, WaveformKind); 4] = [
    ("sin", WaveformKind::Sin),
    ("pulse", WaveformKind::Pulse),
    ("exp", WaveformKind::Exp),
    ("sffm", WaveformKind::Sffm),
];

fn waveform_kind(variant: &str) -> Option<WaveformKind> {
    for (name, kind) in WAVEFORM_KINDS {
        if name == variant {
            return Some(kind);
        }
    }
    None
}

fn variant_of(kind: WaveformKind) -> Option<&'static str> {
    for (name, waveform) in WAVEFORM_KINDS {
        if waveform == kind {
            return Some(name);
        }
    }
    None
}

/// `sim` lowercased, or `Error::Validation` when it is no simulator nyancad netlists for.
pub fn check_sim(sim: &str) -> Result<String, Error> {
    let lower = sim.to_lowercase();
    for (name, _) in SIM_LANGUAGES {
        if name == lower {
            return Ok(lower);
        }
    }
    Err(Error::Validation(format!(
        "sim must be one of ['ngspice', 'vacask', 'xyce'], got {}",
        super::python_repr(sim)
    )))
}

fn sim_language(sim: &str) -> &'static str {
    for (name, language) in SIM_LANGUAGES {
        if name == sim {
            return language;
        }
    }
    ""
}

/// Dialect of a SPICE library VACASK loads as foreign (`lang=`), else None.
pub fn foreign_language(entry: &JsonObject, sim: &str) -> Option<String> {
    let implementation = string_field(entry, "implementation").to_lowercase();
    if sim == "vacask"
        && truthy(entry.get("library"))
        && string_field(entry, "language").to_lowercase() == "spice"
        && (implementation == "ngspice" || implementation == "xyce")
    {
        return Some(implementation);
    }
    None
}

/// The model entry of `model_def` for `sim`: exact `implementation`, else the sim's native
/// language, else a foreign SPICE library.
pub fn select_entry<'a>(model_def: &'a JsonObject, sim: &str) -> Option<&'a JsonObject> {
    let mut entries = Vec::new();
    if let Some(Value::Array(items)) = model_def.get("models") {
        for item in items {
            if let Value::Object(entry) = item {
                entries.push(entry);
            }
        }
    }
    for entry in &entries {
        if string_field(entry, "implementation").to_lowercase() == sim {
            return Some(entry);
        }
    }
    for entry in &entries {
        if string_field(entry, "language").to_lowercase() == sim_language(sim) {
            return Some(entry);
        }
    }
    entries
        .into_iter()
        .find(|entry| foreign_language(entry, sim).is_some())
}

/// The first of `corners` (caller order) the library has, else its first section.
fn select_corner(sections: Option<&Value>, corners: &[String]) -> Option<String> {
    let Some(Value::Array(sections)) = sections else {
        return None;
    };
    let first = sections.first()?;
    for corner in corners {
        for section in sections {
            if section.as_str() == Some(corner.as_str()) {
                return Some(corner.clone());
            }
        }
    }
    Some(python_str(first))
}

/// A library path on one deck line: control characters as spaces, inner backslashes kept
/// (Windows paths), trailing ones (a line continuation, or an escaped closing quote)
/// dropped.
fn path_on_one_line(path: &str) -> String {
    let mut line = String::with_capacity(path.len());
    for character in path.chars() {
        line.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    line.trim_end_matches('\\').to_string()
}

/// The `.lib` / `.include` of a model entry; paths are passed through.
pub fn library(entry: &JsonObject, sim: &str, corners: &[String]) -> Option<Directive> {
    let path = entry.get("library");
    if !truthy(path) {
        return None;
    }
    // a deck writes the value on one line: nyanlib text cannot start lines of its own
    let path = path_on_one_line(&python_str(path?));
    let name = match foreign_language(entry, sim) {
        Some(language) => format!("lang={language}"),
        None => String::new(),
    };
    match select_corner(entry.get("sections"), corners) {
        Some(section) => Some(Directive {
            kind: DirectiveKind::Lib,
            value: format!("{path} {}", one_line(&section)),
            name,
        }),
        None => Some(Directive {
            kind: DirectiveKind::Include,
            value: path,
            name,
        }),
    }
}

/// The inline `code` of a model entry, which nyancad injects as SPICE.
pub fn code_macro(entry: &JsonObject) -> Option<Macro> {
    let code = match entry.get("code") {
        Some(value) if truthy(Some(value)) => python_str(value),
        _ => String::new(),
    };
    let body = code.trim();
    if body.is_empty() {
        return None;
    }
    let language = match entry.get("language") {
        Some(value) if truthy(Some(value)) => python_str(value),
        _ => "spice".to_string(),
    };
    Some(Macro {
        name: entry.get("name").map(python_str).unwrap_or_default(),
        language,
        body: body.to_string(),
        ..Default::default()
    })
}

/// What nyancad substitutes in text (`WORD_RE`): an ASCII identifier.
pub(crate) fn identifier_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?-u:\b)[A-Za-z_][A-Za-z0-9_]*").unwrap())
}

fn valid_identifier_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap())
}

/// nyancad `_eval_params`: a bare name renames a prop; any other expression gets the props
/// it reads substituted (textually, bare: writers wrap expressions per dialect).
fn eval_params(entry_params: &JsonObject, props: &JsonObject) -> IndexMap<String, Value> {
    let mut out = IndexMap::new();
    for (name, expression) in entry_params {
        let expression = python_str(expression);
        let stripped = expression.trim();
        if valid_identifier_re().is_match(stripped) {
            if let Some(value) = props.get(stripped) {
                out.insert(name.clone(), value.clone());
            }
            continue;
        }
        let substitute = |word: &Captures| match props.get(&word[0]) {
            Some(value) => python_str(value),
            None => word[0].to_string(),
        };
        let text = identifier_re().replace_all(&expression, substitute);
        out.insert(name.clone(), Value::String(text.into_owned()));
    }
    out
}

/// The value of a SPICE number (`1k`, `2.5`), None for anything else.
fn number(raw: &Value) -> Option<f64> {
    // ponytail: helpers' SI syntax, not nyancad's `meg*`-prefix leniency
    match raw {
        Value::Number(number) => number.as_f64(),
        Value::String(text) if is_si_number(text.trim()) => parse_si_number(text.trim())
            .ok()
            .map(|value| to_float(&value)),
        _ => None,
    }
}

/// A prop as written (numbers and expressions alike), None when unset.
fn given(raw: Option<&Value>) -> Option<String> {
    match raw {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.is_empty() => None,
        Some(Value::String(text)) => Some(text.trim().to_string()),
        Some(value) => Some(python_str(value)),
    }
}

/// Python's `text or default`.
fn or_default<'a>(text: &'a Option<String>, default: &'a str) -> &'a str {
    match text.as_deref() {
        Some(value) if !value.is_empty() => value,
        _ => default,
    }
}

fn optional_value(raw: Option<&str>) -> Option<ParameterValue> {
    raw.map(parameter_value)
}

/// `(stimulus arg, value)` of a `tran` waveform in SPICE positional order. Each value keeps
/// its own slot: a missing required field is an error, as it is for nyancad/InSpice, and an
/// optional one may only be left off at the end.
fn waveform(
    device: &str,
    variant: &str,
    tran: &JsonObject,
) -> Result<Vec<(String, String)>, Error> {
    let mut out = Vec::new();
    let mut gap: Option<&str> = None;
    for (prop, arg, slot) in tran_fields(variant) {
        let value = given(tran.get(*prop));
        if value.is_none() && *slot == Slot::Required {
            return Err(Error::Validation(format!(
                "{device}: tran {variant} needs '{prop}'"
            )));
        }
        if let (Some(_), Some(missing)) = (&value, gap) {
            return Err(Error::Validation(format!(
                "{device}: tran {variant} has '{prop}' without '{missing}'"
            )));
        }
        match (value, slot) {
            (Some(text), _) => out.push((arg.to_string(), text)),
            (None, Slot::Omit) => {
                gap = gap.or(Some(prop));
            }
            (None, Slot::Default(default)) => out.push((arg.to_string(), default.to_string())),
            (None, Slot::Required) => {}
        }
    }
    Ok(out)
}

/// What drives a V/I source: its `dc`/`ac` props and `tran` waveform. Like nyancad, a sine
/// or pulse source always has a DC value (0) and a sine an AC one (1).
pub fn source(device: &str, props: &JsonObject) -> Result<Stimulus, Error> {
    let dc = given(props.get("dc"));
    let ac = given(props.get("ac"));
    let tran = object(props.get("tran"));
    let variant = tran
        .and_then(|fields| fields.get("type"))
        .and_then(Value::as_str);
    let (Some(tran), Some(variant), Some(kind)) = (tran, variant, variant.and_then(waveform_kind))
    else {
        return Ok(Stimulus {
            dc: optional_value(dc.as_deref()),
            ac_magnitude: optional_value(ac.as_deref()),
            ..Default::default()
        });
    };
    let mut args = IndexMap::new();
    for (name, raw) in waveform(device, variant, tran)? {
        args.insert(name, parameter_value(&raw));
    }
    let dc = if variant == "sin" || variant == "pulse" {
        Some(parameter_value(or_default(&dc, "0")))
    } else {
        None
    };
    let ac_magnitude = if variant == "sin" {
        Some(parameter_value(or_default(&ac, "1")))
    } else {
        optional_value(ac.as_deref())
    };
    Ok(Stimulus {
        dc,
        ac_magnitude,
        waveform: Some(Waveform {
            kind,
            args,
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// `props` with `stimulus` written back to `dc`/`ac`/`tran`; unchanged while `stimulus` is
/// still what they describe. A value the props left to its default (a sine's DC 0, a
/// pulse's rise 0, …) stays left out while it is the default.
pub fn source_props(
    device: &str,
    stimulus: &Stimulus,
    props: &JsonObject,
) -> Result<JsonObject, Error> {
    if stimulus.ac_phase.is_some() {
        return Err(Error::Validation(format!(
            "{device}: a .gsch source has no AC phase"
        )));
    }
    let unnamed = Stimulus {
        name: String::new(),
        ..stimulus.clone()
    };
    if unnamed == source(device, props)? {
        return Ok(props.clone());
    }
    let mut out = JsonObject::new();
    for (key, value) in props {
        if !matches!(key.as_str(), "dc" | "ac" | "tran") {
            out.insert(key.clone(), value.clone());
        }
    }
    let empty = JsonObject::new();
    let old = object(props.get("tran")).unwrap_or(&empty);
    for (prop, value, implied) in [
        ("dc", &stimulus.dc, "0"),
        ("ac", &stimulus.ac_magnitude, "1"),
    ] {
        let Some(value) = value else {
            continue;
        };
        if !props.contains_key(prop) && *value == parameter_value(implied) {
            continue;
        }
        out.insert(
            prop.to_string(),
            Value::String(value_text(value, Dialect::Gsch)),
        );
    }
    if let Some(shape) = &stimulus.waveform {
        let Some(variant) = variant_of(shape.kind) else {
            let kind = format!("{:?}", shape.kind).to_uppercase();
            return Err(Error::Validation(format!(
                "{device}: a .gsch source has no {kind} waveform"
            )));
        };
        let mut tran = JsonObject::new();
        tran.insert("type".to_string(), Value::String(variant.to_string()));
        for (prop, arg, slot) in tran_fields(variant) {
            let Some(value) = shape.args.get(*arg) else {
                continue;
            };
            if let Slot::Default(default) = slot {
                if !old.contains_key(*prop) && *value == parameter_value(default) {
                    continue;
                }
            }
            tran.insert(
                prop.to_string(),
                Value::String(value_text(value, Dialect::Gsch)),
            );
        }
        out.insert("tran".to_string(), Value::Object(tran));
    }
    Ok(out)
}

/// How one `.gsch` device is simulated.
#[derive(Clone, Debug, Default)]
pub struct Element {
    /// IR module name.
    pub module_name: String,
    /// Pin order of that module.
    pub ports: Vec<String>,
    pub params: IndexMap<String, Value>,
    pub model_name: String,
    /// SPICE letter of the module.
    pub prefix: String,
    /// Device pin (lowercase) -> IR pin.
    pub rename: IndexMap<String, String>,
    /// Pins tied to ground when open.
    pub ground: Vec<String>,
    /// A model with `params`: the props its expressions read …
    pub sources: JsonObject,
    /// … and those expressions.
    pub expressions: IndexMap<String, String>,
    /// Model prop defaults the entry's props do not set.
    pub defaults: JsonObject,
    /// A V/I source's.
    pub stimulus: Option<Stimulus>,
}

impl Element {
    /// The IR pin of the device pin `name`.
    pub fn pin(&self, name: &str) -> String {
        self.rename
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }
}

/// A device primitive; `given` device pins map onto its template in order.
fn primitive(
    letter: &str,
    given: &[String],
    params: IndexMap<String, Value>,
    model: &str,
) -> Element {
    let template = device_port_template(letter).unwrap_or(&[]);
    let mut ports: Vec<String> = template.iter().map(|pin| pin.to_string()).collect();
    for extra in given.iter().skip(template.len()) {
        ports.push(extra.clone());
    }
    let mut rename = IndexMap::new();
    for (device_pin, port) in given.iter().zip(&ports) {
        rename.insert(device_pin.to_lowercase(), port.clone());
    }
    Element {
        module_name: device_prefix_to_module(letter).unwrap_or("").to_string(),
        ports,
        params,
        model_name: model.to_string(),
        prefix: letter.to_string(),
        rename,
        ..Default::default()
    }
}

fn subckt(name: &str, ports: &[String], params: IndexMap<String, Value>) -> Element {
    let mut rename = IndexMap::new();
    for port in ports {
        rename.insert(port.to_lowercase(), port.clone());
    }
    Element {
        module_name: name.to_string(),
        ports: ports.to_vec(),
        params,
        prefix: "X".to_string(),
        rename,
        ..Default::default()
    }
}

/// The nyanlib entry key of an instance's model (`models:<model>`, Python's f-string).
pub fn model_key(entry: &JsonObject) -> String {
    format!(
        "models:{}",
        python_str(entry.get("model").unwrap_or(&Value::Null))
    )
}

/// The model definition of an instance, when nyanlib has one (an object).
pub fn model_definition<'a>(entry: &JsonObject, models: &'a JsonObject) -> Option<&'a JsonObject> {
    object(models.get(&model_key(entry)))
}

/// The prop defaults of a nyanlib model.
pub fn model_defaults(model_def: &JsonObject) -> JsonObject {
    let mut defaults = JsonObject::new();
    if let Some(Value::Array(props)) = model_def.get("props") {
        for prop in props {
            let Some(prop) = prop.as_object() else {
                continue;
            };
            let default = prop.get("default");
            if truthy(prop.get("name")) && !matches!(default, None | Some(Value::Null)) {
                defaults.insert(
                    python_str(&prop["name"]),
                    default.cloned().unwrap_or_default(),
                );
            }
        }
    }
    defaults
}

/// The `name`s of a list of `{name: …}` objects (nyanlib ports).
pub fn port_names(ports: Option<&Value>) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(Value::Array(items)) = ports {
        for item in items {
            if let Some(name) = item.as_object().and_then(|port| port.get("name")) {
                names.push(python_str(name));
            }
        }
    }
    names
}

/// The device nyancad netlists for `entry`, or None when it netlists none.
pub fn element(
    entry: &JsonObject,
    models: &JsonObject,
    sim: &str,
    key: &str,
) -> Result<Option<Element>, Error> {
    let label = match first_truthy(&[entry.get("name"), None]) {
        Some(name) => python_str(name),
        None => key.to_string(),
    }; // nyancad's name
    let Some(mut device) = simulated_element(entry, models, sim, &label)? else {
        return Ok(None);
    };
    let Some(model_def) = model_definition(entry, models) else {
        return Ok(Some(vacask(device, sim)));
    };
    let empty = JsonObject::new();
    let props = object(entry.get("props")).unwrap_or(&empty);
    let defaults = model_defaults(model_def);
    for (name, value) in &defaults {
        if !props.contains_key(name) {
            device.defaults.insert(name.clone(), value.clone());
        }
    }
    if let Some(chosen) = select_entry(model_def, sim) {
        if let Some(expressions) = object(chosen.get("params")).filter(|fields| !fields.is_empty())
        {
            let mut sources = defaults.clone();
            for (name, value) in props {
                sources.insert(name.clone(), value.clone());
            }
            device.sources = sources;
            for (name, expression) in expressions {
                device
                    .expressions
                    .insert(name.clone(), python_str(expression));
            }
        }
    }
    Ok(Some(vacask(device, sim)))
}

fn simulated_element(
    entry: &JsonObject,
    models: &JsonObject,
    sim: &str,
    label: &str,
) -> Result<Option<Element>, Error> {
    let Some(nets) = object(entry.get("nets")).filter(|nets| !nets.is_empty()) else {
        return Ok(None); // nyancad reads Mosaic `nets` only
    };
    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
    let model_def = model_definition(entry, models);
    let empty = JsonObject::new();
    let entry_props = object(entry.get("props")).unwrap_or(&empty);
    let mut props: IndexMap<String, Value> = entry_props.clone().into_iter().collect();
    let mut chosen = None;
    let mut name = Value::Null;
    if let Some(model_def) = model_def {
        let defaults = model_defaults(model_def);
        name = first_truthy(&[model_def.get("name"), entry.get("model")])
            .cloned()
            .unwrap_or_default();
        chosen = select_entry(model_def, sim);
        if let Some(selected) = chosen {
            if truthy(selected.get("name")) {
                name = selected["name"].clone();
            }
            props.insert("model".to_string(), name.clone());
            if let Some(expressions) =
                object(selected.get("params")).filter(|fields| !fields.is_empty())
            {
                let mut given_props = defaults;
                for (prop, value) in entry_props {
                    given_props.insert(prop.clone(), value.clone());
                }
                props = eval_params(expressions, &given_props);
            }
        } else {
            let mut merged: IndexMap<String, Value> = defaults.into_iter().collect();
            for (prop, value) in props {
                merged.insert(prop, value);
            }
            props = merged;
            props.entry("model".to_string()).or_insert(name.clone());
        }
    }
    props.retain(|_, value| !value.is_null() && value.as_str() != Some(""));
    let mut model_ports = port_names(model_def.and_then(|model| model.get("ports")));
    model_ports.sort();

    let letter = chosen
        .map(|selected| string_field(selected, "spice-type").to_uppercase())
        .unwrap_or_default();
    if let (Some(selected), false) = (chosen, letter.is_empty()) {
        let given_ports = match selected.get("port-order") {
            Some(order) if truthy(Some(order)) => port_list(order),
            _ => model_ports,
        };
        let model = python_str(&props.shift_remove("model").unwrap_or(name));
        if matches!(letter.as_str(), "D" | "M" | "Q") {
            let model = if foreign_language(selected, sim).is_some() {
                format!("m_{}", model.to_lowercase())
            } else {
                model
            };
            return Ok(Some(primitive(&letter, &given_ports, props, &model)));
        }
        if device_port_template(&letter).is_some() {
            return Ok(Some(primitive(&letter, &given_ports, props, "")));
        }
        // ponytail: unknown letters are called like X/SUBCKT (nyancad: X)
        return Ok(Some(subckt(&model, &given_ports, props)));
    }

    let letter = builtin_letter(kind);
    if let Some(prop) = value_prop(letter) {
        let mut params = IndexMap::new();
        if let Some(value) = props.get(prop) {
            params.insert("value".to_string(), value.clone());
        }
        return Ok(Some(primitive(letter, &pins(&["P", "N"]), params, "")));
    }
    if letter == "V" || letter == "I" {
        let mut device = primitive(letter, &pins(&["P", "N"]), IndexMap::new(), "");
        let props_object: JsonObject = props.into_iter().collect();
        device.stimulus = Some(source(label, &props_object)?);
        return Ok(Some(device));
    }
    if !letter.is_empty() {
        let model = props
            .shift_remove("model")
            .map(|value| python_str(&value))
            .unwrap_or_default();
        let template = match letter {
            "D" => pins(&["P", "N"]),
            "M" => pins(&["D", "G", "S", "B"]),
            _ => pins(&["C", "B", "E"]),
        };
        let mut device = primitive(letter, &template, props, &model);
        let has_bulk = nets.keys().any(|pin_name| pin_name.to_lowercase() == "b");
        if letter == "M" && !has_bulk {
            device.ground = vec!["b".to_string()];
        }
        return Ok(Some(device));
    }
    if model_def.is_some() {
        let model = python_str(&props.shift_remove("model").unwrap_or(name));
        return Ok(Some(subckt(&model, &model_ports, props)));
    }
    Ok(None)
}

fn pins(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// A JSON list of pin names.
fn port_list(order: &Value) -> Vec<String> {
    let mut found = Vec::new();
    if let Value::Array(items) = order {
        for item in items {
            found.push(python_str(item));
        }
    }
    found
}

/// InSpice's VACASK forms: R/C/L are its `sp_*` OSDI models, and D/M/Q take their
/// multiplier `m` as `$mfactor`, left out when 1.
fn vacask(mut device: Element, sim: &str) -> Element {
    if sim != "vacask" {
        return device;
    }
    if let Some(master) = vacask_master(&device.prefix) {
        if device.model_name.is_empty() {
            device.model_name = master.to_string();
        }
    }
    // ponytail: `$mfactor` is computed from `m` here, so a later edit of a model's source
    // `m` does not reach it; make it an expression if that edit matters.
    if matches!(device.prefix.as_str(), "D" | "M" | "Q") {
        if let Some(multiplier) = device.params.shift_remove("m") {
            if number(&multiplier) != Some(1.0) {
                device.params.insert("$mfactor".to_string(), multiplier);
            }
        }
    }
    device
}
