//! Parameter expressions ported from kfnetlist d0daf3e, 1adb071, c12a71e and d94dc38.
//! from: kfnetlist tests/morphnet/test_expression.py, test_parser_expressions.py (with its
//! `[sv]` parametrization, 9766e84) and the
//! unit tests of crates/kfnetlist-schema/src/expression.rs

use std::collections::HashMap;

use oxymorph_netlist::expression::{
    format_expression, parameter_value, parse_expression, Dialect, MAX_EXPRESSION_DEPTH,
};
use oxymorph_netlist::hdl::HdlOptions;
use oxymorph_netlist::{
    parse_hspice, parse_spectre, parse_spice, parse_systemverilog, parse_xyce, write_hspice,
    write_spectre, write_spice, write_systemverilog, write_xyce, Error,
};
use oxymorph_schema::{
    BinaryOp, Call, Circuit, Expression, ModelReference, Module, ModuleReference, Operator,
    Parameter, ParameterValue, PrefixedValue, Siprefix, Ternary, UnaryOp,
};

fn parsed(text: &str) -> Expression {
    parse_expression(text).unwrap()
}

fn canonical(text: &str) -> String {
    format_expression(&parsed(text), Dialect::Spectre, None, false)
}

fn raw(text: &str) -> Expression {
    Expression {
        raw: Some(text.to_string()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// test_expression.py
// ---------------------------------------------------------------------------

#[test]
fn binary_operators_print_with_minimal_parentheses() {
    for (text, printed) in [
        ("0.13*1e-6", "0.13 * 1e-6"),
        ("a+b*c", "a + b * c"),
        ("(a+b)*c", "(a + b) * c"),
        ("a-b-c", "a - b - c"),
        ("a-(b-c)", "a - (b - c)"),
        ("a/(b*c)", "a / (b * c)"),
        ("a%b", "a % b"),
        ("2**3**2", "2 ** 3 ** 2"),
        ("(2**3)**2", "(2 ** 3) ** 2"),
        ("((a))", "a"),
    ] {
        assert_eq!(canonical(text), printed, "{text}");
    }
}

#[test]
fn unary_operators() {
    for (text, printed) in [
        ("-a", "-a"),
        ("+a", "a"),
        ("!a", "!a"),
        ("-(a+b)", "-(a + b)"),
        ("-a**2", "-(a ** 2)"),
        ("(-a)**2", "(-a) ** 2"),
        ("2**-1", "2 ** (-1)"),
        ("- -a", "-(-a)"),
        ("-a*b", "-a * b"),
    ] {
        assert_eq!(canonical(text), printed, "{text}");
    }
}

#[test]
fn comparisons_logic_and_ternary() {
    for (text, printed) in [
        ("a<b && c>=d || !e", "a < b && c >= d || !e"),
        ("a==b != c", "a == b != c"),
        ("a<=b", "a <= b"),
        ("a&&(b||c)", "a && (b || c)"),
        ("a>b?a:b", "a > b ? a : b"),
        ("a?b:c?d:e", "a ? b : c ? d : e"),
        ("(a?b:c)?d:e", "(a ? b : c) ? d : e"),
        ("(a?b:c)+1", "(a ? b : c) + 1"),
    ] {
        assert_eq!(canonical(text), printed, "{text}");
    }
}

#[test]
fn calls() {
    for (text, printed) in [
        ("max(a,2*b)", "max(a, 2 * b)"),
        ("f()", "f()"),
        ("-sqrt(x)", "-sqrt(x)"),
        ("$clog2(N)", "$clog2(N)"),
    ] {
        assert_eq!(canonical(text), printed, "{text}");
    }
}

#[test]
fn si_literals_keep_their_prefix() {
    let expression = parsed("2.2u*w");
    let lhs = expression.binary.as_ref().unwrap().lhs.as_ref().unwrap();
    assert_eq!(
        lhs.literal,
        Some(PrefixedValue {
            double_value: 2.2,
            prefix: Siprefix::Micro
        })
    );
    assert_eq!(canonical("2.2u*w"), "2.2u * w");
    assert_eq!(canonical("1meg"), "1meg");
    assert_eq!(
        format_expression(&expression, Dialect::SystemVerilog, None, false),
        "2.2e-6 * w"
    );
}

#[test]
fn unprefixed_numbers_print_shortest() {
    for (text, printed) in [
        ("1e-06", "1e-6"),
        ("2.0", "2"),
        ("1e16", "1e16"),
        ("1.5e+20", "1.5e20"),
    ] {
        assert_eq!(canonical(text), printed, "{text}");
    }
}

#[test]
fn one_outer_wrapper_is_stripped() {
    for text in ["{0.13 * 1e-6}", "'0.13*1e-6'", " 0.13*1e-6 "] {
        assert_eq!(parsed(text), parsed("0.13*1e-6"), "{text}");
    }
}

#[test]
fn unparseable_text_is_kept_raw_and_printed_verbatim() {
    for text in [
        "DC 1 AC 1",
        "\"pulse\"",
        "{{a}}",
        "a +",
        "",
        "{a} + {b}",
        "1_000",
        "a^b",
    ] {
        let expression = parsed(text);
        assert_eq!(expression, raw(text));
        for dialect in [Dialect::Ngspice, Dialect::Hspice, Dialect::Spectre] {
            assert_eq!(format_expression(&expression, dialect, None, false), text);
        }
    }
}

#[test]
fn dialects_wrap_compound_expressions() {
    for (dialect, printed) in [
        (Dialect::Ngspice, "{0.13 * 1e-6}"),
        (Dialect::Xyce, "{0.13 * 1e-6}"),
        (Dialect::Hspice, "'0.13 * 1e-6'"),
        (Dialect::Spectre, "0.13 * 1e-6"),
        (Dialect::Vacask, "0.13 * 1e-6"),
        (Dialect::SystemVerilog, "0.13 * 1e-6"),
        (Dialect::Vams, "0.13 * 1e-6"),
    ] {
        assert_eq!(
            format_expression(&parsed("0.13*1e-6"), dialect, None, false),
            printed
        );
    }
}

#[test]
fn a_bare_literal_or_reference_is_never_wrapped() {
    for text in ["w", "2.2u", "{w}"] {
        assert_eq!(
            format_expression(&parsed(text), Dialect::Ngspice, None, false),
            text.trim_matches(|character| character == '{' || character == '}')
        );
    }
}

#[test]
fn calls_and_unary_are_wrapped() {
    assert_eq!(
        format_expression(&parsed("max(a,b)"), Dialect::Hspice, None, false),
        "'max(a, b)'"
    );
    assert_eq!(
        format_expression(&parsed("-a"), Dialect::Xyce, None, false),
        "{-a}"
    );
}

#[test]
fn resolve_substitutes_references() {
    let values: HashMap<&str, Expression> =
        HashMap::from([("W", parsed("0.15*1e-6")), ("N", parsed("2"))]);
    let resolve = |name: &str| values.get(name).cloned();
    assert_eq!(
        format_expression(&parsed("W*N"), Dialect::Ngspice, Some(&resolve), false),
        "{0.15 * 1e-6 * 2}"
    );
    assert_eq!(
        format_expression(&parsed("N*W"), Dialect::Spectre, Some(&resolve), false),
        "2 * (0.15 * 1e-6)"
    );
    // a reference resolving to a literal is bare; unresolved names stay
    assert_eq!(
        format_expression(&parsed("N"), Dialect::Ngspice, Some(&resolve), false),
        "2"
    );
    assert_eq!(
        format_expression(&parsed("L"), Dialect::Ngspice, Some(&resolve), false),
        "L"
    );
}

#[test]
fn a_built_tree_prints_like_its_parsed_text() {
    let tree = Expression {
        binary: Some(BinaryOp {
            op: Operator::Mul,
            lhs: Some(Box::new(Expression::literal(0.13))),
            rhs: Some(Box::new(Expression::literal(1e-6))),
        }),
        ..Default::default()
    };
    assert_eq!(parsed("0.13 * 1e-6"), tree);
    assert_eq!(
        format_expression(&tree, Dialect::Spectre, None, false),
        "0.13 * 1e-6"
    );
}

#[test]
fn canonical_text_round_trips() {
    for text in [
        "a + b * c",
        "(a + b) * c",
        "-(a ** 2)",
        "a > b ? max(a, 1k) : -b",
        "2.2u * w",
    ] {
        assert_eq!(canonical(&canonical(text)), canonical(text));
        assert_eq!(canonical(text), text);
    }
}

#[test]
fn raw_text_of_a_named_param_is_wrapped() {
    for (dialect, printed) in [
        (Dialect::Ngspice, "{x^2}"),
        (Dialect::Xyce, "{x^2}"),
        (Dialect::Hspice, "'x^2'"),
        (Dialect::Vacask, "x^2"),
    ] {
        assert_eq!(
            format_expression(&parsed("x^2"), dialect, None, true),
            printed
        );
        // positional
        assert_eq!(
            format_expression(&parsed("x^2"), dialect, None, false),
            "x^2"
        );
    }
}

#[test]
fn raw_text_already_wrapped_is_not_wrapped_again() {
    for text in ["{x^2}", "'x^2'"] {
        // e.g. a bridged kfnetlist setting
        assert_eq!(
            format_expression(&raw(text), Dialect::Ngspice, None, true),
            text
        );
    }
}

#[test]
fn a_raw_bare_word_or_number_stays_bare() {
    for text in ["fast", "true", "1k"] {
        assert_eq!(
            format_expression(&raw(text), Dialect::Ngspice, None, true),
            text
        );
    }
}

#[test]
fn an_unknown_dialect_is_a_value_error() {
    let error = "verilog-a".parse::<Dialect>().unwrap_err();
    assert!(error.to_string().contains("ngspice"), "{error}");
    assert_eq!("vacask".parse::<Dialect>(), Ok(Dialect::Vacask));
}

// ---------------------------------------------------------------------------
// Review focus and the depth limit
// ---------------------------------------------------------------------------

#[test]
fn expression_round_trips_in_every_dialect() {
    // The brief's texts without spaces (`{a*2}`) are not the canonical form: the
    // printer spaces binary operators, as kfnetlist's does.
    let expr = parse_expression("a*2").unwrap();
    for (dialect, text) in [
        (Dialect::Ngspice, "{a * 2}"),
        (Dialect::Xyce, "{a * 2}"),
        (Dialect::Hspice, "'a * 2'"),
        (Dialect::Spectre, "a * 2"),
        (Dialect::Vacask, "a * 2"),
        (Dialect::SystemVerilog, "a * 2"),
        (Dialect::Vams, "a * 2"),
    ] {
        assert_eq!(format_expression(&expr, dialect, None, false), text);
        assert_eq!(
            parse_expression(text.trim_matches(|c| c == '{' || c == '}' || c == '\'')).unwrap(),
            expr
        );
        assert_eq!(parse_expression(text).unwrap(), expr);
    }
}

#[test]
fn expression_depth_is_limited() {
    let nested_parentheses = |depth: usize| format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
    let nested_negations = |depth: usize| format!("{}a", "-".repeat(depth));
    let sum = |terms: usize| vec!["a"; terms].join(" + ");
    // the tree's depth counts, not the text's characters: `((a))` is one node
    assert_eq!(
        parse_expression(&nested_parentheses(10_000)).unwrap(),
        Expression::reference("a")
    );
    let deepest = MAX_EXPRESSION_DEPTH - 1;
    let mut negations = parse_expression(&nested_negations(deepest)).unwrap();
    for _ in 0..deepest {
        let unary = negations.unary.expect("a negation");
        assert_eq!(unary.op, Operator::Neg);
        negations = *unary.operand.unwrap();
    }
    assert_eq!(negations, Expression::reference("a"));
    assert!(parse_expression(&sum(MAX_EXPRESSION_DEPTH)).is_ok());
    // deep text is refused without overflowing the stack
    for text in [
        nested_negations(MAX_EXPRESSION_DEPTH),
        sum(MAX_EXPRESSION_DEPTH + 1),
        nested_negations(100_000),
        sum(100_000),
    ] {
        assert!(matches!(parse_expression(&text), Err(Error::Validation(_))));
    }
    // two-character operators and exponents count once per node
    for operator in [" ** ", " && ", " <= "] {
        let chain = vec!["1e-6"; MAX_EXPRESSION_DEPTH].join(operator);
        assert!(parse_expression(&chain).is_ok(), "{chain}");
    }
    // a parser keeps over-deep text verbatim rather than failing the netlist
    let circuit = parse_spectre(&format!(
        "subckt cell (p n)\nR1 (p n) resistor w={}\nends cell\n",
        nested_negations(200)
    ))
    .unwrap();
    assert_eq!(param(&circuit, "w"), Some(raw(&nested_negations(200))));
}

/// `f(f(…f(1e-6)…))`: a tree `depth` nodes deep, every inner node a call (the costliest
/// node in JSON) and the leaf a literal (the costliest leaf).
fn nested_calls(depth: usize) -> String {
    format!("{}1e-6{}", "f(".repeat(depth - 1), ")".repeat(depth - 1))
}

/// A circuit holding `value` where the IR nests a value deepest: a model reference argument
/// of an instance parameter, and as the parameter's own value.
fn circuit_holding(value: &ParameterValue) -> Circuit {
    let model_ref = ParameterValue {
        model_ref: Some(ModelReference {
            model_interface_name: "m".to_string(),
            arguments: [("w".to_string(), value.clone())].into_iter().collect(),
        }),
        ..Default::default()
    };
    let parameters = vec![
        Parameter {
            name: "r".to_string(),
            default_value: Some(value.clone()),
            ..Default::default()
        },
        Parameter {
            name: "model".to_string(),
            default_value: Some(model_ref),
            ..Default::default()
        },
    ];
    Circuit {
        top_module: "cell".to_string(),
        modules: vec![Module {
            name: "cell".to_string(),
            module_references: vec![ModuleReference {
                name: "R1".to_string(),
                module_name: "resistor".to_string(),
                parameters,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn assert_round_trips(circuit: &Circuit) {
    let json = circuit.to_json().unwrap();
    assert_eq!(&Circuit::from_json(&json).unwrap(), circuit, "JSON");
    let yaml = circuit.to_yaml().unwrap();
    assert_eq!(&Circuit::from_yaml(&yaml).unwrap(), circuit, "YAML");
    let proto = circuit.to_proto_bytes();
    assert_eq!(
        &Circuit::from_proto_bytes(&proto).unwrap(),
        circuit,
        "proto"
    );
}

/// The parsers build trees at most `MAX_EXPRESSION_DEPTH` deep, which every interchange
/// format reads back; deeper text stays `raw`, which round-trips too.
#[test]
fn expression_at_the_bound_round_trips() {
    let at_bound = parameter_value(&nested_calls(MAX_EXPRESSION_DEPTH));
    let expression = at_bound.expression.as_ref().unwrap();
    assert!(expression.call.is_some(), "{expression:?}");
    assert_round_trips(&circuit_holding(&at_bound));
    let text = format!("{{{}}}", nested_calls(MAX_EXPRESSION_DEPTH));
    let deck = format!(".subckt cell p n\nR1 p n r={text}\n.ends cell\n.end\n");
    let parsed = parse_spice(&deck).unwrap();
    assert_eq!(param(&parsed, "r"), at_bound.expression);
    assert_eq!(parse_spice(&write_spice(&parsed)).unwrap(), parsed);

    let over_text = nested_calls(MAX_EXPRESSION_DEPTH + 1);
    assert!(matches!(
        parse_expression(&over_text),
        Err(Error::Validation(_))
    ));
    let over_bound = parameter_value(&over_text);
    assert_eq!(over_bound.expression, Some(raw(&over_text)));
    assert_round_trips(&circuit_holding(&over_bound));
}

// ---------------------------------------------------------------------------
// kfnetlist-schema src/expression.rs unit tests (the canonical printer)
// ---------------------------------------------------------------------------

fn number(double_value: f64, prefix: Siprefix) -> Expression {
    Expression {
        literal: Some(PrefixedValue {
            double_value,
            prefix,
        }),
        ..Default::default()
    }
}

fn binary(op: Operator, lhs: Expression, rhs: Expression) -> Expression {
    Expression {
        binary: Some(BinaryOp {
            op,
            lhs: Some(Box::new(lhs)),
            rhs: Some(Box::new(rhs)),
        }),
        ..Default::default()
    }
}

fn negation(operand: Expression) -> Expression {
    Expression {
        unary: Some(UnaryOp {
            op: Operator::Neg,
            operand: Some(Box::new(operand)),
        }),
        ..Default::default()
    }
}

fn unwrapped(expression: &Expression) -> String {
    format_expression(expression, Dialect::Spectre, None, false)
}

#[test]
fn numbers_print_like_nyancad() {
    for (expression, printed) in [
        (number(0.13, Siprefix::Unspecified), "0.13"),
        (number(1e-6, Siprefix::Unspecified), "1e-6"),
        (number(2.0, Siprefix::Unspecified), "2"),
        (number(1e16, Siprefix::Unspecified), "1e16"),
        (number(1.5e20, Siprefix::Unspecified), "1.5e20"),
        (number(0.0001, Siprefix::Unspecified), "0.0001"),
        (number(2.2, Siprefix::Micro), "2.2u"),
        (number(1.0, Siprefix::Mega), "1meg"),
        // CENTI has no SPICE suffix: Python's format_si_value drops it (the Rust
        // printer scaled it to 0.03); Python wins (ADR 0011).
        (number(3.0, Siprefix::Centi), "3"),
    ] {
        assert_eq!(unwrapped(&expression), printed);
    }
}

#[test]
fn operators_print_with_minimal_parentheses() {
    let (a, b, c) = (
        Expression::reference("a"),
        Expression::reference("b"),
        Expression::reference("c"),
    );
    let cases = [
        (
            binary(
                Operator::Mul,
                Expression::literal(0.13),
                Expression::literal(1e-6),
            ),
            "0.13 * 1e-6",
        ),
        (
            binary(
                Operator::Mul,
                binary(Operator::Add, a.clone(), b.clone()),
                c.clone(),
            ),
            "(a + b) * c",
        ),
        (
            binary(
                Operator::Add,
                a.clone(),
                binary(Operator::Mul, b.clone(), c.clone()),
            ),
            "a + b * c",
        ),
        (
            binary(
                Operator::Sub,
                a.clone(),
                binary(Operator::Sub, b.clone(), c.clone()),
            ),
            "a - (b - c)",
        ),
        (
            binary(
                Operator::Sub,
                binary(Operator::Sub, a.clone(), b.clone()),
                c.clone(),
            ),
            "a - b - c",
        ),
        (
            binary(
                Operator::Pow,
                a.clone(),
                binary(Operator::Pow, b.clone(), c.clone()),
            ),
            "a ** b ** c",
        ),
        (
            binary(
                Operator::Pow,
                binary(Operator::Pow, a.clone(), b.clone()),
                c.clone(),
            ),
            "(a ** b) ** c",
        ),
        (
            negation(binary(Operator::Pow, a.clone(), b.clone())),
            "-(a ** b)",
        ),
        (
            binary(Operator::Pow, negation(a.clone()), b.clone()),
            "(-a) ** b",
        ),
        (
            binary(Operator::Mul, negation(a.clone()), b.clone()),
            "-a * b",
        ),
        (
            binary(
                Operator::Or,
                binary(Operator::Lt, a.clone(), b.clone()),
                c.clone(),
            ),
            "a < b || c",
        ),
    ];
    for (expression, printed) in cases {
        assert_eq!(unwrapped(&expression), printed);
    }
}

fn pick(condition: Expression) -> Expression {
    let maximum = Expression {
        call: Some(Call {
            name: "max".to_string(),
            args: vec![Expression::reference("a"), number(1.0, Siprefix::Kilo)],
        }),
        ..Default::default()
    };
    Expression {
        ternary: Some(Ternary {
            cond: Some(Box::new(condition)),
            then: Some(Box::new(maximum)),
            otherwise: Some(Box::new(negation(Expression::reference("b")))),
        }),
        ..Default::default()
    }
}

#[test]
fn calls_ternaries_and_raw_text() {
    let greater = binary(
        Operator::Gt,
        Expression::reference("a"),
        Expression::reference("b"),
    );
    assert_eq!(unwrapped(&pick(greater)), "a > b ? max(a, 1k) : -b");
    assert_eq!(
        unwrapped(&pick(pick(Expression::reference("c")))),
        "(c ? max(a, 1k) : -b) ? max(a, 1k) : -b"
    );
    assert_eq!(unwrapped(&raw("DC 1 AC 1")), "DC 1 AC 1");
    assert_eq!(
        unwrapped(&binary(
            Operator::Add,
            raw("DC 1 AC 1"),
            Expression::reference("x")
        )),
        "(DC 1 AC 1) + x"
    );
    assert_eq!(unwrapped(&Expression::default()), "");
}

// ---------------------------------------------------------------------------
// test_parser_expressions.py
// ---------------------------------------------------------------------------

const SPICE: &str = "\
.subckt cell p n a=1 b=2
R1 p n 1k w={a*b+1} l=a
.ends cell
.end
";
const SPECTRE: &str = "\
subckt cell (p n) a=1 b=2
R1 (p n) resistor r=1k w=a*b+1 l=a
ends cell
";
const SV: &str = "\
module cell #(parameter a = 1, parameter b = 2) (input p, n);
  res #(.w(a*b+1), .l(a)) R1 (p, n);
endmodule
";

fn parse_sv(text: &str) -> Result<Circuit, Error> {
    parse_systemverilog(text, &HdlOptions::default())
}

type Parser = fn(&str) -> Result<Circuit, Error>;
type Writer = fn(&Circuit) -> String;

fn dialects() -> Vec<(Parser, Writer, String, &'static str)> {
    vec![
        (
            parse_spice as Parser,
            write_spice as Writer,
            SPICE.to_string(),
            "w={a * b + 1}",
        ),
        (
            parse_hspice,
            write_hspice,
            SPICE.replace("{a*b+1}", "'a*b+1'"),
            "w='a * b + 1'",
        ),
        (
            parse_xyce,
            write_xyce,
            SPICE.replace("n a=1", "n PARAMS: a=1"),
            "w={a * b + 1}",
        ),
        (
            parse_spectre,
            write_spectre,
            SPECTRE.to_string(),
            "w=a * b + 1",
        ),
        (
            parse_sv,
            write_systemverilog,
            SV.to_string(),
            ".w(a * b + 1)",
        ),
    ]
}

fn param(circuit: &Circuit, name: &str) -> Option<Expression> {
    let cell = circuit
        .modules
        .iter()
        .find(|module| module.name == "cell")
        .expect("a cell module");
    let instance = cell
        .module_references
        .iter()
        .find(|reference| reference.name == "R1")
        .expect("an R1 instance");
    let parameter = instance
        .parameters
        .iter()
        .find(|parameter| parameter.name == name)
        .expect("the parameter");
    parameter.default_value.clone().expect("a value").expression
}

#[test]
fn expression_round_trips() {
    for (parse, write, text, written) in dialects() {
        let first = parse(&text).unwrap();
        assert_eq!(param(&first, "w"), Some(parsed("a*b+1")), "{text}");
        let out = write(&first);
        assert!(out.contains(written), "{written} not in {out}");
        assert_eq!(param(&parse(&out).unwrap(), "w"), param(&first, "w"));
    }
}

#[test]
fn bare_identifier_is_a_reference() {
    for (parse, _, text, _) in dialects() {
        assert_eq!(
            param(&parse(&text).unwrap(), "l"),
            Some(Expression::reference("a"))
        );
    }
}

#[test]
fn spectre_bare_expression_ends_at_the_next_param() {
    for value in [
        "a * b + 1",
        "max(a, b) * 2",
        "(a + 1) * 2",
        "a > b ? a : b",
        "-a",
    ] {
        let text = format!("subckt cell (p n)\nR1 (p n) resistor w={value} l=a\nends cell\n");
        let circuit = parse_spectre(&text).unwrap();
        assert_eq!(param(&circuit, "w"), Some(parsed(value)), "{value}");
        assert_eq!(param(&circuit, "l"), Some(Expression::reference("a")));
    }
}

#[test]
fn spectre_model_expression_stops_at_the_closing_paren() {
    let circuit = parse_spectre("model m resistor (w=(a+1)*2 l=b)\n").unwrap();
    let [ext_module] = circuit.ext_modules.as_slice() else {
        panic!("one model");
    };
    let values: Vec<Option<Expression>> = ext_module
        .parameters
        .iter()
        .map(|parameter| parameter.default_value.clone().unwrap().expression)
        .collect();
    assert_eq!(
        values,
        [Some(parsed("(a+1)*2")), Some(Expression::reference("b"))]
    );
}

#[test]
fn plain_numbers_stay_prefixed_values() {
    for circuit in [parse_spice(SPICE).unwrap(), parse_spectre(SPECTRE).unwrap()] {
        let cell = circuit
            .modules
            .iter()
            .find(|module| module.name == "cell")
            .unwrap();
        for parameter in &cell.parameters {
            assert!(parameter
                .default_value
                .as_ref()
                .unwrap()
                .prefixed_value
                .is_some());
        }
    }
}

#[test]
fn param_directive_keeps_its_expression_text() {
    let cases: [(Parser, Writer, &str, &str); 4] = [
        (
            parse_spice,
            write_spice,
            ".param x={a*b+1}\n.end\n",
            ".param x={a * b + 1}",
        ),
        (
            parse_hspice,
            write_hspice,
            ".param x='a*b+1'\n.end\n",
            ".param x='a * b + 1'",
        ),
        (
            parse_xyce,
            write_xyce,
            ".param x={a*b+1}\n.end\n",
            ".param x={a * b + 1}",
        ),
        (
            parse_spectre,
            write_spectre,
            "parameters x=a*b+1\n",
            "parameters x=a * b + 1",
        ),
    ];
    for (parse, write, text, written) in cases {
        let out = write(&parse(text).unwrap());
        assert!(out.contains(written), "{written} not in {out}");
        assert_eq!(write(&parse(&out).unwrap()), out);
    }
}
