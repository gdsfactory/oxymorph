//! Shared Verilog-family helpers (SystemVerilog, Verilog-AMS), ported from `hdl.py`.
//!
//! - [`apply_defines`]: `` `ifdef `` / `` `define `` handling before parsing.
//! - Bit-level nets: a bus `in[3:0]` is the nets `in[3]` … `in[0]`. Port connection
//!   expressions resolve to ordered (msb-first) bit lists, so a bit-select `in[0]`
//!   meets bit 0 of port `in`.
//! - [`format_bits`]: the writers' inverse, bits back to `bus[m:l]` / `{…}`.
//!
//! Text is untrusted. Every expansion (a bus range, a replication, an instance array,
//! the pins of one array connection) is capped at [`MAX_EXPANDED_WIDTH`], and one
//! [`ExpansionBudget`] per parse bounds what a whole text expands to.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;
use oxymorph_schema::{
    Circuit, Connection, Directive, DirectiveKind, ModuleReference, Port, SignalDomain,
};
use regex::Regex;

use crate::error::Error;
use crate::helpers::{domain_of, net_map_to_connections, open_node, pin_names, NetMap, PinDomains};

/// The most bits (or instances) one expression, declaration or instance array expands to.
pub const MAX_EXPANDED_WIDTH: usize = 1_000_000;

/// What one parse may expand to, in units (a declared bit, a connection pin, an instance
/// with its master's port bits and its params, a byte of `` `define `` output):
/// `max(1_000_000, 64 × text length)`. Held by the parse, so small texts cannot amplify.
#[derive(Clone, Debug)]
pub struct ExpansionBudget {
    limit: usize,
    used: usize,
}

impl ExpansionBudget {
    const FLOOR: usize = 1_000_000;
    const UNITS_PER_BYTE: usize = 64;

    /// The budget of a parse of `text`.
    pub fn for_text(text: &str) -> Self {
        let scaled = text.len().saturating_mul(Self::UNITS_PER_BYTE);
        Self {
            limit: scaled.max(Self::FLOOR),
            used: 0,
        }
    }

    /// Spend `units`; `Error::Validation` naming the budget once it is exceeded.
    pub fn charge(&mut self, units: usize) -> Result<(), Error> {
        self.used = self.used.saturating_add(units);
        if self.used > self.limit {
            return Err(validation(format!(
                "the text expands past its expansion budget of {} units",
                self.limit
            )));
        }
        Ok(())
    }
}

/// Options of the HDL parsers (`parse_systemverilog`, `parse_vams_with`).
#[derive(Clone, Debug, Default)]
pub struct HdlOptions {
    /// Predefined macros for `` `ifdef `` (`{"IS_SIM": "1"}`); with none, the `` `else ``
    /// branches are taken.
    pub defines: IndexMap<String, String>,
    /// Port/net type (or discipline) → domain, e.g. `{"connection": Waveguide}`.
    pub disciplines: IndexMap<String, SignalDomain>,
    /// Cell → pin → domain, for cells without typed ports; wins over `disciplines`.
    pub pin_domains: Option<PinDomains>,
}

/// `(msb, lsb)` as declared.
pub type Range = (i64, i64);
/// Bus name → declared range.
pub type Buses = IndexMap<String, Range>;
/// `[3:0]` → `["3", "0"]`; `[3]` → `["3"]`.
pub type Dim = Vec<String>;

fn validation(message: String) -> Error {
    Error::Validation(message)
}

// ── `ifdef / `define ─────────────────────────────────────────────────────

fn conditional_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^[ \t]*`(ifdef|ifndef|elsif|else|endif|define|undef)\b[ \t]*(\w*)[ \t]*(.*?)\s*$",
        )
        .unwrap()
    })
}

fn macro_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`(\w+)").unwrap())
}

/// One `` `ifdef `` level: whether its parent is active, and whether a branch was taken.
struct Conditional {
    parent_active: bool,
    taken: bool,
}

