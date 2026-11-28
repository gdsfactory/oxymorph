//! Source stimuli (`Simulation.stimuli`) in every dialect's native syntax, ported from
//! kfnetlist `stimulus.py`.
//!
//! One `Stimulus` per V/I element, named `"<module>/<instance>"` and pointed to by the
//! element's `properties["stimulus"]`. Waveform args use SPICE semantics with Spectre
//! names (see [`waveform_args`]); each dialect renders and reads them natively:
//!
//! - SPICE: `DC 0.9 AC 1 0 PULSE(0 1.8 1n 10p 10p 1n 2n)` (positional);
//! - Spectre/VACASK: `dc=0.9 mag=1 type=pulse val0=0 ...`;
//! - Verilog-A/AMS: LRM Annex E Table E.1 primitives `vpulse #(.dc(0.9), .val0(0), .td(1n))`.
//!
//! Writers walk the args in [`waveform_args`] order: `Waveform.args` read back from
//! protobuf has no order.

use std::collections::HashMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use oxymorph_schema::paths::posix_file;
use oxymorph_schema::{
    Circuit, Expression, ModuleReference, ParameterValue, Simulation, Stimulus, Waveform,
    WaveformKind,
};
use regex::Regex;

use crate::error::Error;
use crate::expression::{format_expression, parameter_value, to_float, value_text, Dialect};
use crate::helpers::{parse_verilog_number, python_g};

const PULSE_ARGS: &[&str] = &["val0", "val1", "delay", "rise", "fall", "width", "period"];
const SIN_ARGS: &[&str] = &["sinedc", "ampl", "freq", "delay", "theta"];
const EXP_ARGS: &[&str] = &["val0", "val1", "delay", "tau1", "td2", "tau2"];
const SFFM_ARGS: &[&str] = &["sinedc", "ampl", "freq", "modindex", "modfreq"];

/// kind, SPICE function, Spectre type, args in SPICE positional order.
const WAVEFORMS: [(WaveformKind, &str, &str, &[&str]); 5] = [
    (WaveformKind::Pulse, "PULSE", "pulse", PULSE_ARGS),
    (WaveformKind::Sin, "SIN", "sine", SIN_ARGS),
    (WaveformKind::Exp, "EXP", "exp", EXP_ARGS),
    (WaveformKind::Sffm, "SFFM", "fm", SFFM_ARGS),
    (WaveformKind::Pwl, "PWL", "pwl", &[]),
];

/// A missing positional arg before a given one (SPICE's default for all but
/// val0/val1/width/period, which nyancad never leaves out).
const GAP: &str = "0";

/// InSpice leaves these out of a VACASK source when 0.
const VACASK_DROP_ZERO: [&str; 2] = ["delay", "theta"];

/// `(SPICE function, Spectre type, args in SPICE positional order)` of `kind`.
pub fn waveform_args(
    kind: WaveformKind,
) -> Option<(&'static str, &'static str, &'static [&'static str])> {
    for (waveform_kind, function, spectre_type, names) in WAVEFORMS {
        if waveform_kind == kind {
            return Some((function, spectre_type, names));
        }
    }
    None
}

/// The positional args of `kind`; none for PWL (points) and an unspecified kind.
fn arg_names(kind: WaveformKind) -> &'static [&'static str] {
    match waveform_args(kind) {
        Some((_, _, names)) => names,
        None => &[],
    }
}

fn kind_by_function(function: &str) -> Option<WaveformKind> {
    let upper = function.to_uppercase();
    WAVEFORMS
        .iter()
        .find(|waveform| waveform.1 == upper)
        .map(|waveform| waveform.0)
}

fn kind_by_type(spectre_type: &str) -> Option<WaveformKind> {
    WAVEFORMS
        .iter()
        .find(|waveform| waveform.2 == spectre_type)
        .map(|waveform| waveform.0)
}

