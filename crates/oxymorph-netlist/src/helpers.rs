//! Shared helpers ported from `value_utils.py`, `net_utils.py` and
//! `simulation_data.py`. Kept behaviourally identical to upstream.

use std::collections::HashMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use oxymorph_schema::{
    AnalysisKind, Circuit, Connection, Expression, ExternalModule, Macro, Module, ModuleReference,
    OutputRequestKind, Parameter, ParameterValue, PortReference, PrefixedValue, SignalDomain,
    Siprefix,
};
use regex::Regex;

use crate::expression::{format_expression, Dialect, Resolve};

// ---------------------------------------------------------------------------
// SI suffix <-> SIPrefix
// ---------------------------------------------------------------------------

pub fn si_suffix_to_prefix(suffix: &str) -> Option<Siprefix> {
    Some(match suffix {
        "a" => Siprefix::Atto,
        "f" => Siprefix::Femto,
        "p" => Siprefix::Pico,
        "n" => Siprefix::Nano,
        "u" => Siprefix::Micro,
        "m" => Siprefix::Milli,
        "k" => Siprefix::Kilo,
        "x" => Siprefix::Mega,
        "meg" => Siprefix::Mega,
        "g" => Siprefix::Giga,
        "t" => Siprefix::Tera,
        _ => return None,
    })
}

pub fn prefix_to_si_suffix(prefix: Siprefix) -> &'static str {
    match prefix {
        Siprefix::Atto => "a",
        Siprefix::Femto => "f",
        Siprefix::Pico => "p",
        Siprefix::Nano => "n",
        Siprefix::Micro => "u",
        Siprefix::Milli => "m",
        Siprefix::Kilo => "k",
        Siprefix::Mega => "meg",
        Siprefix::Giga => "g",
        Siprefix::Tera => "t",
        _ => "",
    }
}

pub const MIL_SCALE: f64 = 25.4e-6;

// ---------------------------------------------------------------------------
// Device port templates
// ---------------------------------------------------------------------------

pub fn device_port_template(prefix: &str) -> Option<&'static [&'static str]> {
    Some(match prefix {
        "R" | "C" | "L" | "D" | "V" | "I" => &["p", "n"],
        "Q" => &["c", "b", "e"],
        "M" => &["d", "g", "s", "b"],
        _ => return None,
    })
}

pub fn device_prefix_to_module(prefix: &str) -> Option<&'static str> {
    Some(match prefix {
        "R" => "resistor",
        "C" => "capacitor",
        "L" => "inductor",
        "D" => "diode",
        "Q" => "bjt",
        "M" => "mosfet",
        "V" => "vsource",
        "I" => "isource",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Instance parameters as simulation writers see them
// ---------------------------------------------------------------------------

/// Instances flagged `sim_skip` are kept in the IR but not simulated.
pub fn is_skipped(reference: &ModuleReference) -> bool {
    reference.properties.get("sim_skip").map(String::as_str) == Some("1")
}

/// Written value of a parameter: its expression printed for `dialect` (raw text wrapped
/// only when `named`; references through `resolve`), else its default as an SI number.
/// None when there is nothing to write (no value, or empty raw text).
pub fn param_text(
    parameter: &Parameter,
    dialect: Dialect,
    named: bool,
    resolve: Option<Resolve>,
) -> Option<String> {
    let value = parameter.default_value.as_ref()?;
    match &value.expression {
        Some(expression) if expression.raw.as_deref() == Some("") => None,
        Some(expression) => Some(format_expression(expression, dialect, resolve, named)),
        None => Some(match &value.prefixed_value {
            Some(prefixed_value) => format_si_value(prefixed_value),
            None => "0".to_string(),
        }),
    }
}

/// `name=value` for every parameter with something to write.
pub fn named_params(parameters: &[Parameter], dialect: Dialect) -> Vec<String> {
    let mut assignments = Vec::new();
    for parameter in parameters {
        if let Some(text) = param_text(parameter, dialect, true, None) {
            assignments.push(format!("{}={text}", parameter.name));
        }
    }
    assignments
}

/// Modules and external modules by name, shared by every instance line of a write.
pub struct Lookups<'a> {
    pub ext_by_name: IndexMap<&'a str, &'a ExternalModule>,
    pub mod_by_name: IndexMap<&'a str, &'a Module>,
}

impl<'a> Lookups<'a> {
    pub fn of(circuit: &'a Circuit) -> Self {
        let mut ext_by_name = IndexMap::new();
        for ext_module in &circuit.ext_modules {
            ext_by_name.insert(ext_module.name.as_str(), ext_module);
        }
        let mut mod_by_name = IndexMap::new();
        for module in &circuit.modules {
            mod_by_name.insert(module.name.as_str(), module);
        }
        Self {
            ext_by_name,
            mod_by_name,
        }
    }
}

/// The source params of an instance (`properties["role"] == "source"`, e.g. a `.gsch`
/// `width`): what its model expressions (`w = width * 1e-6`) read, not device params.
pub struct SourceParams<'a> {
    values: IndexMap<&'a str, &'a ParameterValue>,
}

