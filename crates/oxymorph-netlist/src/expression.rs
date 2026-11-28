//! Parse and print parameter expressions (the schema's [`Expression`] tree), ported
//! from kfnetlist `netlist/expression.py` (d0daf3e, 1adb071, c12a71e, d94dc38) and the
//! canonical printer of kfnetlist-schema `src/expression.rs`.
//!
//! [`parse_expression`] reads any dialect's expression text with one shared grammar
//! (`grammars/expression.lark`); [`format_expression`] prints the canonical infix text
//! (`0.13 * 1e-6`), wrapped as the dialect wants it.

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use lark_rs::tree::{Child, ParseTree, Tree};
use oxymorph_schema::{
    BinaryOp, Call, Expression, Operator, ParameterValue, PrefixedValue, Siprefix, Ternary, UnaryOp,
};
use regex::{Captures, Regex};

use crate::ast::is_deeper_than;
use crate::engine::Engine;
use crate::error::Error;
use crate::helpers::{
    format_si_value, format_verilog_value, is_si_number, parse_parameter_number, parse_si_number,
};

const GRAMMAR: &str = include_str!("../grammars/expression.lark");

thread_local! {
    static ENGINE: Engine = Engine::lalr(GRAMMAR).expect("expression grammar builds");
}

/// Deepest tree `parse_expression` builds: the IR, its printers and its interchange
/// formats nest, and an expression is untrusted netlist text.
pub use oxymorph_schema::MAX_EXPRESSION_DEPTH;

/// How a simulator spells an expression (kfnetlist's dialect names).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Ngspice,
    Xyce,
    Hspice,
    Spectre,
    Vacask,
    SystemVerilog,
    Vams,
    /// A `.gsch` prop: bare, like Spectre.
    Gsch,
}

const DIALECT_NAMES: [(&str, Dialect); 8] = [
    ("ngspice", Dialect::Ngspice),
    ("xyce", Dialect::Xyce),
    ("hspice", Dialect::Hspice),
    ("spectre", Dialect::Spectre),
    ("vacask", Dialect::Vacask),
    ("systemverilog", Dialect::SystemVerilog),
    ("vams", Dialect::Vams),
    ("gsch", Dialect::Gsch),
];

impl Dialect {
    /// `(open, close)` around a compound expression.
    pub fn wrapping(self) -> (&'static str, &'static str) {
        match self {
            Dialect::Ngspice | Dialect::Xyce => ("{", "}"),
            Dialect::Hspice => ("'", "'"),
            Dialect::Spectre
            | Dialect::Vacask
            | Dialect::SystemVerilog
            | Dialect::Vams
            | Dialect::Gsch => ("", ""),
        }
    }
}

/// A dialect name that is not one of [`DIALECT_NAMES`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnknownDialect(pub String);

impl fmt::Display for UnknownDialect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = DIALECT_NAMES.iter().map(|(name, _)| *name).collect();
        write!(
            formatter,
            "unknown dialect {:?}; expected one of {}",
            self.0,
            names.join(", ")
        )
    }
}

impl std::error::Error for UnknownDialect {}

impl FromStr for Dialect {
    type Err = UnknownDialect;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        for (dialect_name, dialect) in DIALECT_NAMES {
            if dialect_name == name {
                return Ok(dialect);
            }
        }
        Err(UnknownDialect(name.to_string()))
    }
}

/// Substitutes a reference (or a word of raw text) by name.
pub type Resolve<'a> = &'a dyn Fn(&str) -> Option<Expression>;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A netlist identifier (the SPICE/Spectre IDENT token): a bare reference by name.
fn identifier_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.\[\]:!#]*$").unwrap())
}

/// What nyancad substitutes in unparsed text.
fn word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?-u:\b)[A-Za-z_][A-Za-z0-9_]*").unwrap())
}

/// The expression tree of `text`; `raw` text when it does not parse.
///
/// One outer `{…}` or `'…'` (SPICE dialect wrapping) is stripped first. The only error is a
/// tree deeper than [`MAX_EXPRESSION_DEPTH`] nodes (`Error::Validation`); parentheses that
/// add no node (`((a))`) do not count.
pub fn parse_expression(text: &str) -> Result<Expression, Error> {
    let mut body = text.trim();
    let bytes = body.as_bytes();
    if bytes.len() >= 2
        && matches!(
            (bytes[0], bytes[bytes.len() - 1]),
            (b'{', b'}') | (b'\'', b'\'')
        )
    {
        body = &body[1..body.len() - 1];
    }
    let Ok(tree) = ENGINE.with(|engine| engine.parse(body)) else {
        return Ok(raw_expression(text));
    };
    // the grammar's parse tree has one rule node per expression node
    if is_deeper_than(&tree, MAX_EXPRESSION_DEPTH) {
        return Err(Error::Validation(format!(
            "expression nests deeper than {MAX_EXPRESSION_DEPTH} levels"
        )));
    }
    Ok(build(&tree).unwrap_or_else(|| raw_expression(text)))
}

