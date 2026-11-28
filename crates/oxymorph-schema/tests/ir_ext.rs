//! IR types added by the kfnetlist port (expressions, macros, stimuli), from
//! kfnetlist `tests/morphnet/test_circuit_models.py` and
//! `crates/kfnetlist-schema/tests/schema.rs` (`decisions/0011`).

use oxymorph_schema::*;
use serde_json::json;

fn number(value: f64) -> ParameterValue {
    ParameterValue::prefixed(value, Siprefix::Unspecified)
}

fn raw(text: &str) -> Expression {
    Expression {
        raw: Some(text.to_string()),
        ..Default::default()
    }
}

fn boxed(expression: Expression) -> Option<Box<Expression>> {
    Some(Box::new(expression))
}

fn parameter_with(name: &str, value: ParameterValue) -> Parameter {
    Parameter {
        name: name.to_string(),
        default_value: Some(value),
        ..Default::default()
    }
}

fn circuit_with_module(module: Module) -> Circuit {
    Circuit {
        top_module: module.name.clone(),
        modules: vec![module],
        ..Default::default()
    }
}

fn spice_macro(name: &str, body: &str) -> Macro {
    Macro {
        name: name.to_string(),
        language: "spice".to_string(),
        body: body.to_string(),
        properties: [("origin".to_string(), "nyanlib".to_string())]
            .into_iter()
            .collect(),
    }
}

fn assert_cross_format_round_trip(circuit: &Circuit) {
    assert_eq!(
        &Circuit::from_json(&circuit.to_json().unwrap()).unwrap(),
        circuit
    );
    assert_eq!(
        &Circuit::from_yaml(&circuit.to_yaml().unwrap()).unwrap(),
        circuit
    );
    assert_eq!(
        &Circuit::from_proto_bytes(&circuit.to_proto_bytes()).unwrap(),
        circuit
    );
}

// ----------------------------------------------------------------------
// test_circuit_models.py :: TestExpression
// ----------------------------------------------------------------------

#[test]
fn rejects_two_nodes() {
    let expression = Expression {
        reference: Some("a".to_string()),
        raw: Some("b".to_string()),
        ..Default::default()
    };
    let error = expression.validate().unwrap_err();
    assert!(error.contains("oneof"), "{error}");
}

#[test]
fn rejects_expression_beside_a_number() {
    let mut value = ParameterValue::prefixed(1.0, Siprefix::Unspecified);
    value.expression = Some(raw(""));
    let error = value.validate().unwrap_err();
    assert!(error.contains("oneof"), "{error}");
}

#[test]
fn tree_roundtrips_proto_binary() {
    let x = Expression::reference("x");
    let tree = Expression {
        ternary: Some(Ternary {
            cond: boxed(Expression {
                binary: Some(BinaryOp {
                    op: Operator::Gt,
                    lhs: boxed(x.clone()),
                    rhs: boxed(x.clone()),
                }),
                ..Default::default()
            }),
            then: boxed(Expression {
                unary: Some(UnaryOp {
                    op: Operator::Neg,
                    operand: boxed(x.clone()),
                }),
                ..Default::default()
            }),
            otherwise: boxed(Expression {
                call: Some(Call {
                    name: "max".to_string(),
                    args: vec![Expression::literal(1.5), raw("")],
                }),
                ..Default::default()
            }),
        }),
        ..Default::default()
    };
    let value = ParameterValue {
        expression: Some(tree),
        ..Default::default()
    };
    assert!(value.validate().is_ok());
    let module = Module {
        name: "top".to_string(),
        parameters: vec![parameter_with("w", value)],
        ..Default::default()
    };
    let circuit = circuit_with_module(module);
    assert_eq!(
        Circuit::from_proto_bytes(&circuit.to_proto_bytes()).unwrap(),
        circuit
    );
}

// ----------------------------------------------------------------------
// test_circuit_models.py :: JSON shared with the Rust schema
// ----------------------------------------------------------------------

#[test]
fn expression_json_is_flat() {
    assert_eq!(
        serde_json::to_value(Expression::reference("w")).unwrap(),
        json!({"reference": "w"})
    );
    let text = r#"{"binary": {"op": 3, "lhs": {"reference": "w"},
        "rhs": {"literal": {"double_value": 2.0, "prefix": 0}}}}"#;
    let expression: Expression = serde_json::from_str(text).unwrap();
    let binary = expression.binary.as_ref().unwrap();
    assert_eq!(binary.op, Operator::Mul);
    assert_eq!(binary.lhs.as_deref(), Some(&Expression::reference("w")));
    assert_eq!(binary.rhs.as_deref(), Some(&Expression::literal(2.0)));
    assert_eq!(
        serde_json::to_value(&expression).unwrap(),
        serde_json::from_str::<serde_json::Value>(text).unwrap()
    );
}