/// Keep only the active `` `ifdef `` branches and expand object-like macros.
///
/// `defines` are the predefined macros; with none, an `` `ifdef X … `else … `endif`` takes
/// the `` `else `` branch, like a compiler. `` `define `` lines in active code register a
/// macro and are kept (they are recorded as directives).
pub fn apply_defines(
    text: &str,
    defines: &IndexMap<String, String>,
    budget: &mut ExpansionBudget,
) -> Result<String, Error> {
    let mut macros = defines.clone();
    let mut output = String::with_capacity(text.len());
    let mut stack: Vec<Conditional> = Vec::new();
    let mut active = true;
    for line in text.split_inclusive('\n') {
        let captures = conditional_re().captures(line);
        let keyword = captures.as_ref().map(|found| found[1].to_string());
        let name = captures
            .as_ref()
            .map(|found| found[2].to_string())
            .unwrap_or_default();
        let keyword = keyword.as_deref();
        if matches!(keyword, Some("elsif" | "else" | "endif")) && stack.is_empty() {
            return Err(validation(format!(
                "`{} without `ifdef",
                keyword.unwrap_or_default()
            )));
        }
        match keyword {
            Some(conditional @ ("ifdef" | "ifndef")) => {
                let hit = macros.contains_key(&name) == (conditional == "ifdef");
                stack.push(Conditional {
                    parent_active: active,
                    taken: hit,
                });
                active = active && hit;
            }
            Some("elsif") => {
                let level = stack.last_mut().expect("checked above");
                let hit = !level.taken && macros.contains_key(&name);
                level.taken = level.taken || hit;
                active = level.parent_active && hit;
            }
            Some("else") => {
                let level = stack.last_mut().expect("checked above");
                active = level.parent_active && !level.taken;
                level.taken = true;
            }
            Some("endif") => active = stack.pop().expect("checked above").parent_active,
            _ if !active => continue,
            Some("define") => {
                let value = captures.as_ref().map(|found| found[3].to_string());
                macros.insert(name, value.unwrap_or_default());
                budget.charge(line.len())?;
                output.push_str(line);
            }
            Some(_) => {
                macros.shift_remove(&name);
                budget.charge(line.len())?;
                output.push_str(line);
            }
            None => expand_macros(line, &macros, &mut output, budget)?,
        }
    }
    if !stack.is_empty() {
        return Err(validation("`ifdef without `endif".to_string()));
    }
    Ok(output)
}

/// `line` with its macros expanded onto `output`, each piece charged before it is added.
fn expand_macros(
    line: &str,
    macros: &IndexMap<String, String>,
    output: &mut String,
    budget: &mut ExpansionBudget,
) -> Result<(), Error> {
    budget.charge(line.len())?;
    let mut last = 0;
    for captures in macro_re().captures_iter(line) {
        let whole = captures.get(0).expect("group 0");
        output.push_str(&line[last..whole.start()]);
        match macros.get(&captures[1]) {
            Some(value) => {
                budget.charge(value.len())?;
                output.push_str(value);
            }
            None => output.push_str(whole.as_str()),
        }
        last = whole.end();
    }
    output.push_str(&line[last..]);
    Ok(())
}

// ── Directives (shared by both parsers and writers) ──────────────────────

/// The `(keyword, value)` pairs a preprocessor extracted, as directives.
pub fn directives_of(pairs: &[(String, String)]) -> Vec<Directive> {
    let mut directives = Vec::new();
    for (keyword, value) in pairs {
        let kind = match keyword.as_str() {
            "include" => DirectiveKind::Include,
            "define" => DirectiveKind::Define,
            "timescale" => DirectiveKind::Timescale,
            _ => DirectiveKind::Unspecified,
        };
        let value = if kind == DirectiveKind::Include {
            value.trim_matches(|character| character == '"' || character == '\'')
        } else {
            value.as_str()
        };
        directives.push(Directive {
            kind,
            value: value.to_string(),
            ..Default::default()
        });
    }
    directives
}

/// `` `include ``, `` `define `` and `` `timescale `` lines.
pub fn write_directives(directives: &[Directive], lines: &mut Vec<String>) {
    for directive in directives {
        match directive.kind {
            DirectiveKind::Include => lines.push(format!("`include \"{}\"", directive.value)),
            DirectiveKind::Define => lines.push(format!("`define {}", directive.value)),
            DirectiveKind::Timescale => lines.push(format!("`timescale {}", directive.value)),
            _ => {}
        }
    }
}

// ── Expressions ──────────────────────────────────────────────────────────

/// A port connection or parameter expression as the HDL transformers build it.
#[derive(Clone, Debug, PartialEq)]
pub enum NetExpression {
    /// A net: `a`, `a[3]`, `a[3:0]`; `select` is empty, one bit or `(msb, lsb)`.
    Net { name: String, select: Dim },
    /// A numeric literal (`0`, `1'b0`): a constant tie when used as a net.
    Literal(String),
    /// `{a, b[1:0]}`: parts in source (msb-first) order.
    Concat(Vec<NetExpression>),
    /// Any other expression, as text.
    Text(String),
}

impl NetExpression {
    pub fn net(name: &str) -> Self {
        NetExpression::Net {
            name: name.to_string(),
            select: Vec::new(),
        }
    }