fn raw_expression(text: &str) -> Expression {
    Expression {
        raw: Some(text.to_string()),
        ..Default::default()
    }
}

fn build(tree: &ParseTree) -> Option<Expression> {
    match tree {
        ParseTree::Tree(node) => build_tree(node),
        _ => None,
    }
}

fn build_child(child: &Child) -> Option<Expression> {
    match child {
        Child::Tree(node) => build_tree(node),
        _ => None,
    }
}

fn token_text(child: &Child) -> Option<&str> {
    match child {
        Child::Token(token) => Some(token.value.as_str()),
        _ => None,
    }
}

/// Grammar tree -> `Expression`; rule aliases are lowercase `Operator` names.
fn build_tree(node: &Tree) -> Option<Expression> {
    let children = &node.children;
    match node.data.as_str() {
        "literal" => Some(Expression {
            literal: Some(parse_si_number(token_text(children.first()?)?).ok()?),
            ..Default::default()
        }),
        "reference" => Some(Expression::reference(token_text(children.first()?)?)),
        "call" => {
            let mut args = Vec::new();
            for argument in &children[1..] {
                args.push(build_child(argument)?);
            }
            Some(Expression {
                call: Some(Call {
                    name: token_text(children.first()?)?.to_string(),
                    args,
                }),
                ..Default::default()
            })
        }
        "ternary" => {
            let [condition, then, otherwise] = children.as_slice() else {
                return None;
            };
            Some(Expression {
                ternary: Some(Ternary {
                    cond: Some(Box::new(build_child(condition)?)),
                    then: Some(Box::new(build_child(then)?)),
                    otherwise: Some(Box::new(build_child(otherwise)?)),
                }),
                ..Default::default()
            })
        }
        name => {
            let op = operator_named(name)?;
            if matches!(op, Operator::Neg | Operator::Not) {
                return Some(Expression {
                    unary: Some(UnaryOp {
                        op,
                        operand: Some(Box::new(build_child(children.first()?)?)),
                    }),
                    ..Default::default()
                });
            }
            let [lhs, rhs] = children.as_slice() else {
                return None;
            };
            Some(Expression {
                binary: Some(BinaryOp {
                    op,
                    lhs: Some(Box::new(build_child(lhs)?)),
                    rhs: Some(Box::new(build_child(rhs)?)),
                }),
                ..Default::default()
            })
        }
    }
}

fn operator_named(name: &str) -> Option<Operator> {
    Some(match name {
        "or" => Operator::Or,
        "and" => Operator::And,
        "eq" => Operator::Eq,
        "ne" => Operator::Ne,
        "lt" => Operator::Lt,
        "le" => Operator::Le,
        "gt" => Operator::Gt,
        "ge" => Operator::Ge,
        "add" => Operator::Add,
        "sub" => Operator::Sub,
        "mul" => Operator::Mul,
        "div" => Operator::Div,
        "mod" => Operator::Mod,
        "pow" => Operator::Pow,
        "neg" => Operator::Neg,
        "not" => Operator::Not,
        _ => return None,
    })
}

/// A number's `prefixed_value`; an identifier's `reference`; else the expression tree
/// of `text` (`raw` when it does not parse or nests too deep).
pub fn parameter_value(text: &str) -> ParameterValue {
    if is_si_number(text) {
        return parse_parameter_number(text).unwrap_or_default();
    }
    let expression = if identifier_re().is_match(text) {
        Expression::reference(text)
    } else {
        expression_or_raw(text)
    };
    ParameterValue {
        expression: Some(expression),
        ..Default::default()
    }
}

/// `parse_expression`, keeping over-deep text verbatim as `raw` (Python's
/// `RecursionError` path).
pub fn expression_or_raw(text: &str) -> Expression {
    parse_expression(text).unwrap_or_else(|_| raw_expression(text))
}