#[test]
fn json_omits_unset_oneofs_and_empty_macros() {
    let module = Module {
        name: "top".to_string(),
        parameters: vec![parameter_with("w", number(1.0))],
        ..Default::default()
    };
    let circuit = Circuit {
        modules: vec![module],
        ..Default::default()
    };
    // kfnetlist also drops every null; the pre-fork corpus pins morphnet's nulls on
    // the original fields, so only the fields this port adds are omitted when unset.
    for text in [circuit.to_json().unwrap(), circuit.to_yaml().unwrap()] {
        assert!(!text.contains("macros"), "{text}");
        assert!(!text.contains("expression"), "{text}");
        assert!(!text.contains("stimuli"), "{text}");
    }
    assert_eq!(
        Circuit::from_json(&circuit.to_json().unwrap()).unwrap(),
        circuit
    );
    assert_eq!(
        Circuit::from_yaml(&circuit.to_yaml().unwrap()).unwrap(),
        circuit
    );
}

#[test]
fn stimulus_json_round_trips_through_the_rust_schema() {
    let stimulus = Stimulus {
        name: "__top__/V1".to_string(),
        dc: Some(number(0.9)),
        ac_magnitude: Some(number(1.0)),
        waveform: Some(Waveform {
            kind: WaveformKind::Pulse,
            args: [
                ("val0".to_string(), number(0.0)),
                (
                    "val1".to_string(),
                    ParameterValue {
                        expression: Some(Expression::reference("vdd")),
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let pwl = Stimulus {
        name: "__top__/I1".to_string(),
        waveform: Some(Waveform {
            kind: WaveformKind::Pwl,
            points: vec![number(0.0), number(0.0), number(1e-9), number(1.0)],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut circuit = circuit_with_module(Module {
        name: "__top__".to_string(),
        ..Default::default()
    });
    circuit.simulation = Some(Simulation {
        stimuli: vec![stimulus, pwl],
        ..Default::default()
    });
    assert_cross_format_round_trip(&circuit);
    let text = circuit.to_json().unwrap();
    assert!(!text.contains("ac_phase"), "{text}");
    assert!(!text.contains("\"file\""), "{text}");
}

#[test]
fn a_pwl_file_is_a_path_stored_posix() {
    let wave: Waveform = serde_json::from_value(json!({"kind": 5, "file": "data\\w.txt"})).unwrap();
    assert_eq!(wave.file.as_deref(), Some("data/w.txt"));
    let mut circuit = circuit_with_module(Module {
        name: "__top__".to_string(),
        ..Default::default()
    });
    circuit.simulation = Some(Simulation {
        stimuli: vec![Stimulus {
            name: "__top__/V1".to_string(),
            waveform: Some(wave),
            ..Default::default()
        }],
        ..Default::default()
    });
    assert!(circuit
        .to_json()
        .unwrap()
        .replace(' ', "")
        .contains(r#""file":"data/w.txt""#));
    assert_cross_format_round_trip(&circuit);
    let both = Waveform {
        kind: WaveformKind::Pwl,
        file: Some("w.txt".to_string()),
        points: vec![number(0.0)],
        ..Default::default()
    };
    let error = both.validate().unwrap_err();
    assert!(error.contains("points or a file"), "{error}");
}

#[test]
fn a_file_belongs_to_a_pwl_waveform_only() {
    let wave = Waveform {
        kind: WaveformKind::Pulse,
        file: Some("w.txt".to_string()),
        ..Default::default()
    };
    let error = wave.validate().unwrap_err();
    assert!(error.contains("PWL"), "{error}");
}

// ----------------------------------------------------------------------
// kfnetlist-schema tests/schema.rs
// ----------------------------------------------------------------------

#[test]
fn wire_matches_a_fixed_protobuf_fixture() {
    // Circuit.name = field 1, Circuit.top_module = field 3; wire type 2.
    let bytes = b"\x0a\x04demo\x1a\x03top";
    let circuit = Circuit::from_proto_bytes(bytes).unwrap();
    assert_eq!(circuit.name, "demo");
    assert_eq!(circuit.top_module, "top");
    assert_eq!(circuit.to_proto_bytes(), bytes);

    // modules (10) > connections (50) > source (10): TerminalReference {a, p}; then an
    // unknown varint field 99 and an unknown fixed32 field 98, which are skipped.
    let bytes = [
        &[0x52, 11, 0x92, 0x03, 8, 0x52, 6][..],
        b"\x0a\x01a\x12\x01p",
        &[0x98, 0x06, 1, 0x95, 0x06, 0, 0, 0, 0],
    ]
    .concat();
    let circuit = Circuit::from_proto_bytes(&bytes).unwrap();
    let source = circuit.modules[0].connections[0].source.as_ref().unwrap();
    assert_eq!(
        (source.instance_name.as_str(), source.port_name.as_str()),
        ("a", "p")
    );
    assert_eq!(circuit.to_proto_bytes(), bytes[..13]);

    // simulation (40) > temperatures (6): packed (as written) or one field per value
    let (one, two) = (1.0f64.to_le_bytes(), 2.0f64.to_le_bytes());
    let packed = [&[0xc2, 0x02, 18, 0x32, 16][..], &one, &two].concat();
    let unpacked = [&[0xc2, 0x02, 18, 0x31][..], &one, &[0x31], &two].concat();
    for bytes in [&packed, &unpacked] {
        let circuit = Circuit::from_proto_bytes(bytes).unwrap();
        assert_eq!(
            circuit.simulation.as_ref().unwrap().temperatures,
            [1.0, 2.0]
        );
        assert_eq!(circuit.to_proto_bytes(), packed);
    }
}

fn library_circuit() -> Circuit {
    let library = |path: &str, models: &[&str]| ExternalLibrary {
        path: path.into(),
        model_type: "bsim4".into(),
        external_model_names: models.iter().map(|model| model.to_string()).collect(),
    };
    let model = |name: &str, group: &str| ExternalModule {
        name: name.into(),
        library_name: "sky130".into(),
        group_name: group.into(),
        kind: ExternalModuleKind::Model,
        ..Default::default()
    };
    Circuit {
        name: "corners".into(),
        // out of key order: the index keeps the IR's order through the wire
        libraries: [
            (
                "sky130".to_string(),
                library("models.lib", &["nfet", "pfet"]),
            ),
            ("analog".to_string(), library("analog.lib", &[])),
        ]
        .into_iter()
        .collect(),
        ext_modules: vec![model("nfet", "tt"), model("nfet", "ff")],
        directives: vec![Directive {
            kind: DirectiveKind::Lib,
            name: String::new(),
            value: "models.lib tt".into(),
        }],
        ..Default::default()
    }
}

#[test]
fn model_library_and_corner_round_trip() {
    let circuit = library_circuit();
    let back = Circuit::from_proto_bytes(&circuit.to_proto_bytes()).unwrap();
    assert_eq!(back, circuit);
    assert!(back.libraries.keys().eq(["sky130", "analog"]));

    // libraries (45): {"zz": ExternalLibrary {path (2) "z.lib", external_model_names (4) ["n"]}};
    // ext_modules (20): {name (1) "nch", library_name (4) "zz", group_name (5) "tt"}
    let bytes = [
        &[0xa2, 0x01, 13][..],
        b"\x0a\x03nch\x22\x02zz\x2a\x02tt",
        &[0xea, 0x02, 16],
        b"\x0a\x02zz\x12\x0a\x12\x05z.lib\x22\x01n",
    ]
    .concat();
    let circuit = Circuit::from_proto_bytes(&bytes).unwrap();
    let (name, library) = circuit.libraries.first().unwrap();
    assert_eq!((name.as_str(), library.path.as_str()), ("zz", "z.lib"));
    assert_eq!(library.external_model_names, ["n"]);
    let model = &circuit.ext_modules[0];
    assert_eq!(
        (
            model.name.as_str(),
            model.library_name.as_str(),
            model.group_name.as_str()
        ),
        ("nch", "zz", "tt")
    );
    assert_eq!(circuit.to_proto_bytes(), bytes);
}

#[test]
fn hostile_protobuf_is_an_error_not_a_panic() {
    let mut circuit = library_circuit();
    circuit.simulation = Some(Simulation {
        temperatures: vec![27.0],
        ..Default::default()
    });
    let bytes = circuit.to_proto_bytes();
    for end in 0..bytes.len() {
        let _ = Circuit::from_proto_bytes(&bytes[..end]);
    }
    // a 10-byte varint key that overflows the field number; a length past the end
    assert!(Circuit::from_proto_bytes(&[0xff; 10]).is_err());
    assert!(Circuit::from_proto_bytes(&[0x0a, 0x7f, b'x']).is_err());

    // each unary level is two messages (Expression, UnaryOp): 60 levels pass 100
    let mut expression = raw("x");
    for _ in 0..60 {
        expression = Expression {
            unary: Some(UnaryOp {
                op: Operator::Neg,
                operand: boxed(expression),
            }),
            ..Default::default()
        };
    }
    let deep = circuit_with_module(Module {
        name: "top".into(),
        parameters: vec![parameter_with(
            "p",
            ParameterValue {
                expression: Some(expression),
                ..Default::default()
            },
        )],
        ..Default::default()
    });
    let error = Circuit::from_proto_bytes(&deep.to_proto_bytes()).unwrap_err();
    assert!(error.contains("recursion limit"), "{error}");
}

#[test]
fn oneof_validation_works_in_rust() {
    let parameter: ParameterValue = serde_json::from_value(json!({"model_ref": {
        "model_interface_name": "model", "arguments": {"x": {"prefixed_value": {"double_value": 2.0}}}
    }}))
    .unwrap();
    assert!(parameter.validate().is_ok());
    let circuit = circuit_with_module(Module {
        name: "top".to_string(),
        parameters: vec![parameter_with("p", parameter.clone())],
        ..Default::default()
    });
    assert_cross_format_round_trip(&circuit);
    assert_eq!(
        serde_json::from_value::<ParameterValue>(serde_json::to_value(&parameter).unwrap())
            .unwrap(),
        parameter
    );
    let two_values: ParameterValue =
        serde_json::from_value(json!({"prefixed_value": {}, "model_ref": {}})).unwrap();
    assert!(two_values.validate().is_err());
    let number_and_expression: ParameterValue =
        serde_json::from_value(json!({"prefixed_value": {}, "expression": {"raw": "x"}})).unwrap();
    assert!(number_and_expression.validate().is_err());
    let two_nodes: Expression =
        serde_json::from_value(json!({"raw": "x", "reference": "y"})).unwrap();
    assert!(two_nodes.validate().is_err());
    let nested = Expression {
        unary: Some(UnaryOp {
            op: Operator::Neg,
            operand: boxed(two_nodes),
        }),
        ..Default::default()
    };
    assert!(nested.validate().is_err());
}

#[test]
fn macros_survive_json_yaml_and_proto() {
    let mut circuit = circuit_with_module(Module {
        name: "top".to_string(),
        macros: vec![spice_macro("local", ".param a=1")],
        ..Default::default()
    });
    circuit.macros = vec![spice_macro("rdiv", ".subckt rdiv a b\n.ends rdiv")];
    assert_cross_format_round_trip(&circuit);
    assert!(circuit.to_json().unwrap().contains("\"macros\""));
}

#[test]
fn stimuli_and_their_sources_survive_proto() {
    let simulation: Simulation = serde_json::from_value(json!({
        "stimuli": [{
            "name": "top/V1",
            "dc": {"prefixed_value": {"double_value": 0.9}},
            "waveform": {"kind": WaveformKind::Pulse as i32,
                "args": {"val0": {"prefixed_value": {"double_value": 0.0}},
                         "val1": {"expression": {"reference": "vdd"}}}}
        }]
    }))
    .unwrap();
    let source = ModuleReference {
        name: "V1".into(),
        module_name: "vsource".into(),
        properties: [("stimulus".to_string(), "top/V1".to_string())]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let mut circuit = circuit_with_module(Module {
        name: "top".into(),
        module_references: vec![source],
        ..Default::default()
    });
    circuit.simulation = Some(simulation.clone());
    let back = Circuit::from_proto_bytes(&circuit.to_proto_bytes()).unwrap();
    assert_eq!(back.simulation, Some(simulation));
    assert_eq!(
        back.modules[0].module_references[0].properties["stimulus"],
        "top/V1"
    );
    assert!(!serde_json::to_string(&Simulation::default())
        .unwrap()
        .contains("stimuli"));
}