/// Verilog-AMS LRM Annex E Table E.1: the primitive's shape and its names for ours.
fn primitive_names(kind: WaveformKind) -> (&'static str, &'static [(&'static str, &'static str)]) {
    match kind {
        WaveformKind::Pulse => ("pulse", &[("delay", "td")]),
        WaveformKind::Sin => (
            "sine",
            &[("sinedc", "offset"), ("delay", "td"), ("theta", "damp")],
        ),
        WaveformKind::Exp => (
            "exp",
            &[
                ("delay", "td0"),
                ("tau1", "tau0"),
                ("td2", "td1"),
                ("tau2", "tau1"),
            ],
        ),
        WaveformKind::Sffm => (
            "sine",
            &[
                ("sinedc", "offset"),
                ("modindex", "fmmodindex"),
                ("modfreq", "fmmodfreq"),
            ],
        ),
        WaveformKind::Pwl => ("pwl", &[]),
        WaveformKind::Unspecified => ("sine", &[]),
    }
}

/// Cadence Spectre's own names for some args (theirs, ours): sine `damp`, exp rise
/// delay `td1`.
fn spectre_aliases(kind: WaveformKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        WaveformKind::Sin => &[("damp", "theta")],
        WaveformKind::Exp => &[("td1", "delay")],
        _ => &[],
    }
}

fn function_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)^([A-Za-z]+)\((.*)\)$").unwrap())
}

/// `PULSE (0 1)`: a space before the parenthesis.
fn spaced_function_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(PULSE|SIN|EXP|SFFM|PWL)\s+\(").unwrap())
}

/// A waveform file in the stored form; `""` and `.` are no file (kfnetlist `Waveform._file`).
fn stored_file(path: &str) -> Option<String> {
    let stored = posix_file(path);
    if stored == "." {
        None
    } else {
        Some(stored)
    }
}

/// The stimulus `reference` points to, None when it has none.
pub fn stimulus_of<'a>(circuit: &'a Circuit, reference: &ModuleReference) -> Option<&'a Stimulus> {
    let name = reference.properties.get("stimulus")?;
    if name.is_empty() {
        return None;
    }
    let simulation = circuit.simulation.as_ref()?;
    simulation
        .stimuli
        .iter()
        .find(|stimulus| &stimulus.name == name)
}

/// A circuit's stimuli by name, the first of two with one name, as [`stimulus_of`] finds
/// them. A writer builds it once, so finding each instance's stimulus does not scan them all.
pub struct StimulusIndex<'a> {
    by_name: HashMap<&'a str, &'a Stimulus>,
}

impl<'a> StimulusIndex<'a> {
    pub fn new(circuit: &'a Circuit) -> Self {
        let mut by_name = HashMap::new();
        if let Some(simulation) = &circuit.simulation {
            for stimulus in &simulation.stimuli {
                by_name.entry(stimulus.name.as_str()).or_insert(stimulus);
            }
        }
        Self { by_name }
    }

    /// [`stimulus_of`] through the index.
    pub fn of(&self, reference: &ModuleReference) -> Option<&'a Stimulus> {
        let name = reference.properties.get("stimulus")?;
        if name.is_empty() {
            return None;
        }
        self.by_name.get(name.as_str()).copied()
    }
}

/// Name `stimulus` after `reference` (an element of `module_name`) and point
/// `reference` to it; parsers collect the result and [`add_stimulus`] it at the end.
pub fn name_stimulus(
    module_name: &str,
    reference: &mut ModuleReference,
    mut stimulus: Stimulus,
) -> Stimulus {
    stimulus.name = format!("{module_name}/{}", reference.name);
    reference
        .properties
        .insert("stimulus".to_string(), stimulus.name.clone());
    stimulus
}

/// Add a named stimulus to `circuit`, replacing one of the same name.
pub fn add_stimulus(circuit: &mut Circuit, stimulus: Stimulus) {
    add_stimuli(circuit, vec![stimulus]);
}