impl SourceParams<'_> {
    /// A reference to a source param printed as that param's value.
    pub fn resolve(&self, name: &str) -> Option<Expression> {
        let value = self.values.get(name)?;
        if let Some(expression) = &value.expression {
            return Some(expression.clone());
        }
        value
            .prefixed_value
            .as_ref()
            .map(|prefixed_value| Expression {
                literal: Some(prefixed_value.clone()),
                ..Default::default()
            })
    }
}

fn is_source(parameter: &Parameter) -> bool {
    parameter.properties.get("role").map(String::as_str) == Some("source")
}

/// The params of `reference` a simulator sees, and its source params.
pub fn sim_params(reference: &ModuleReference) -> (Vec<&Parameter>, SourceParams<'_>) {
    let mut shown = Vec::new();
    let mut values = IndexMap::new();
    for parameter in &reference.parameters {
        if !is_source(parameter) {
            shown.push(parameter);
        } else if let Some(value) = &parameter.default_value {
            values.insert(parameter.name.as_str(), value);
        }
    }
    (shown, SourceParams { values })
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// The macro languages a SPICE deck takes verbatim.
pub const SPICE_LANGUAGES: [&str; 4] = ["spice", "ngspice", "xyce", "hspice"];

/// A macro language a writer accepts, and the `(before, after)` lines it wraps the body in.
pub type MacroLanguage<'a> = (&'a str, Option<(&'a str, &'a str)>);

fn accepted<'a>(language: &str, accept: &[MacroLanguage<'a>]) -> Option<MacroLanguage<'a>> {
    let language = language.to_lowercase();
    for entry in accept {
        if entry.0 == language {
            return Some(*entry);
        }
    }
    None
}

/// `text` on one deck line: CR, LF, every other control character and `\\` (Spectre's and
/// some SPICE dialects' line continuation) become a space, so untrusted metadata echoed
/// into a generated deck can neither start a line of its own nor swallow the next one.
pub fn one_line(text: &str) -> String {
    let mut line = String::with_capacity(text.len());
    for character in text.chars() {
        line.push(if character.is_control() || character == '\\' {
            ' '
        } else {
            character
        });
    }
    line
}

/// The bodies of the macros whose `language` is in `accept`, verbatim (between the lines it
/// maps to, if any); any other is a `skipped macro <name> (<language>)` comment, never a
/// silent drop (kfnetlist also warns). Bodies are raw simulator code, written verbatim
/// except for NUL characters; the comment's name and language are kept on one line.
pub fn macro_lines(macros: &[Macro], accept: &[MacroLanguage], comment: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for code in macros {
        let Some((_, wrapping)) = accepted(&code.language, accept) else {
            lines.push(format!(
                "{comment} skipped macro {} ({})",
                one_line(&code.name),
                one_line(&code.language)
            ));
            continue;
        };
        if let Some((before, _)) = wrapping {
            lines.push(before.to_string());
        }
        for line in code.body.lines() {
            lines.push(line.replace('\0', "")); // a NUL ends a C string in a simulator
        }
        if let Some((_, after)) = wrapping {
            lines.push(after.to_string());
        }
    }
    lines
}

/// Every SPICE language, unwrapped: what the SPICE writers accept.
pub fn spice_macro_languages() -> Vec<MacroLanguage<'static>> {
    let mut languages = Vec::new();
    for language in SPICE_LANGUAGES {
        languages.push((language, None));
    }
    languages
}

// ---------------------------------------------------------------------------
// SI number parsing / formatting
// ---------------------------------------------------------------------------

fn si_number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^([+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?)(meg|mil|[afpnumkgtx])?$")
            .unwrap()
    })
}

pub fn parse_si_number(token: &str) -> Result<PrefixedValue, String> {
    let caps = si_number_re()
        .captures(token)
        .ok_or_else(|| format!("Not a valid SI number: {token:?}"))?;
    let numeric_str = caps.get(1).unwrap().as_str();
    let suffix = caps.get(2).map(|m| m.as_str());
    let value: f64 = numeric_str
        .parse()
        .map_err(|_| format!("Not a valid SI number: {token:?}"))?;
    match suffix {
        None => Ok(PrefixedValue {
            double_value: value,
            prefix: Siprefix::Unspecified,
        }),
        Some(s) => {
            let lower = s.to_lowercase();
            if lower == "mil" {
                return Ok(PrefixedValue {
                    double_value: value * MIL_SCALE,
                    prefix: Siprefix::Unspecified,
                });
            }
            match si_suffix_to_prefix(&lower) {
                Some(prefix) => Ok(PrefixedValue {
                    double_value: value,
                    prefix,
                }),
                None => Err(format!("Unknown SI suffix: {s:?}")),
            }
        }
    }
}