    /// The expression as written (Python's `str` of it).
    pub fn text(&self) -> String {
        match self {
            NetExpression::Net { name, select } if select.is_empty() => name.clone(),
            NetExpression::Net { name, select } => format!("{name}[{}]", select.join(":")),
            NetExpression::Literal(text) | NetExpression::Text(text) => text.clone(),
            NetExpression::Concat(parts) => {
                let mut texts = Vec::new();
                for part in parts {
                    texts.push(part.text());
                }
                format!("{{{}}}", texts.join(", "))
            }
        }
    }

    /// A lower bound on the bits the expression resolves to: every leaf is one or more.
    fn leaf_count(&self) -> usize {
        match self {
            NetExpression::Concat(parts) => {
                let mut count: usize = 0;
                for part in parts {
                    count = count.saturating_add(part.leaf_count());
                }
                count
            }
            _ => 1,
        }
    }
}

/// `{count{parts}}`: the parts repeated `count` times, refused beyond the expansion cap,
/// charged to `budget` before they are copied.
pub fn replicate(
    count_text: &str,
    parts: Vec<NetExpression>,
    budget: &mut ExpansionBudget,
) -> Result<NetExpression, Error> {
    let count = to_int(count_text)
        .filter(|count| *count >= 1)
        .ok_or_else(|| {
            validation(format!(
                "replication count '{count_text}' must be a positive integer"
            ))
        })?;
    let mut part_leaves: usize = 0;
    for part in &parts {
        part_leaves = part_leaves.saturating_add(part.leaf_count());
    }
    let total = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .saturating_mul(part_leaves);
    if total > MAX_EXPANDED_WIDTH {
        return Err(too_wide(&format!("{{{count_text}{{…}}}}"), total));
    }
    budget.charge(total)?;
    let mut repeated = Vec::with_capacity(total);
    for _ in 0..count {
        repeated.extend(parts.iter().cloned());
    }
    Ok(NetExpression::Concat(repeated))
}

fn too_wide(what: &str, width: usize) -> Error {
    validation(format!(
        "{what}: {width} bits or instances exceed the limit of {MAX_EXPANDED_WIDTH}"
    ))
}

/// The integer a text is (`_` separators allowed); None when it is not one.
pub fn to_int(text: &str) -> Option<i64> {
    text.trim().replace('_', "").parse().ok()
}

/// `(msb, lsb)` of a declared `[msb:lsb]`; None if not constant.
pub fn to_range(select: &[String]) -> Option<Range> {
    if select.len() != 2 {
        return None;
    }
    Some((to_int(&select[0])?, to_int(&select[1])?))
}

/// `(msb, lsb)` of a one-dimensional declaration; None when scalar. A non-constant range
/// (`[N-1:0]`) is treated as scalar.
pub fn decl_range(name: &str, dims: &[Dim]) -> Result<Option<Range>, Error> {
    if dims.len() > 1 {
        return Err(validation(format!(
            "{name}: multi-dimensional buses are not supported"
        )));
    }
    let range = dims.first().and_then(|dim| to_range(dim));
    if let Some(range) = range {
        check_width(name, range)?;
    }
    Ok(range)
}

/// Bits of a range (saturating; a range is checked against the cap before use).
pub fn range_width(range: Range) -> usize {
    let width = range.0.abs_diff(range.1).saturating_add(1);
    usize::try_from(width).unwrap_or(usize::MAX)
}

fn check_width(name: &str, range: Range) -> Result<(), Error> {
    let width = range_width(range);
    if width > MAX_EXPANDED_WIDTH {
        return Err(too_wide(&format!("{name}[{}:{}]", range.0, range.1), width));
    }
    Ok(())
}

/// `name[msb]` … `name[lsb]`, refused beyond the expansion cap.
pub fn bits(name: &str, range: Range) -> Result<Vec<String>, Error> {
    check_width(name, range)?;
    Ok(bit_names(name, range))
}

/// `name[msb]` … `name[lsb]` (writers: the range comes from the IR).
pub fn bit_names(name: &str, range: Range) -> Vec<String> {
    let (msb, lsb) = range;
    let mut names = Vec::new();
    if msb >= lsb {
        let mut index = msb;
        loop {
            names.push(format!("{name}[{index}]"));
            if index == lsb {
                break;
            }
            index -= 1;
        }
    } else {
        for index in msb..=lsb {
            names.push(format!("{name}[{index}]"));
        }
    }
    names
}