/// [`add_stimulus`] of each of `stimuli` in turn, in one pass: an existing stimulus is
/// dropped when one of `stimuli` has its name, and one of `stimuli` is added when no later
/// one has its name.
pub fn add_stimuli(circuit: &mut Circuit, stimuli: Vec<Stimulus>) {
    if stimuli.is_empty() {
        return; // no `simulation` made for nothing
    }
    let mut last_index: HashMap<String, usize> = HashMap::new();
    for (index, stimulus) in stimuli.iter().enumerate() {
        last_index.insert(stimulus.name.clone(), index);
    }
    let simulation = circuit.simulation.get_or_insert_with(Simulation::default);
    simulation
        .stimuli
        .retain(|existing| !last_index.contains_key(&existing.name));
    for (index, stimulus) in stimuli.into_iter().enumerate() {
        if last_index.get(&stimulus.name) == Some(&index) {
            simulation.stimuli.push(stimulus);
        }
    }
}

/// Record `stimulus` as what drives `reference` (an element of `module_name`).
pub fn attach(
    circuit: &mut Circuit,
    module_name: &str,
    reference: &mut ModuleReference,
    stimulus: Stimulus,
) {
    let named = name_stimulus(module_name, reference, stimulus);
    add_stimulus(circuit, named);
}

fn text(value: &ParameterValue, dialect: Dialect) -> String {
    if let (Some(prefixed_value), Dialect::SystemVerilog) = (&value.prefixed_value, dialect) {
        let literal = Expression {
            literal: Some(prefixed_value.clone()), // no SI suffixes
            ..Default::default()
        };
        return format_expression(&literal, dialect, None, false);
    }
    value_text(value, dialect)
}

/// A positional SPICE value: a param name is wrapped too (`PULSE(0 {vdd})`).
fn spice_value(value: &ParameterValue, dialect: Dialect) -> String {
    let written = value_text(value, dialect);
    let is_reference = value
        .expression
        .as_ref()
        .is_some_and(|expression| expression.reference.is_some());
    if is_reference {
        let (opening, closing) = dialect.wrapping();
        return format!("{opening}{written}{closing}");
    }
    written
}

fn number(value: Option<&ParameterValue>) -> Option<f64> {
    value?.prefixed_value.as_ref().map(to_float)
}

/// The text of a param as written (a quoted string or `[...]` stays raw).
fn plain(value: &ParameterValue) -> String {
    if let Some(raw) = value
        .expression
        .as_ref()
        .and_then(|expression| expression.raw.as_ref())
    {
        return raw.clone();
    }
    text(value, Dialect::Spectre)
}

fn trim_quotes(text: &str) -> &str {
    text.trim_matches(|character| character == '"' || character == '\'')
}

/// A Verilog-A value read from text: `1M` is mega there.
pub fn verilog_value(text: &str) -> ParameterValue {
    parse_verilog_number(text).unwrap_or_else(|| parameter_value(text))
}

// ── SPICE ──────────────────────────────────────────────────────────────

