//! SystemVerilog preprocessing, ported from `systemverilog/preprocess.py`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use regex::{Captures, Regex};

use super::{block_comment_re, directive_re, line_comment_re};
use crate::error::Error;
use crate::hdl::{apply_defines, ExpansionBudget};

/// How deep procedural statements may nest (`if (a) if (b) …`, `@(x) @(y) …`).
pub const MAX_STATEMENT_DEPTH: usize = 128;

#[derive(Debug, Default, PartialEq)]
pub struct SvPreprocessResult {
    pub text: String,
    pub directives: Vec<(String, String)>,
    pub blocks: Vec<String>,
}

/// `(* pwl_file = "w.txt" *) vpwl [#(] ...`: a PWL source's file (see `stimulus.rs`).
fn pwl_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\(\*\s*pwl_file\s*=\s*("(?:[^"\\]|\\.)*")[^*]*\*\)\s*(\\\S+\s|[A-Za-z_$][\w$]*)\s*(#\s*\()?"#,
        )
        .unwrap()
    })
}

/// Constructs that are not netlist structure. Matched left to right, so a construct
/// nested in an earlier one is consumed with it.
fn construct_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b(always_comb|always_ff|always_latch|always|initial|final|assign|assert|assume|cover|typedef|import|export|genvar|timeunit|timeprecision|defparam|specparam|(?:virtual\s+)?class|function|task|generate|covergroup|property|sequence|clocking|specify|interface|package|program|primitive)\b",
        )
        .unwrap()
    })
}

fn end_keyword(keyword: &str) -> Option<&'static str> {
    Some(match keyword {
        "class" => "endclass",
        "function" => "endfunction",
        "task" => "endtask",
        "generate" => "endgenerate",
        "covergroup" => "endgroup",
        "property" => "endproperty",
        "sequence" => "endsequence",
        "clocking" => "endclocking",
        "specify" => "endspecify",
        "interface" => "endinterface",
        "package" => "endpackage",
        "program" => "endprogram",
        "primitive" => "endprimitive",
        _ => return None,
    })
}

const PROCEDURAL: [&str; 6] = [
    "always",
    "always_comb",
    "always_ff",
    "always_latch",
    "initial",
    "final",
];
const DROPPED: [&str; 6] = [
    "typedef",
    "import",
    "export",
    "genvar",
    "timeunit",
    "timeprecision",
];

fn simple_assign_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let net_ref = r"(?:\\\S+|[A-Za-z_][\w$]*)(?:\s*\[[^\]]*\])*";
        Regex::new(&format!(
            r"\Aassign\s+{net_ref}\s*=\s*{net_ref}(?:\s*,\s*{net_ref}\s*=\s*{net_ref})*\s*;\z"
        ))
        .unwrap()
    })
}

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)\A\s*(@\*|[A-Za-z_][\w$]*|.)").unwrap())
}

fn semicolon_scan_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""(?:\\.|[^"\\])*"|[(\[{]|[)\]}]|;"#).unwrap())
}

fn label_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\A\s*:\s*[A-Za-z_][\w$]*").unwrap())
}

fn group_atom_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\A\s*[\w$.']+").unwrap())
}

fn blank_lines_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n{2,}").unwrap())
}

/// The attribute as a `pwl_file` parameter of the instance.
fn pwl_file_param(captures: &Captures) -> String {
    let opened = format!("{} #(.pwl_file({})", &captures[2], &captures[1]);
    if captures.get(3).is_some() {
        format!("{opened}, ")
    } else {
        format!("{opened}) ")
    }
}

/// Remove `(* attr = value *)`, but not the `@(*)` event control.
fn strip_attributes(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut position = 0;
    let mut search = 0;
    while let Some(found) = text[search..].find("(*") {
        let start = search + found;
        let body = &text[start + 2..];
        if body.trim_start().starts_with(')') {
            search = start + 1;
            continue;
        }
        let Some(close) = body.find("*)") else {
            break;
        };
        output.push_str(&text[position..start]);
        position = start + 2 + close + 2;
        search = position;
    }
    output.push_str(&text[position..]);
    output
}

/// Prepare SystemVerilog text for parsing.
///
/// 1. Strip block comments, line comments and `(* attributes *)` (a PWL source's
///    `pwl_file` attribute becomes its parameter).
/// 2. Keep the active `` `ifdef `` branches for `defines`, expand macros, then extract
///    backtick directives.
/// 3. Replace behavioral constructs with `__sv_placeholder__ N ;` markers; typedef /
///    import / genvar / timeunit statements are dropped.
pub fn preprocess_systemverilog(
    text: &str,
    defines: &IndexMap<String, String>,
    budget: &mut ExpansionBudget,
) -> Result<SvPreprocessResult, Error> {
    let text = block_comment_re().replace_all(text, "");
    let text = line_comment_re().replace_all(&text, "");
    let text = pwl_file_re().replace_all(&text, pwl_file_param); // kept: the rest are dropped
    let text = strip_attributes(&text);
    let text = apply_defines(&text, defines, budget)?;

    let mut directives = Vec::new();
    for captures in directive_re().captures_iter(&text) {
        directives.push((captures[1].to_string(), captures[2].trim().to_string()));
    }
    let text = directive_re().replace_all(&text, "");

    let (text, blocks) = replace_behavioral_blocks(&text)?;

    let mut text = blank_lines_re().replace_all(&text, "\n").trim().to_string();
    if !text.is_empty() {
        text.push('\n');
    }
    Ok(SvPreprocessResult {
        text,
        directives,
        blocks,
    })
}