/// Bit nets (msb first) of a connection expression. A literal is a constant tie repeated
/// over `width` bits. Selects of undeclared buses or out-of-range bits are errors. Every
/// bit is charged to `budget` before it is made.
pub fn resolve(
    expression: &NetExpression,
    buses: &Buses,
    width: usize,
    budget: &mut ExpansionBudget,
) -> Result<Vec<String>, Error> {
    match expression {
        NetExpression::Concat(parts) => {
            let mut nets = Vec::new();
            for part in parts {
                nets.extend(resolve(part, buses, 1, budget)?);
                if nets.len() > MAX_EXPANDED_WIDTH {
                    return Err(too_wide(&expression.text(), nets.len()));
                }
            }
            Ok(nets)
        }
        NetExpression::Literal(text) => {
            budget.charge(width)?;
            Ok(vec![text.clone(); width])
        }
        NetExpression::Text(text) => {
            Err(validation(format!("unsupported net expression '{text}'")))
        }
        NetExpression::Net { name, select } => {
            let bus = buses.get(name).copied();
            if select.is_empty() {
                return match bus {
                    Some(range) => {
                        budget.charge(range_width(range))?;
                        bits(name, range)
                    }
                    None => {
                        budget.charge(1)?;
                        Ok(vec![name.clone()])
                    }
                };
            }
            let text = expression.text();
            let Some(bus) = bus else {
                // ponytail: integer ranges only; evaluate `[N-1:0]` against parameters if needed
                return Err(validation(format!(
                    "{text}: select of '{name}', which is not a bus with a constant range"
                )));
            };
            let (low, high) = (bus.0.min(bus.1), bus.0.max(bus.1));
            let mut indices = Vec::new();
            for index_text in select {
                match to_int(index_text) {
                    Some(index) if (low..=high).contains(&index) => indices.push(index),
                    _ => {
                        return Err(validation(format!(
                            "{text}: select outside {name}[{}:{}]",
                            bus.0, bus.1
                        )))
                    }
                }
            }
            let selected = (indices[0], indices[indices.len() - 1]);
            budget.charge(range_width(selected))?;
            bits(name, selected)
        }
    }
}

/// Per-instance bits of one port connection of an instance array (IEEE 1800 23.3.3.5).
///
/// `count * width` bits are sliced msb-first over the instances (leftmost first); exactly
/// `width` bits go to every instance. `width` is None for a module without a definition.
pub fn split_array(
    nets: Vec<String>,
    count: usize,
    width: Option<usize>,
    budget: &mut ExpansionBudget,
) -> Result<Vec<Vec<String>>, Error> {
    let total = nets.len();
    let width = width.unwrap_or(if total.is_multiple_of(count) {
        total / count
    } else {
        1
    });
    if total == count.saturating_mul(width) {
        let mut chunks = Vec::with_capacity(count);
        for chunk in nets.chunks(width.max(1)) {
            chunks.push(chunk.to_vec());
        }
        chunks.resize(count, Vec::new());
        return Ok(chunks);
    }
    if total == width {
        let pins = count.saturating_mul(width);
        if pins > MAX_EXPANDED_WIDTH {
            return Err(too_wide("broadcast", pins));
        }
        budget.charge(pins)?;
        return Ok(vec![nets; count]);
    }
    Err(validation(format!(
        "{total} bits for {count} instance(s) of a {width}-bit port"
    )))
}

/// `(instance, port bit, net)` triples for one port connection.
///
/// `port_ranges` are the target module's port ranges (None = scalar), or None when the
/// module is not defined: its port then takes the width of the expression, with bits
/// `port[n-1]` … `port[0]`. Every triple is charged to `budget`.
pub fn connect(
    instances: &[String],
    port: &str,
    expression: &NetExpression,
    buses: &Buses,
    port_ranges: Option<&IndexMap<String, Option<Range>>>,
    budget: &mut ExpansionBudget,
) -> Result<Vec<(String, String, String)>, Error> {
    let known = port_ranges.is_some_and(|ranges| ranges.contains_key(port));
    let range = port_ranges.and_then(|ranges| ranges.get(port).copied().flatten());
    let width = if known {
        Some(range.map_or(1, range_width))
    } else {
        None
    };
    let chunks =
        connection_chunks(expression, buses, instances.len(), width, budget).map_err(|error| {
            let message = match error {
                Error::Validation(message) => message,
                other => other.to_string(),
            };
            validation(format!(
                "{}.{port}({}): {message}",
                instances[0],
                expression.text()
            ))
        })?;
    let mut triples = Vec::new();
    for (instance, chunk) in instances.iter().zip(chunks) {
        let pins = match range {
            Some(range) => bit_names(port, range),
            None if chunk.len() == 1 => vec![port.to_string()],
            None => bit_names(port, (chunk.len() as i64 - 1, 0)),
        };
        for (pin, net) in pins.into_iter().zip(chunk) {
            triples.push((instance.clone(), pin, net));
        }
    }
    Ok(triples)
}