/// `DC d AC m p FUNC(args)`: the positional value of a SPICE V/I line.
pub fn spice_text(stimulus: &Stimulus, dialect: Dialect) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(dc) = &stimulus.dc {
        parts.push("DC".to_string());
        parts.push(spice_value(dc, dialect));
    }
    if stimulus.ac_magnitude.is_some() || stimulus.ac_phase.is_some() {
        let magnitude = stimulus
            .ac_magnitude
            .clone()
            .unwrap_or_else(|| parameter_value("1"));
        parts.push("AC".to_string());
        parts.push(spice_value(&magnitude, dialect));
        if let Some(phase) = &stimulus.ac_phase {
            parts.push(spice_value(phase, dialect));
        }
    }
    let Some(waveform) = &stimulus.waveform else {
        return parts.join(" ");
    };
    // ponytail: an unspecified kind has no SPICE function and is not written
    let Some((function, _, names)) = waveform_args(waveform.kind) else {
        return parts.join(" ");
    };
    if waveform.kind == WaveformKind::Pwl {
        if let Some(file) = &waveform.file {
            parts.push(format!("PWL FILE \"{}\"", posix_file(file)));
            return parts.join(" ");
        }
    }
    let mut args: Vec<String> = Vec::new();
    if waveform.kind == WaveformKind::Pwl {
        for point in &waveform.points {
            args.push(spice_value(point, dialect));
        }
    } else {
        let last = names
            .iter()
            .rposition(|name| waveform.args.contains_key(*name));
        let given = last.map_or(0, |index| index + 1);
        for name in &names[..given] {
            match waveform.args.get(*name) {
                Some(value) => args.push(spice_value(value, dialect)),
                None => args.push(GAP.to_string()),
            }
        }
    }
    parts.push(format!("{function}({})", args.join(" ")));
    parts.join(" ")
}