// Verilog-A scale factors (LRM 2.6.2): case matters, `M` is mega and `m` milli.
fn verilog_scale(suffix: &str) -> Siprefix {
    match suffix {
        "T" => Siprefix::Tera,
        "G" => Siprefix::Giga,
        "M" => Siprefix::Mega,
        "K" | "k" => Siprefix::Kilo,
        "m" => Siprefix::Milli,
        "u" => Siprefix::Micro,
        "n" => Siprefix::Nano,
        "p" => Siprefix::Pico,
        "f" => Siprefix::Femto,
        "a" => Siprefix::Atto,
        _ => Siprefix::Unspecified,
    }
}

fn verilog_number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^([+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?)([TGMKkmunpfa])?$").unwrap()
    })
}

/// A Verilog-A number (`1M` = 1e6, `1m` = 1e-3); None for anything else.
pub fn parse_verilog_number(token: &str) -> Option<ParameterValue> {
    let captures = verilog_number_re().captures(token)?;
    let double_value: f64 = captures[1].parse().ok()?;
    let prefix = captures.get(2).map_or(Siprefix::Unspecified, |suffix| {
        verilog_scale(suffix.as_str())
    });
    Some(ParameterValue::prefixed(double_value, prefix))
}

/// `value` with a Verilog-A scale factor: mega is `M`, not SPICE's `meg`.
pub fn format_verilog_value(value: &PrefixedValue) -> String {
    let text = format_si_value(value);
    match text.strip_suffix("meg") {
        Some(mantissa) => format!("{mantissa}M"),
        None => text,
    }
}

pub fn parse_parameter_number(token: &str) -> Result<ParameterValue, String> {
    Ok(ParameterValue {
        prefixed_value: Some(parse_si_number(token)?),
        ..Default::default()
    })
}

pub fn is_si_number(token: &str) -> bool {
    si_number_re().is_match(token)
}

/// Emulate Python's `f"{x:g}"` (shortest clean representation, 6 significant
/// digits, scientific notation outside `[-4, 6)`).
pub fn python_g(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let exp = x.abs().log10().floor() as i32;
    if !(-4..6).contains(&exp) {
        let s = format!("{:.*e}", 5, x);
        let (mant, ex) = s.split_once('e').unwrap();
        let mant = trim_zeros(mant);
        let ex: i32 = ex.parse().unwrap();
        format!("{}e{}{:02}", mant, if ex < 0 { '-' } else { '+' }, ex.abs())
    } else {
        let decimals = (5 - exp).max(0) as usize;
        let s = format!("{:.*}", decimals, x);
        trim_zeros(&s)
    }
}

fn trim_zeros(s: &str) -> String {
    if s.contains('.') {
        let t = s.trim_end_matches('0');
        let t = t.trim_end_matches('.');
        t.to_string()
    } else {
        s.to_string()
    }
}

pub fn format_si_value(pv: &PrefixedValue) -> String {
    let num = pv.double_value;
    let num_str = if num.fract() == 0.0 && num.is_finite() {
        format!("{}", num as i64)
    } else {
        python_g(num)
    };
    if pv.prefix == Siprefix::Unspecified {
        return num_str;
    }
    format!("{}{}", num_str, prefix_to_si_suffix(pv.prefix))
}