/// The per-instance bits of one connection, each future triple charged to `budget`.
fn connection_chunks(
    expression: &NetExpression,
    buses: &Buses,
    count: usize,
    width: Option<usize>,
    budget: &mut ExpansionBudget,
) -> Result<Vec<Vec<String>>, Error> {
    let nets = resolve(expression, buses, width.unwrap_or(1), budget)?;
    let chunks = split_array(nets, count, width, budget)?;
    let mut triples: usize = 0;
    for chunk in &chunks {
        triples = triples.saturating_add(chunk.len());
    }
    budget.charge(triples)?;
    Ok(chunks)
}

/// `Error::Validation` when a module header names a port twice (illegal Verilog, and a
/// writer would list the port's bits once per occurrence).
// parity: kf-hdl-nets (kfnetlist keeps every occurrence)
pub fn check_unique_ports(module: &str, port_names: &[String]) -> Result<(), Error> {
    let mut seen = std::collections::HashSet::new();
    for name in port_names {
        if !seen.insert(name.as_str()) {
            return Err(validation(format!(
                "{module}: port '{name}' appears twice in the module header"
            )));
        }
    }
    Ok(())
}

/// Charge the instances of one instance statement (`count` of them, an array or one):
/// each costs itself, its master's port bits (a writer lists every bit of a port the
/// instance connects, even partly) and its params.
pub fn charge_instances(
    count: usize,
    port_ranges: Option<&IndexMap<String, Option<Range>>>,
    params: usize,
    budget: &mut ExpansionBudget,
) -> Result<(), Error> {
    let mut each = params.saturating_add(1);
    for range in port_ranges.into_iter().flat_map(IndexMap::values) {
        each = each.saturating_add(range.map_or(1, range_width));
    }
    budget.charge(count.saturating_mul(each))
}

/// `I[3]` … `I[0]` for an instance array `I[3:0]`, else `[name]`.
pub fn instance_names(name: &str, range: Option<Range>) -> Result<Vec<String>, Error> {
    match range {
        Some(range) => bits(name, range),
        None => Ok(vec![name.to_string()]),
    }
}

// ── Writers ──────────────────────────────────────────────────────────────

fn identifier_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_$]*$").unwrap())
}

const RESERVED: [&str; 38] = [
    "module",
    "macromodule",
    "endmodule",
    "assign",
    "alias",
    "parameter",
    "localparam",
    "input",
    "output",
    "inout",
    "ref",
    "signed",
    "unsigned",
    "wire",
    "tri",
    "triand",
    "trior",
    "tri0",
    "tri1",
    "supply0",
    "supply1",
    "trireg",
    "wand",
    "wor",
    "uwire",
    "logic",
    "bit",
    "reg",
    "byte",
    "shortint",
    "int",
    "longint",
    "integer",
    "time",
    "shortreal",
    "real",
    "realtime",
    "string",
];

/// The reserved words that are not built-in types.
const NOT_TYPES: [&str; 11] = [
    "input",
    "output",
    "inout",
    "ref",
    "module",
    "macromodule",
    "endmodule",
    "assign",
    "alias",
    "parameter",
    "localparam",
];

/// A SystemVerilog built-in net or variable type (`wire`, `logic`, `real`, …).
pub fn is_builtin_type(name: &str) -> bool {
    RESERVED.contains(&name) && !NOT_TYPES.contains(&name)
}

/// `name` as a SystemVerilog identifier, escaped (`\0 `) when needed.
pub fn sv_name(name: &str) -> String {
    if identifier_re().is_match(name) && !RESERVED.contains(&name) {
        name.to_string()
    } else {
        format!("\\{name} ")
    }
}

/// Range of a schema port (`width` + `range` property); None if scalar. The IR may come
/// from untrusted JSON: a port wider than [`MAX_EXPANDED_WIDTH`] is written as a scalar,
/// so its range is never walked.
pub fn declared_range(width: u32, range_text: Option<&str>) -> Option<Range> {
    let range = range_text.filter(|text| !text.is_empty()).and_then(|text| {
        let parts: Vec<String> = text.split(':').map(str::to_string).collect();
        to_range(&parts)
    });
    let range = if range.is_some() || width <= 1 {
        range
    } else {
        Some((i64::from(width) - 1, 0))
    };
    range.filter(|range| range_width(*range) <= MAX_EXPANDED_WIDTH)
}

/// Bit pins of a (possibly bus) port, as registered by the parsers.
pub fn port_pins(name: &str, width: u32, range_text: Option<&str>) -> Vec<String> {
    match declared_range(width, range_text) {
        Some(range) => bit_names(name, range),
        None => vec![name.to_string()],
    }
}