/// Args of `FUNC(...)`: split on spaces/commas outside braces and quotes.
fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0i64;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    for character in text.chars() {
        if (character == '\'' || character == '"')
            && depth == 0
            && (quote.is_none() || quote == Some(character))
        {
            quote = if quote.is_some() {
                None
            } else {
                Some(character)
            };
        } else if quote.is_none() && (character == '{' || character == '(') {
            depth += 1;
        } else if quote.is_none() && (character == '}' || character == ')') {
            depth -= 1;
        }
        if quote.is_none() && depth == 0 && matches!(character, ' ' | '\t' | ',') {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(character);
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn waveform_from_values(kind: WaveformKind, values: Vec<ParameterValue>) -> Waveform {
    if kind == WaveformKind::Pwl {
        return Waveform {
            kind,
            points: values,
            ..Default::default()
        };
    }
    let names = arg_names(kind);
    let mut args = IndexMap::new();
    for (name, value) in names.iter().zip(values) {
        args.insert(name.to_string(), value);
    }
    Waveform {
        kind,
        args,
        ..Default::default()
    }
}

fn pwl_file_waveform(file: &str) -> Waveform {
    Waveform {
        kind: WaveformKind::Pwl,
        file: stored_file(file),
        ..Default::default()
    }
}

/// The stimulus of a SPICE V/I value (the tokens after the nodes); an unknown source
/// function (`AM(...)`) is an error.
pub fn parse_spice(source_text: &str) -> Result<Stimulus, Error> {
    let mut stimulus = Stimulus::default();
    let unspaced = spaced_function_re().replace_all(source_text, "$1(");
    let tokens = split_args(&unspaced);
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let upper = token.to_uppercase();
        let function = function_re().captures(token);
        let next_upper = tokens.get(index + 1).map(|next| next.to_uppercase());
        if upper == "PWL"
            && next_upper
                .as_ref()
                .is_some_and(|next| next.starts_with("FILE"))
        {
            // `PWL FILE "w.txt"` (Xyce) or `PWL FILE=w.txt`
            let inline = tokens[index + 1]
                .split_once('=')
                .map_or("", |(_, path)| path);
            let path = if inline.is_empty() {
                tokens.get(index + 2).map_or("", String::as_str)
            } else {
                inline
            };
            stimulus.waveform = Some(pwl_file_waveform(trim_quotes(path.trim())));
            index += if inline.is_empty() { 3 } else { 2 };
            continue;
        }
        let known_kind = function
            .as_ref()
            .and_then(|captures| kind_by_function(&captures[1]));
        if let (Some(captures), Some(kind)) = (&function, known_kind) {
            let mut values = Vec::new();
            for arg in split_args(&captures[2]) {
                values.push(parameter_value(&arg));
            }
            stimulus.waveform = Some(waveform_from_values(kind, values));
        } else if upper == "DC" && index + 1 < tokens.len() {
            stimulus.dc = Some(parameter_value(&tokens[index + 1]));
            index += 1;
        } else if upper == "AC" && index + 1 < tokens.len() {
            stimulus.ac_magnitude = Some(parameter_value(&tokens[index + 1]));
            index += 1;
            if let Some(next) = tokens.get(index + 1) {
                let next_upper = next.to_uppercase();
                if !function_re().is_match(next) && next_upper != "DC" && next_upper != "AC" {
                    stimulus.ac_phase = Some(parameter_value(next));
                    index += 1;
                }
            }
        } else if stimulus.dc.is_none() && index == 0 && function.is_none() {
            stimulus.dc = Some(parameter_value(token)); // `V1 a 0 1.8`: the DC value
        } else {
            return Err(Error::Validation(format!(
                "unsupported source value {token:?} in {source_text:?}"
            )));
        }
        index += 1;
    }
    Ok(stimulus)
}

// ── Spectre / VACASK ───────────────────────────────────────────────────

/// `(name, text)` of a Spectre `vsource`/`isource`. VACASK takes InSpice's forms: quoted
/// `type`, no zero `delay`/`theta`, no `rise <= 0`, and `td2` counted from the delay.
pub fn spectre_params(stimulus: &Stimulus, dialect: Dialect) -> Vec<(String, String)> {
    let vacask = dialect == Dialect::Vacask;
    let mut params: Vec<(String, String)> = Vec::new();
    let levels = [
        ("dc", &stimulus.dc),
        ("mag", &stimulus.ac_magnitude),
        ("phase", &stimulus.ac_phase),
    ];
    for (name, value) in levels {
        if let Some(value) = value {
            params.push((name.to_string(), text(value, dialect)));
        }
    }
    let Some(waveform) = &stimulus.waveform else {
        return params;
    };
    let Some((_, spectre_type, names)) = waveform_args(waveform.kind) else {
        return params;
    };
    let type_text = if vacask {
        format!("\"{spectre_type}\"")
    } else {
        spectre_type.to_string()
    };
    params.push(("type".to_string(), type_text));
    if waveform.kind == WaveformKind::Pwl {
        if let Some(file) = &waveform.file {
            params.push(("file".to_string(), format!("\"{}\"", posix_file(file))));
            return params;
        }
        let mut points = Vec::new();
        for point in &waveform.points {
            points.push(text(point, dialect));
        }
        params.push(("wave".to_string(), format!("[{}]", points.join(" "))));
        return params;
    }
    let delay = waveform.args.get("delay");
    for name in names {
        let Some(value) = waveform.args.get(*name) else {
            continue;
        };
        let value_number = number(Some(value));
        if vacask && VACASK_DROP_ZERO.contains(name) && value_number == Some(0.0) {
            continue;
        }
        if vacask && *name == "rise" && value_number.is_some_and(|rise| rise <= 0.0) {
            continue;
        }
        let mut written = text(value, dialect);
        if let (true, "td2", Some(delay)) = (vacask, *name, delay) {
            match (value_number, number(Some(delay))) {
                (Some(td2), Some(delay_number)) => written = python_g(td2 - delay_number),
                // ponytail: an expression stays symbolic
                _ => written = format!("({written})-({})", text(delay, dialect)),
            }
        }
        params.push((name.to_string(), written));
    }
    params
}

/// The stimulus in a Spectre source's params, None when they hold none.
pub fn from_spectre(
    params: &IndexMap<String, ParameterValue>,
    dialect: Dialect,
) -> Option<Stimulus> {
    let mut stimulus = Stimulus {
        dc: params.get("dc").cloned(),
        ac_magnitude: params.get("mag").cloned(),
        ac_phase: params.get("phase").cloned(),
        ..Default::default()
    };
    if let Some(type_value) = params.get("type") {
        let spectre_type = text(type_value, Dialect::Spectre);
        let kind = kind_by_type(&trim_quotes(spectre_type.trim()).to_lowercase());
        match kind {
            Some(WaveformKind::Pwl) if params.contains_key("file") => {
                let file = plain(&params["file"]);
                stimulus.waveform = Some(pwl_file_waveform(trim_quotes(&file)));
            }
            Some(WaveformKind::Pwl) => {
                let wave = params.get("wave").map(plain).unwrap_or_default();
                let wave = wave.trim();
                let wave = wave.strip_prefix('[').unwrap_or(wave);
                let wave = wave.strip_suffix(']').unwrap_or(wave);
                let mut points = Vec::new();
                for arg in split_args(wave) {
                    points.push(parameter_value(&arg));
                }
                stimulus.waveform = Some(waveform_from_values(WaveformKind::Pwl, points));
            }
            Some(kind) => {
                let mut args = IndexMap::new();
                for (theirs, ours) in spectre_aliases(kind) {
                    if let Some(value) = params.get(*theirs) {
                        args.insert(ours.to_string(), value.clone());
                    }
                }
                let names = arg_names(kind);
                for name in names {
                    if let Some(value) = params.get(*name) {
                        args.insert(name.to_string(), value.clone());
                    }
                }
                if dialect == Dialect::Vacask {
                    let td2 = number(args.get("td2"));
                    let delay = number(args.get("delay"));
                    if let (Some(td2), Some(delay)) = (td2, delay) {
                        args.insert("td2".to_string(), parameter_value(&python_g(td2 + delay)));
                    }
                }
                stimulus.waveform = Some(Waveform {
                    kind,
                    args,
                    ..Default::default()
                });
            }
            None => {}
        }
    }
    if stimulus == Stimulus::default() {
        return None;
    }
    Some(stimulus)
}

/// The params of a Spectre source [`from_spectre`] reads into `stimulus`; an unknown
/// `type` (`prbs`) and its params stay device params.
pub fn spectre_keys(stimulus: Option<&Stimulus>) -> Vec<&'static str> {
    let Some(stimulus) = stimulus else {
        return Vec::new();
    };
    let mut keys = vec!["dc", "mag", "phase"];
    if let Some(waveform) = &stimulus.waveform {
        keys.extend(["type", "wave", "file"]);
        if let Some((_, _, names)) = waveform_args(waveform.kind) {
            keys.extend(names);
        }
        for (theirs, _) in spectre_aliases(waveform.kind) {
            keys.push(*theirs);
        }
    }
    keys
}

