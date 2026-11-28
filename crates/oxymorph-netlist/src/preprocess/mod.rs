//! Per-format preprocessing, ported verbatim from `*/preprocess.py`.

use std::sync::OnceLock;

use regex::Regex;

pub mod systemverilog;

pub(crate) fn block_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").unwrap())
}

// ---------------------------------------------------------------------------
// SPICE family
// ---------------------------------------------------------------------------

pub fn strip_inline_comment(line: &str) -> String {
    let mut in_quote = false;
    for (i, ch) in line.char_indices() {
        if ch == '\'' {
            in_quote = !in_quote;
        } else if !in_quote && (ch == '$' || ch == ';') {
            return line[..i].trim_end().to_string();
        }
    }
    line.to_string()
}

pub fn join_continuations(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut merged: Vec<String> = vec![lines[0].clone()];
    for line in &lines[1..] {
        if let Some(rest) = line.strip_prefix('+') {
            let last = merged.last_mut().unwrap();
            last.push(' ');
            last.push_str(rest.trim_start());
        } else {
            merged.push(line.clone());
        }
    }
    merged.join("\n")
}

pub fn join_continuations_backslash(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut merged: Vec<String> = Vec::new();
    let mut buf = String::new();
    for line in lines {
        if let Some(rest) = line.strip_suffix('\\') {
            buf.push_str(rest.trim_end());
            buf.push(' ');
        } else {
            buf.push_str(line);
            merged.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        merged.push(buf);
    }
    merged.join("\n")
}

fn spice_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let stripped = raw.trim();
        if stripped.is_empty() || stripped.starts_with('*') {
            continue;
        }
        let cleaned = strip_inline_comment(stripped);
        if cleaned.is_empty() {
            continue;
        }
        lines.push(cleaned);
    }
    lines
}

pub fn preprocess_spice(text: &str) -> String {
    let joined = join_continuations(&spice_lines(text));
    if !joined.is_empty() && !joined.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

pub fn preprocess_hspice(text: &str) -> String {
    let lines = spice_lines(text);
    let backslash_joined = join_continuations_backslash(&lines);
    let joined_lines: Vec<String> = backslash_joined.lines().map(|s| s.to_string()).collect();
    let joined = join_continuations(&joined_lines);
    if !joined.is_empty() && !joined.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

pub fn preprocess_xyce(text: &str) -> String {
    preprocess_spice(text)
}

// ---------------------------------------------------------------------------
// Spectre
// ---------------------------------------------------------------------------

pub fn preprocess_spectre(text: &str) -> String {
    let text = block_comment_re().replace_all(text, "");
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let stripped = raw.trim();
        if stripped.is_empty() {
            continue;
        }
        let mut s = stripped.to_string();
        if let Some(idx) = s.find("//") {
            s = s[..idx].trim_end().to_string();
        }
        if s.is_empty() {
            continue;
        }
        lines.push(s);
    }
    let joined = join_continuations_backslash(&lines);
    let result = add_semicolons(&joined);
    if !result.is_empty() && !result.ends_with('\n') {
        format!("{result}\n")
    } else {
        result
    }
}

fn add_semicolons(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        let first_word = stripped.split_whitespace().next().unwrap_or("");
        if matches!(first_word, "subckt" | "section" | "ends" | "endsection") {
            out.push(stripped.to_string());
        } else {
            out.push(format!("{stripped} ;"));
        }
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Spectre-SPICE
// ---------------------------------------------------------------------------

pub struct SpectreSpiceSection {
    pub language: String,
    pub text: String,
}

pub fn simulator_lang_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*simulator\s+lang\s*=\s*(\w+)").unwrap())
}

/// Split a Spectre-SPICE file into sections by language mode. The file starts in
/// `language` (`spice` for a Spectre-SPICE file, `spectre` for a Spectre one); a
/// `simulator lang=spectre|spice` line switches the mode.
pub fn preprocess_spectre_spice(text: &str, language: &str) -> Vec<SpectreSpiceSection> {
    let mut sections = Vec::new();
    let mut current_lang = language.to_string();
    let mut current_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(caps) = simulator_lang_re().captures(line) {
            if !current_lines.is_empty() {
                sections.push(SpectreSpiceSection {
                    language: current_lang.clone(),
                    text: format!("{}\n", current_lines.join("\n")),
                });
                current_lines.clear();
            }
            current_lang = caps.get(1).unwrap().as_str().to_lowercase();
        } else {
            current_lines.push(line.to_string());
        }
    }
    if !current_lines.is_empty() {
        sections.push(SpectreSpiceSection {
            language: current_lang,
            text: format!("{}\n", current_lines.join("\n")),
        });
    }
    sections
}