/// `base[index]` → `(base, index)`.
pub fn split_bit(name: &str) -> Option<(&str, i64)> {
    let without_bracket = name.strip_suffix(']')?;
    let open = without_bracket.rfind('[')?;
    let index_text = &without_bracket[open + 1..];
    let digits = index_text.strip_prefix('-').unwrap_or(index_text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((&without_bracket[..open], index_text.parse().ok()?))
}

/// `name` without a trailing bit select (`in[3]` → `in`).
pub fn bus_base(name: &str) -> &str {
    split_bit(name).map_or(name, |(base, _)| base)
}

enum Part {
    Run(String, Vec<i64>),
    Other(String),
}

/// Connection expression for bit nets: `a`, `a[2]`, `a[3:1]` or `{…}`.
pub fn format_bits(nets: &[String], buses: &Buses, escape: fn(&str) -> String) -> String {
    let mut parts: Vec<Part> = Vec::new();
    for net in nets {
        let bit = split_bit(net).filter(|(base, _)| buses.contains_key(*base));
        let Some((base, index)) = bit else {
            parts.push(Part::Other(escape(net)));
            continue;
        };
        if let Some(Part::Run(last_base, run)) = parts.last_mut() {
            if last_base == base && continues_run(run, index) {
                run.push(index);
                continue;
            }
        }
        parts.push(Part::Run(base.to_string(), vec![index]));
    }
    let mut texts = Vec::new();
    for part in parts {
        match part {
            Part::Other(text) => texts.push(text),
            Part::Run(base, run) => {
                let bus = buses[base.as_str()];
                let first = run[0];
                let last = run[run.len() - 1];
                if (first, last) == bus && run.len() == range_width(bus) {
                    texts.push(escape(&base));
                } else if run.len() == 1 {
                    texts.push(format!("{}[{first}]", escape(&base)));
                } else {
                    texts.push(format!("{}[{first}:{last}]", escape(&base)));
                }
            }
        }
    }
    if texts.len() == 1 {
        texts.remove(0)
    } else {
        format!("{{{}}}", texts.join(", "))
    }
}

/// Whether `index` extends `run` by one bit in its direction. In i128: a net name's index
/// may be any i64, so the difference of two can overflow.
fn continues_run(run: &[i64], index: i64) -> bool {
    let last = i128::from(run[run.len() - 1]);
    let step = if run.len() > 1 {
        last - i128::from(run[run.len() - 2])
    } else {
        i128::from(index) - last
    };
    step.abs() == 1 && i128::from(index) == last + step
}

/// [`format_bits`] for one instance port; empty when it is unconnected. A bit of a partly
/// connected bus port is written on its own open node (kfnetlist raises here).
pub fn connection_expr(
    instance: &str,
    pins: &[String],
    nets: &[Option<&String>],
    buses: &Buses,
    escape: fn(&str) -> String,
) -> String {
    if nets.iter().all(Option::is_none) {
        return String::new();
    }
    let mut connected = Vec::new();
    for (pin, net) in pins.iter().zip(nets) {
        match net {
            Some(net) => connected.push((*net).clone()),
            None => connected.push(open_node(instance, pin)),
        }
    }
    format_bits(&connected, buses, escape)
}

/// Module or external module name → its ports; a module wins over an external module of
/// the same name, and the first of two with one name wins.
pub type MasterPorts<'a> = HashMap<&'a str, &'a [Port]>;

/// [`MasterPorts`] of a circuit, built once per write.
pub fn master_ports(circuit: &Circuit) -> MasterPorts<'_> {
    let mut masters: MasterPorts = HashMap::new();
    for module in &circuit.modules {
        masters
            .entry(module.name.as_str())
            .or_insert(module.ports.as_slice());
    }
    for ext_module in &circuit.ext_modules {
        masters
            .entry(ext_module.name.as_str())
            .or_insert(ext_module.ports.as_slice());
    }
    masters
}