/// Replace non-structural constructs with numbered placeholder tokens.
pub fn replace_behavioral_blocks(text: &str) -> Result<(String, Vec<String>), Error> {
    let mut blocks: Vec<String> = Vec::new();
    let mut output = String::with_capacity(text.len());
    let mut position = 0;
    while let Some(found) = construct_re().find_at(text, position) {
        let construct = found.as_str();
        let keyword = construct.split_whitespace().last().unwrap_or(construct);
        let end = if let Some(close) = end_keyword(keyword) {
            match_end(text, found.end(), keyword, close)
        } else if PROCEDURAL.contains(&keyword) {
            skip_statement(text, found.end(), 0)?
        } else {
            skip_to_semicolon(text, found.end())
        };
        let statement = &text[found.start()..end];
        output.push_str(&text[position..found.start()]);
        if keyword == "assign" && simple_assign_re().is_match(statement) {
            output.push_str(statement);
        } else if !DROPPED.contains(&keyword) {
            output.push_str(&format!(" __sv_placeholder__ {} ;", blocks.len()));
            blocks.push(statement.trim().to_string());
        }
        position = end;
    }
    output.push_str(&text[position..]);
    Ok((output, blocks))
}

/// Index just past the next `;` outside brackets and strings.
fn skip_to_semicolon(text: &str, start: usize) -> usize {
    let mut depth: i64 = 0;
    for found in semicolon_scan_re().find_iter(&text[start..]) {
        match found.as_str() {
            ";" if depth == 0 => return start + found.end(),
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            _ => {}
        }
    }
    text.len()
}

thread_local! {
    static END_PATTERNS: RefCell<HashMap<(String, String), Regex>> = RefCell::new(HashMap::new());
}

/// Index just past the `close` keyword (and optional `: label`) matching an open.
fn match_end(text: &str, start: usize, open: &str, close: &str) -> usize {
    END_PATTERNS.with(|patterns| {
        let mut patterns = patterns.borrow_mut();
        let pattern = patterns
            .entry((open.to_string(), close.to_string()))
            .or_insert_with(|| Regex::new(&format!(r"\b({open})\b|\b({close})\b")).unwrap());
        let rest = &text[start..];
        let mut depth = 1;
        for captures in pattern.captures_iter(rest) {
            if captures.get(1).is_some() {
                depth += 1;
            } else {
                depth -= 1;
            }
            if depth == 0 {
                let end = start + captures.get(0).expect("group 0").end();
                return match label_re().find(&text[end..]) {
                    Some(label) => end + label.end(),
                    None => end,
                };
            }
        }
        text.len()
    })
}

/// Skip `(...)` or a single atom (`#10`, `@clk`).
fn skip_group(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    let opening = start + (rest.len() - rest.trim_start().len());
    if !text[opening..].starts_with('(') {
        return match group_atom_re().find(rest) {
            Some(atom) => start + atom.end(),
            None => start,
        };
    }
    let mut depth = 0;
    for (offset, byte) in text.as_bytes()[opening..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return opening + offset + 1;
        }
    }
    text.len()
}

/// Index just past one procedural statement starting at `start`.
fn skip_statement(text: &str, start: usize, depth: usize) -> Result<usize, Error> {
    if depth > MAX_STATEMENT_DEPTH {
        return Err(Error::Validation(format!(
            "procedural statements nest deeper than {MAX_STATEMENT_DEPTH}"
        )));
    }
    let Some(captures) = token_re().captures(&text[start..]) else {
        return Ok(text.len());
    };
    let token = captures.get(1).expect("group 1").as_str();
    let next = start + captures.get(0).expect("group 0").end();
    Ok(match token {
        "begin" => match_end(text, next, "begin", "end"),
        "fork" => match_end(text, next, "fork", "join|join_any|join_none"),
        "case" | "casex" | "casez" | "randcase" => {
            match_end(text, next, "case|casex|casez|randcase", "endcase")
        }
        "@" | "#" | "for" | "foreach" | "while" | "repeat" | "wait" => {
            skip_statement(text, skip_group(text, next), depth + 1)?
        }
        "@*" | "unique" | "priority" | "forever" => skip_statement(text, next, depth + 1)?,
        "do" => skip_to_semicolon(text, skip_statement(text, next, depth + 1)?),
        "if" => {
            let end = skip_statement(text, skip_group(text, next), depth + 1)?;
            match token_re().captures(&text[end..]) {
                Some(following) if &following[1] == "else" => {
                    let after = end + following.get(0).expect("group 0").end();
                    skip_statement(text, after, depth + 1)?
                }
                _ => end,
            }
        }
        _ => skip_to_semicolon(text, start),
    })
}