/// The Spectre/primitive params `stimulus` was read from (not device params).
pub fn stimulus_keys(stimulus: Option<&Stimulus>) -> Vec<&'static str> {
    let Some(stimulus) = stimulus else {
        return Vec::new();
    };
    let mut keys = vec!["dc", "mag", "phase", "type", "wave"];
    if let Some(waveform) = &stimulus.waveform {
        if let Some((_, _, names)) = waveform_args(waveform.kind) {
            keys.extend(names);
        }
        for (_, theirs) in primitive_names(waveform.kind).1 {
            keys.push(*theirs);
        }
    }
    keys
}

// ── Verilog-A/AMS primitives (LRM Annex E Table E.1) ───────────────────

/// `(master, [(param, text)])` of the Table E.1 primitive for `stimulus`. A DC/AC
/// source is a `vsine` with only `dc`/`mag`/`phase` (LRM E.2.2.3).
pub fn primitive(
    stimulus: &Stimulus,
    current: bool,
    dialect: Dialect,
) -> (String, Vec<(String, String)>) {
    let mut params: Vec<(String, String)> = Vec::new();
    let levels = [
        ("dc", &stimulus.dc),
        ("mag", &stimulus.ac_magnitude),
        ("phase", &stimulus.ac_phase),
    ];
    for (name, value) in levels {
        if let Some(value) = value {
            params.push((name.to_string(), text(value, dialect)));
        }
    }
    let letter = if current { "i" } else { "v" };
    let Some(waveform) = &stimulus.waveform else {
        return (format!("{letter}sine"), params);
    };
    let (shape, renames) = primitive_names(waveform.kind);
    if waveform.kind == WaveformKind::Pwl && waveform.file.is_some() {
        // the file rides in a `(* pwl_file *)` attribute (see primitive_file)
    } else if waveform.kind == WaveformKind::Pwl {
        let mut points = Vec::new();
        for point in &waveform.points {
            points.push(text(point, dialect));
        }
        params.push(("wave".to_string(), format!("'{{{}}}", points.join(", "))));
    } else if let Some((_, _, names)) = waveform_args(waveform.kind) {
        for name in names {
            let Some(value) = waveform.args.get(*name) else {
                continue;
            };
            let theirs = renames
                .iter()
                .find(|(ours, _)| ours == name)
                .map_or(*name, |(_, theirs)| *theirs);
            params.push((theirs.to_string(), text(value, dialect)));
        }
    }
    (format!("{letter}{shape}"), params)
}