// ---------------------------------------------------------------------------
// VAMS
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct VamsPreprocessResult {
    pub text: String,
    pub directives: Vec<(String, String)>,
    pub analog_blocks: Vec<String>,
}

pub(crate) fn line_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"//[^\n]*").unwrap())
}

pub(crate) fn directive_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*`(\w+)\s*(.*)").unwrap())
}

/// Strip comments, keep the active `` `ifdef `` branches for `defines` (expanding
/// macros), extract directives and replace analog blocks with placeholders.
pub fn preprocess_vams(
    text: &str,
    defines: &indexmap::IndexMap<String, String>,
    budget: &mut crate::hdl::ExpansionBudget,
) -> Result<VamsPreprocessResult, crate::Error> {
    let text = block_comment_re().replace_all(text, "").to_string();
    let text = line_comment_re().replace_all(&text, "").to_string();
    let text = crate::hdl::apply_defines(&text, defines, budget)?;

    let mut directives: Vec<(String, String)> = Vec::new();
    for caps in directive_re().captures_iter(&text) {
        directives.push((
            caps.get(1).unwrap().as_str().to_string(),
            caps.get(2).unwrap().as_str().trim().to_string(),
        ));
    }
    let text = directive_re().replace_all(&text, "").to_string();

    let (mut text, analog_blocks) = replace_analog_blocks(&text);

    text = Regex::new(r"\n{2,}")
        .unwrap()
        .replace_all(&text, "\n")
        .to_string();
    text = text.trim().to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }

    Ok(VamsPreprocessResult {
        text,
        directives,
        analog_blocks,
    })
}

fn analog_begin_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\banalog\s+begin\b").unwrap())
}

fn begin_end_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(begin|end)\b").unwrap())
}

fn replace_analog_blocks(text: &str) -> (String, Vec<String>) {
    let mut blocks: Vec<String> = Vec::new();
    let text = replace_analog_begin_end(text, &mut blocks);
    let text = replace_analog_single(&text, &mut blocks);
    (text, blocks)
}

fn replace_analog_begin_end(text: &str, blocks: &mut Vec<String>) -> String {
    let mut result: Vec<String> = Vec::new();
    let re = analog_begin_re();
    let be = begin_end_re();
    let mut i = 0usize;
    while i < text.len() {
        let m = match re.find(&text[i..]) {
            Some(m) => m,
            None => {
                result.push(text[i..].to_string());
                break;
            }
        };
        let start = i + m.start();
        result.push(text[i..start].to_string());
        let body_start = i + m.end();
        let mut depth = 1i32;
        let mut j = body_start;
        while j < text.len() && depth > 0 {
            let bm = match be.find(&text[j..]) {
                Some(bm) => bm,
                None => {
                    j = text.len();
                    break;
                }
            };
            let keyword = bm.as_str();
            j += bm.end();
            if keyword == "begin" {
                depth += 1;
            } else {
                depth -= 1;
            }
        }
        let block_text = text[start..j].trim().to_string();
        let idx = blocks.len();
        blocks.push(block_text);
        result.push(format!("__analog_placeholder__ {idx} ;"));
        i = j;
    }
    result.concat()
}

fn replace_analog_single(text: &str, blocks: &mut Vec<String>) -> String {
    // Mirrors `ANALOG_SINGLE_RE = \banalog\b\s+(?!begin\b)([^;]+;)` without
    // lookahead (Rust's regex crate has none).
    let analog_re = Regex::new(r"\banalog\b\s+").unwrap();
    let mut result = String::new();
    let mut last = 0usize;
    while let Some(m) = analog_re.find(&text[last..]) {
        let start = last + m.start();
        let after = last + m.end();
        let rest = &text[after..];
        if rest.starts_with("begin") {
            // Not a single statement; emit the match and continue after it.
            result.push_str(&text[last..after]);
            last = after;
            continue;
        }
        // Find the terminating ';'.
        match rest.find(';') {
            Some(semi) => {
                let stmt = &text[after..after + semi + 1];
                let idx = blocks.len();
                blocks.push(format!("analog {stmt}"));
                result.push_str(&text[last..start]);
                result.push_str(&format!("__analog_placeholder__ {idx} ;"));
                last = after + semi + 1;
            }
            None => {
                result.push_str(&text[last..]);
                last = text.len();
            }
        }
    }
    result.push_str(&text[last..]);
    result
}