/// `value` as the text of a `k=v` directive in `dialect`; "" when empty.
pub fn value_text(value: &ParameterValue, dialect: Dialect) -> String {
    if let Some(prefixed_value) = &value.prefixed_value {
        if dialect == Dialect::Vams {
            return number(prefixed_value, dialect);
        }
        return format_si_value(prefixed_value);
    }
    if let Some(expression) = &value.expression {
        return format_expression(expression, dialect, None, true);
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------

// Binding strength of a printed node; raw text inside a tree is always parenthesised.
const RAW: i32 = -1;
const TERNARY: i32 = 0;
const UNARY: i32 = 7;
const CALL: i32 = 9;
const ATOM: i32 = 10;

fn operator_text(op: Operator) -> (&'static str, i32) {
    match op {
        Operator::Or => ("||", 1),
        Operator::And => ("&&", 2),
        Operator::Eq => ("==", 3),
        Operator::Ne => ("!=", 3),
        Operator::Lt => ("<", 4),
        Operator::Le => ("<=", 4),
        Operator::Gt => (">", 4),
        Operator::Ge => (">=", 4),
        Operator::Add => ("+", 5),
        Operator::Sub => ("-", 5),
        Operator::Mul => ("*", 6),
        Operator::Div => ("/", 6),
        Operator::Mod => ("%", 6),
        Operator::Pow => ("**", 8),
        Operator::Neg => ("-", UNARY),
        Operator::Not => ("!", UNARY),
        Operator::Unspecified => ("?", RAW),
    }
}

/// Plain value of a prefixed number (`2.2` MICRO is `2.2e-06`).
pub fn to_float(value: &PrefixedValue) -> f64 {
    value.double_value * 10f64.powi(si_exponent(value.prefix))
}

fn si_exponent(prefix: Siprefix) -> i32 {
    match prefix {
        Siprefix::Unspecified => 0,
        Siprefix::Quecto => -30,
        Siprefix::Ronto => -27,
        Siprefix::Yocto => -24,
        Siprefix::Zepto => -21,
        Siprefix::Atto => -18,
        Siprefix::Femto => -15,
        Siprefix::Pico => -12,
        Siprefix::Nano => -9,
        Siprefix::Micro => -6,
        Siprefix::Milli => -3,
        Siprefix::Centi => -2,
        Siprefix::Deci => -1,
        Siprefix::Deca => 1,
        Siprefix::Hecto => 2,
        Siprefix::Kilo => 3,
        Siprefix::Mega => 6,
        Siprefix::Giga => 9,
        Siprefix::Tera => 12,
        Siprefix::Peta => 15,
        Siprefix::Exa => 18,
        Siprefix::Zetta => 21,
        Siprefix::Yotta => 24,
        Siprefix::Ronna => 27,
        Siprefix::Quetta => 30,
    }
}

/// `mantissa` and `exponent` as nyancad writes them: `1e-6`, not Python's `1e-06`.
fn join_exponent(mantissa: &str, exponent: i32) -> String {
    let trimmed = if mantissa.contains('.') {
        mantissa.trim_end_matches('0').trim_end_matches('.')
    } else {
        mantissa
    };
    format!("{trimmed}e{exponent}")
}

fn split_exponent(scientific: &str) -> (&str, i32) {
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific, "0"));
    (mantissa, exponent.parse().unwrap_or(0))
}

/// Python's `repr` of a float without `.0` or exponent padding: `0.13`, `2`, `1e-6`.
fn shortest(value: f64) -> String {
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = split_exponent(&scientific);
    if (-4..16).contains(&exponent) {
        format!("{value}")
    } else {
        join_exponent(mantissa, exponent)
    }
}

/// Python's `f"{value:.12g}"`, its exponent form written by `write_exponent(mantissa,
/// exponent)` (`1e-6` here, Python's `1e-06` in SystemVerilog).
pub fn twelve_significant_digits(value: f64, write_exponent: fn(&str, i32) -> String) -> String {
    const SIGNIFICANT_DIGITS: i32 = 12;
    let scientific = format!("{:.*e}", (SIGNIFICANT_DIGITS - 1) as usize, value);
    let (mantissa, exponent) = split_exponent(&scientific);
    if (-4..SIGNIFICANT_DIGITS).contains(&exponent) {
        let decimals = (SIGNIFICANT_DIGITS - 1 - exponent) as usize;
        trim_fraction_zeros(&format!("{value:.decimals$}"))
    } else {
        write_exponent(mantissa, exponent)
    }
}

/// `2.500` → `2.5`, `3.000` → `3`.
pub fn trim_fraction_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text.to_string()
    }
}

/// nyancad's spelling: SI suffix kept, else the shortest repr (`1e-6`, `2`).
fn number(value: &PrefixedValue, dialect: Dialect) -> String {
    if value.prefix != Siprefix::Unspecified && dialect == Dialect::Vams {
        return format_verilog_value(value); // `M`, not `meg`
    }
    if value.prefix != Siprefix::Unspecified && dialect != Dialect::SystemVerilog {
        return format_si_value(value); // SystemVerilog has no SI suffixes
    }
    let plain = to_float(value);
    if value.prefix == Siprefix::Unspecified {
        shortest(plain)
    } else {
        twelve_significant_digits(plain, join_exponent)
    }
}

fn wrap_in_parentheses(text: String, needs_parentheses: bool) -> String {
    if needs_parentheses {
        format!("({text})")
    } else {
        text
    }
}