/// `(port name, bit pins)` of the master's ports, or the pins the instance uses (`pins`,
/// from [`crate::helpers::pins_by_instance`]). A port the instance has no pin on gets no
/// bits, so it is written open (`.p()`) without expanding its width.
// ponytail: writers stay infallible, so a wide bus port the instance connects only partly
// still costs its MAX_EXPANDED_WIDTH bits per instance here (each open bit is written on
// its own node); parsed text is charged for it by `ExpansionBudget`, IR from JSON is not.
// Add a fallible `try_write_*` with a budget if that matters.
pub fn port_order(
    reference: &ModuleReference,
    masters: &MasterPorts,
    pins: &[&str],
) -> Vec<(String, Vec<String>)> {
    let Some(ports) = masters.get(reference.module_name.as_str()) else {
        return bus_pins(&pin_names(pins));
    };
    let mut touched: HashSet<&str> = HashSet::new();
    for pin in pins {
        touched.insert(pin);
        touched.insert(bus_base(pin));
    }
    let mut order = Vec::new();
    for port in *ports {
        let bits = if touched.contains(port.name.as_str()) {
            let range_text = port.properties.get("range").map(String::as_str);
            port_pins(&port.name, port.width, range_text)
        } else {
            Vec::new()
        };
        order.push((port.name.clone(), bits));
    }
    order
}

/// `.port(expr)` connections of one instance, in port order.
pub fn port_connections(
    reference: &ModuleReference,
    masters: &MasterPorts,
    pins: &[&str],
    instance_nets: &IndexMap<(String, String), String>,
    buses: &Buses,
    port_name: fn(&str) -> String,
) -> String {
    let mut connections = Vec::new();
    for (name, port_bits) in port_order(reference, masters, pins) {
        let mut nets = Vec::new();
        for pin in &port_bits {
            nets.push(instance_nets.get(&(reference.name.clone(), pin.clone())));
        }
        let expression = connection_expr(&reference.name, &port_bits, &nets, buses, sv_name);
        connections.push(format!(".{}({expression})", port_name(&name)));
    }
    connections.join(", ")
}

/// `{base: (max, min)}` of every `base[i]` net name.
pub fn net_buses(names: &[&str]) -> Buses {
    let mut found: IndexMap<String, Range> = IndexMap::new();
    for name in names {
        if let Some((base, index)) = split_bit(name) {
            let range = found.entry(base.to_string()).or_insert((index, index));
            range.0 = range.0.max(index);
            range.1 = range.1.min(index);
        }
    }
    found
}

/// Group pins `in0[0], in0[1], en` into `[("in0", [in0[1], in0[0]]), ("en", [en])]`.
pub fn bus_pins(ports: &[String]) -> Vec<(String, Vec<String>)> {
    let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
    for pin in ports {
        groups
            .entry(bus_base(pin).to_string())
            .or_default()
            .push(pin.clone());
    }
    let mut grouped = Vec::new();
    for (base, mut pins) in groups {
        pins.sort_by_key(|pin| std::cmp::Reverse(split_bit(pin).map_or(0, |(_, index)| index)));
        grouped.push((base, pins));
    }
    grouped
}

// ── Signal domains (net_utils.py) ────────────────────────────────────────

/// [`net_map_to_connections`], recording each net's declared type (`wire`, `connection`,
/// `optical`) as the `discipline` property.
pub fn typed_connections(net_map: &NetMap, types: &IndexMap<String, String>) -> Vec<Connection> {
    // parity: kf-hdl-nets
    let mut connections = net_map_to_connections(net_map);
    for connection in &mut connections {
        if let Some(net_type) = types.get(bus_base(&connection.name)) {
            if !net_type.is_empty() {
                connection
                    .properties
                    .insert("discipline".to_string(), net_type.clone());
            }
        }
    }
    connections
}

/// Where a cell's ports live in the circuit.
#[derive(Clone, Copy)]
enum Cell {
    External(usize),
    Defined(usize),
}

fn cell_ports(circuit: &Circuit, cell: Cell) -> &[Port] {
    match cell {
        Cell::External(index) => &circuit.ext_modules[index].ports,
        Cell::Defined(index) => &circuit.modules[index].ports,
    }
}

fn cell_ports_mut(circuit: &mut Circuit, cell: Cell) -> &mut [Port] {
    match cell {
        Cell::External(index) => &mut circuit.ext_modules[index].ports,
        Cell::Defined(index) => &mut circuit.modules[index].ports,
    }
}