/// `text` as Python's `json.dumps` writes a string: `\\`, `\"` and control characters
/// escaped, anything outside printable ASCII as `\uXXXX` (a Verilog string literal too).
pub fn json_string(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            ' '..='~' => quoted.push(character),
            _ => {
                let mut units = [0u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    quoted.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    quoted.push('"');
    quoted
}

// ---------------------------------------------------------------------------
// Signal domains
// ---------------------------------------------------------------------------

/// Cell name → pin name → signal domain.
pub type PinDomains = IndexMap<String, IndexMap<String, SignalDomain>>;

/// `o1`, `o2`, ... are waveguide terminals; `e1`, `vdd`, ... are electrical.
pub fn domain_of(port_name: &str) -> SignalDomain {
    let mut characters = port_name.chars();
    if characters.next() == Some('o')
        && characters
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    {
        SignalDomain::Waveguide
    } else {
        SignalDomain::Electrical
    }
}

// ---------------------------------------------------------------------------
// Net maps
// ---------------------------------------------------------------------------

/// Insertion-ordered net map (Python dict semantics).
#[derive(Default)]
pub struct NetMap {
    map: IndexMap<String, Vec<PortReference>>,
}

impl NetMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, net_name: &str, instance_name: &str, port_name: &str) {
        self.map
            .entry(net_name.to_string())
            .or_default()
            .push(PortReference {
                instance_name: instance_name.to_string(),
                port_name: port_name.to_string(),
            });
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Vec<PortReference>)> {
        self.map.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// The node an unconnected pin is written on: its own, never one shared by all.
pub fn open_node(instance_name: &str, pin_name: &str) -> String {
    format!("nc_{instance_name}_{pin_name}")
}

/// For each net with N port references, N-1 connections from the first; a net with one
/// is a source-only connection, except an [`open_node`] (an unconnected pin as a writer
/// spells it), which is no net at all.
pub fn net_map_to_connections(net_map: &NetMap) -> Vec<Connection> {
    let mut connections = Vec::new();
    for (net_name, refs) in net_map.iter() {
        // parity: kf-one-pin-nets
        if let [only] = refs.as_slice() {
            // by instance: a parser that does not know the master's pins reads `p1`
            if !net_name.starts_with(&open_node(&only.instance_name, "")) {
                connections.push(Connection {
                    name: net_name.to_string(),
                    source: Some(only.clone()),
                    domain: domain_of(&only.port_name),
                    ..Default::default()
                });
            }
            continue;
        }
        if refs.is_empty() {
            continue;
        }
        let source = refs[0].clone();
        let domain = net_domain(refs);
        for target in &refs[1..] {
            connections.push(Connection {
                name: net_name.to_string(),
                source: Some(source.clone()),
                target: Some(target.clone()),
                // parity: kf-signal-domains
                domain,
                weight: 0,
                properties: Default::default(),
            });
        }
    }
    connections
}

/// A net touching any waveguide terminal is a waveguide net.
pub fn net_domain(refs: &[PortReference]) -> SignalDomain {
    for port_reference in refs {
        if domain_of(&port_reference.port_name) == SignalDomain::Waveguide {
            return SignalDomain::Waveguide;
        }
    }
    SignalDomain::Electrical
}

/// `(instance, port) -> net`; a net named `ground` (when not empty) is node `0`.
pub fn connections_to_instance_nets(
    connections: &[Connection],
    ground: &str,
) -> IndexMap<(String, String), String> {
    let mut mapping = IndexMap::new();
    for conn in connections {
        let net_name = if !ground.is_empty() && conn.name == ground {
            "0".to_string()
        } else {
            conn.name.clone()
        };
        if let Some(s) = &conn.source {
            mapping.insert(
                (s.instance_name.clone(), s.port_name.clone()),
                net_name.clone(),
            );
        }
        if let Some(t) = &conn.target {
            mapping.insert((t.instance_name.clone(), t.port_name.clone()), net_name);
        }
    }
    mapping
}

/// Instance name → the pins it connects, in `instance_nets` order.
pub type InstancePins<'a> = HashMap<&'a str, Vec<&'a str>>;

/// [`InstancePins`] of a module's `instance_nets`. A writer builds it once per module, so
/// finding an instance's pins never scans every connection of the module.
pub fn pins_by_instance(instance_nets: &IndexMap<(String, String), String>) -> InstancePins<'_> {
    let mut pins: InstancePins = HashMap::new();
    for (instance, pin) in instance_nets.keys() {
        pins.entry(instance.as_str())
            .or_default()
            .push(pin.as_str());
    }
    pins
}

/// The pins `instance` connects; empty when it has none.
pub fn pins_of<'a>(instance_pins: &'a InstancePins<'a>, instance: &str) -> &'a [&'a str] {
    match instance_pins.get(instance) {
        Some(pins) => pins,
        None => &[],
    }
}

/// An instance's pins as owned names: its port order when its master is unknown.
pub fn pin_names(pins: &[&str]) -> Vec<String> {
    let mut names = Vec::new();
    for pin in pins {
        names.push(pin.to_string());
    }
    names
}

// ---------------------------------------------------------------------------
// Simulation kind maps
// ---------------------------------------------------------------------------

pub fn analysis_kind(kind: &str) -> Option<AnalysisKind> {
    Some(match kind {
        "op" => AnalysisKind::Op,
        "dc" => AnalysisKind::Dc,
        "ac" => AnalysisKind::Ac,
        "tran" => AnalysisKind::Tran,
        "noise" => AnalysisKind::Noise,
        "tf" => AnalysisKind::Tf,
        "sens" => AnalysisKind::Sens,
        "pz" => AnalysisKind::Pz,
        "disto" => AnalysisKind::Disto,
        "four" => AnalysisKind::Four,
        "fft" => AnalysisKind::Fft,
        _ => return None,
    })
}

pub fn output_request_kind(kind: &str) -> Option<OutputRequestKind> {
    Some(match kind {
        "print" => OutputRequestKind::Print,
        "plot" => OutputRequestKind::Plot,
        "probe" => OutputRequestKind::Probe,
        "save" => OutputRequestKind::Save,
        _ => return None,
    })
}