/// Printed child and its binding strength; a missing child prints as empty raw text.
fn child_text(
    child: &Option<Box<Expression>>,
    dialect: Dialect,
    resolve: Option<Resolve>,
) -> (String, i32) {
    match child {
        Some(expression) => printed(expression, dialect, resolve),
        None => (String::new(), RAW),
    }
}

/// Printed `expression` and its binding strength.
fn printed(expression: &Expression, dialect: Dialect, resolve: Option<Resolve>) -> (String, i32) {
    if let Some(name) = &expression.reference {
        // once, like nyancad's textual substitution
        if let Some(substitute) = resolve.and_then(|lookup| lookup(name)) {
            return printed(&substitute, dialect, None);
        }
        return (name.clone(), ATOM);
    }
    if let Some(literal) = &expression.literal {
        return (number(literal, dialect), ATOM);
    }
    if let Some(call) = &expression.call {
        let mut arguments = Vec::new();
        for argument in &call.args {
            arguments.push(printed(argument, dialect, resolve).0);
        }
        return (format!("{}({})", call.name, arguments.join(", ")), CALL);
    }
    if let Some(unary) = &expression.unary {
        let (operand, strength) = child_text(&unary.operand, dialect, resolve);
        let operand = wrap_in_parentheses(operand, strength < CALL);
        return (format!("{}{operand}", operator_text(unary.op).0), UNARY);
    }
    if let Some(binary) = &expression.binary {
        let (symbol, strength) = operator_text(binary.op);
        let right_associative = binary.op == Operator::Pow; // `**` groups to the right
        let (lhs, lhs_strength) = child_text(&binary.lhs, dialect, resolve);
        let (rhs, rhs_strength) = child_text(&binary.rhs, dialect, resolve);
        let lhs = wrap_in_parentheses(
            lhs,
            lhs_strength < strength || (lhs_strength == strength && right_associative),
        );
        let rhs = wrap_in_parentheses(
            rhs,
            rhs_strength < strength || (rhs_strength == strength && !right_associative),
        );
        return (format!("{lhs} {symbol} {rhs}"), strength);
    }
    if let Some(ternary) = &expression.ternary {
        let (condition, condition_strength) = child_text(&ternary.cond, dialect, resolve);
        let (then, then_strength) = child_text(&ternary.then, dialect, resolve);
        let (otherwise, otherwise_strength) = child_text(&ternary.otherwise, dialect, resolve);
        let condition = wrap_in_parentheses(condition, condition_strength <= TERNARY);
        let then = wrap_in_parentheses(then, then_strength == RAW);
        let otherwise = wrap_in_parentheses(otherwise, otherwise_strength == RAW);
        return (format!("{condition} ? {then} : {otherwise}"), TERNARY);
    }
    let raw = expression.raw.as_deref().unwrap_or("");
    match resolve {
        // unparsed: substitute words, as nyancad does
        Some(lookup) if !raw.is_empty() => {
            let substitute_word = |word: &Captures| match lookup(&word[0]) {
                Some(substitute) => printed(&substitute, dialect, None).0,
                None => word[0].to_string(),
            };
            (
                word_re().replace_all(raw, substitute_word).into_owned(),
                RAW,
            )
        }
        _ => (raw.to_string(), RAW),
    }
}

/// Canonical text of `expression` for `dialect`.
///
/// ngspice/xyce wrap a compound expression in `{…}`, hspice in `'…'`; spectre, vacask,
/// systemverilog and vams print it bare. A literal or a reference is never wrapped. Raw
/// text is wrapped only as the value of a `named` (`k=v`) param, and not when it is
/// empty, already wrapped, or a bare word or number; a positional value (`DC 1 AC 1`)
/// stays verbatim. `resolve(name)` may substitute a reference, or a word of raw text;
/// the substitute is printed as is, not resolved again.
pub fn format_expression(
    expression: &Expression,
    dialect: Dialect,
    resolve: Option<Resolve>,
    named: bool,
) -> String {
    let (opening, closing) = dialect.wrapping();
    let (text, strength) = printed(expression, dialect, resolve);
    if strength == ATOM || (strength == RAW && !named) {
        return text;
    }
    if strength == RAW && (text.is_empty() || is_wrapped(&text) || is_single_token(&text)) {
        return text;
    }
    format!("{opening}{text}{closing}")
}

/// Python's `text[:1] + text[-1:] in ("{}", "''")`.
fn is_wrapped(text: &str) -> bool {
    matches!(
        (text.chars().next(), text.chars().last()),
        (Some('{'), Some('}')) | (Some('\''), Some('\''))
    )
}

fn is_single_token(text: &str) -> bool {
    match parse_expression(text) {
        Ok(token) => token.literal.is_some() || token.reference.is_some(),
        Err(_) => false,
    }
}