/// The PWL file of `stimulus` (POSIX), which Verilog-A says in a `pwl_file` attribute:
/// Annex E's `vpwl` takes only an inline `wave`.
pub fn primitive_file(stimulus: &Stimulus) -> Option<String> {
    let waveform = stimulus.waveform.as_ref()?;
    if waveform.kind != WaveformKind::Pwl {
        return None;
    }
    waveform.file.as_deref().map(posix_file)
}

/// The stimulus of a Table E.1 source primitive, None for any other master.
pub fn from_primitive(
    master: &str,
    params: &IndexMap<String, ParameterValue>,
    file: Option<&str>,
) -> Option<Stimulus> {
    let master = master.to_lowercase();
    let mut characters = master.chars();
    let letter = characters.next()?;
    let shape = characters.as_str();
    if !matches!(letter, 'v' | 'i') || !matches!(shape, "pulse" | "sine" | "exp" | "pwl") {
        return None;
    }
    let mut stimulus = Stimulus {
        dc: params.get("dc").cloned(),
        ac_magnitude: params.get("mag").cloned(),
        ac_phase: params.get("phase").cloned(),
        ..Default::default()
    };
    if shape == "pwl" {
        if let Some(file) = file.filter(|file| !file.is_empty()) {
            stimulus.waveform = Some(pwl_file_waveform(file));
            return Some(stimulus);
        }
        let wave = params
            .get("wave")
            .map(|value| text(value, Dialect::Vams))
            .unwrap_or_default();
        let wave = wave.trim();
        let wave = wave.strip_prefix('\'').unwrap_or(wave);
        let wave = wave.strip_prefix('{').unwrap_or(wave);
        let wave = wave.strip_suffix('}').unwrap_or(wave);
        if !wave.is_empty() {
            let mut points = Vec::new();
            for arg in split_args(wave) {
                points.push(verilog_value(&arg));
            }
            stimulus.waveform = Some(waveform_from_values(WaveformKind::Pwl, points));
        }
        return Some(stimulus);
    }
    let kind = match shape {
        "sine" if params.keys().any(|name| name.starts_with("fmmod")) => WaveformKind::Sffm,
        "sine" => WaveformKind::Sin,
        "pulse" => WaveformKind::Pulse,
        _ => WaveformKind::Exp,
    };
    let renames = primitive_names(kind).1;
    let names = arg_names(kind);
    let mut args = IndexMap::new();
    for (name, value) in params {
        let ours = renames
            .iter()
            .find(|(_, theirs)| theirs == name)
            .map_or(name.as_str(), |(ours, _)| *ours);
        if names.contains(&ours) {
            args.insert(ours.to_string(), value.clone());
        }
    }
    if !args.is_empty() {
        stimulus.waveform = Some(Waveform {
            kind,
            args,
            ..Default::default()
        });
    }
    Some(stimulus)
}