/// Resolve port and net domains in place.
///
/// 1. A port whose type/discipline is in `disciplines` gets that domain; so does a net
///    whose declared type (`discipline` property) is in it. A cell pin in `pin_domains`
///    has that domain, whether the cell is defined or a leaf; it wins over `disciplines`.
/// 2. A net touching any waveguide terminal is waveguide; untyped ports on it become
///    waveguide too, across the hierarchy, until nothing changes.
/// 3. Terminals neither typed nor reached keep the `o<N>` name rule.
pub fn propagate_domains(
    circuit: &mut Circuit,
    disciplines: &IndexMap<String, SignalDomain>,
    pin_domains: Option<&PinDomains>,
) {
    let empty = PinDomains::new();
    let pin_domains = pin_domains.unwrap_or(&empty);
    let mut cells: IndexMap<String, Cell> = IndexMap::new();
    for (index, ext_module) in circuit.ext_modules.iter().enumerate() {
        cells.insert(ext_module.name.clone(), Cell::External(index));
    }
    for (index, module) in circuit.modules.iter().enumerate() {
        cells.insert(module.name.clone(), Cell::Defined(index));
    }
    // (cell, port index) of the ports a type or pin domain fixed
    let mut typed: std::collections::HashSet<(String, usize)> = Default::default();
    for (cell_name, cell) in &cells {
        let cell_pins = pin_domains.get(cell_name);
        for (index, port) in cell_ports_mut(circuit, *cell).iter_mut().enumerate() {
            let pinned = cell_pins.and_then(|pins| pins.get(&port.name)).copied();
            if let Some(domain) = pinned.or_else(|| disciplines.get(&port.discipline).copied()) {
                port.domain = domain;
                typed.insert((cell_name.clone(), index));
            }
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for module_index in 0..circuit.modules.len() {
            let (domains, reached) =
                net_domains(circuit, module_index, &cells, disciplines, pin_domains);
            let module = &mut circuit.modules[module_index];
            for (connection, domain) in module.connections.iter_mut().zip(domains) {
                connection.domain = domain;
            }
            for (cell_name, port_index) in reached {
                if typed.contains(&(cell_name.clone(), port_index)) {
                    continue;
                }
                let port = &mut cell_ports_mut(circuit, cells[&cell_name])[port_index];
                if port.domain != SignalDomain::Waveguide {
                    port.domain = SignalDomain::Waveguide;
                    changed = true;
                }
            }
        }
    }
}

/// The domain of every connection of a module, and the `(cell, port)`s on its waveguide
/// nets.
fn net_domains(
    circuit: &Circuit,
    module_index: usize,
    cells: &IndexMap<String, Cell>,
    disciplines: &IndexMap<String, SignalDomain>,
    pin_domains: &PinDomains,
) -> (Vec<SignalDomain>, Vec<(String, usize)>) {
    let module = &circuit.modules[module_index];
    let mut masters: IndexMap<&str, &str> = IndexMap::new();
    for reference in &module.module_references {
        masters.insert(&reference.name, &reference.module_name);
    }
    let mut nets: IndexMap<&str, Vec<usize>> = IndexMap::new();
    for (index, connection) in module.connections.iter().enumerate() {
        nets.entry(&connection.name).or_default().push(index);
    }
    let mut domains = vec![SignalDomain::Electrical; module.connections.len()];
    let mut reached = Vec::new();
    for connection_indices in nets.values() {
        let mut waveguide = false;
        let mut owners: Vec<(String, Option<usize>)> = Vec::new();
        for index in connection_indices {
            let connection = &module.connections[*index];
            let declared = connection
                .properties
                .get("discipline")
                .map_or("", String::as_str);
            if disciplines.get(declared) == Some(&SignalDomain::Waveguide) {
                waveguide = true;
            }
            for port_reference in [&connection.source, &connection.target]
                .into_iter()
                .flatten()
            {
                let cell_name = if port_reference.instance_name.is_empty() {
                    module.name.as_str()
                } else {
                    masters
                        .get(port_reference.instance_name.as_str())
                        .copied()
                        .unwrap_or("")
                };
                let pin = bus_base(&port_reference.port_name);
                let owner = cells.get(cell_name).and_then(|cell| {
                    cell_ports(circuit, *cell)
                        .iter()
                        .position(|port| port.name == pin)
                });
                let domain = match (owner, cells.get(cell_name)) {
                    (Some(port_index), Some(cell)) => cell_ports(circuit, *cell)[port_index].domain,
                    _ => leaf_domain(pin_domains, cell_name, &port_reference.port_name),
                };
                if domain == SignalDomain::Waveguide {
                    waveguide = true;
                }
                owners.push((cell_name.to_string(), owner));
            }
        }
        let domain = if waveguide {
            SignalDomain::Waveguide
        } else {
            SignalDomain::Electrical
        };
        for index in connection_indices {
            domains[*index] = domain;
        }
        if waveguide {
            for (cell_name, owner) in owners {
                if let Some(port_index) = owner {
                    reached.push((cell_name, port_index));
                }
            }
        }
    }
    (domains, reached)
}

/// Domain of a pin of a cell without a definition.
fn leaf_domain(pin_domains: &PinDomains, cell: &str, pin: &str) -> SignalDomain {
    pin_domains
        .get(cell)
        .and_then(|pins| pins.get(bus_base(pin)))
        .copied()
        .unwrap_or_else(|| domain_of(pin))
}
