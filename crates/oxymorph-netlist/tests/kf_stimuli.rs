//! Source stimuli, PWL sources and open pins, ported from kfnetlist 75c630f, b1153f6,
//! b4dc1ec, 30a1621, 6db1e11, 650763b, 8f0a4b4, 7970b4f, b26cd6b and d5b82b2.
//! from: kfnetlist tests/morphnet/test_stimulus.py, test_spice_family_sources.py,
//! test_pwl_sources.py, test_review_fixes.py, test_net_utils.py (TestNetMapToConnections),
//! test_spectre_parser.py (TestSimulationFixtures), test_vams_parser.py
//! (TestSimulationFixtures); the Verilog-A and SystemVerilog parts of test_pwl_sources.py
//! and test_review_fixes.py (Task 6: 650763b, b26cd6b, d5b82b2 grammar and transformers)

use indexmap::IndexMap;
use oxymorph_netlist::expression::{parameter_value, to_float, Dialect};
use oxymorph_netlist::hdl::HdlOptions;
use oxymorph_netlist::helpers::{net_map_to_connections, open_node, NetMap};
use oxymorph_netlist::stimulus::{
    self, add_stimuli, add_stimulus, attach, from_primitive, from_spectre, primitive,
    primitive_file, spectre_params, spice_text, stimulus_of, verilog_value,
};
use oxymorph_netlist::{
    parse_hspice, parse_spectre, parse_spectre_spice, parse_spice, parse_systemverilog, parse_vams,
    parse_xyce, write_hspice, write_spectre, write_spectre_spice, write_spice, write_systemverilog,
    write_vams, write_xyce, Error,
};
use oxymorph_schema::{
    Circuit, Connection, Expression, Module, ModuleReference, ParameterValue, PortReference,
    Stimulus, Waveform, WaveformKind,
};

type Parse = fn(&str) -> Result<Circuit, Error>;
type Write = fn(&Circuit) -> String;

const SPICE_PARSERS: [(&str, Parse); 4] = [
    ("spice", parse_spice),
    ("hspice", parse_hspice),
    ("xyce", parse_xyce),
    ("spectre_spice", parse_spectre_spice),
];

fn parse_sv(text: &str) -> Result<Circuit, Error> {
    parse_systemverilog(text, &HdlOptions::default())
}

fn pv(text: &str) -> ParameterValue {
    parameter_value(text)
}

fn wave(kind: WaveformKind, args: &[(&str, &str)]) -> Waveform {
    let mut values = IndexMap::new();
    for (name, text) in args {
        values.insert(name.to_string(), pv(text));
    }
    Waveform {
        kind,
        args: values,
        ..Default::default()
    }
}

fn stim(
    dc: Option<&str>,
    ac: Option<&str>,
    phase: Option<&str>,
    waveform: Option<Waveform>,
) -> Stimulus {
    Stimulus {
        dc: dc.map(pv),
        ac_magnitude: ac.map(pv),
        ac_phase: phase.map(pv),
        waveform,
        ..Default::default()
    }
}

fn pwl_points(points: &[&str]) -> Waveform {
    Waveform {
        kind: WaveformKind::Pwl,
        points: points.iter().map(|point| pv(point)).collect(),
        ..Default::default()
    }
}

fn pwl_file(file: &str) -> Waveform {
    Waveform {
        kind: WaveformKind::Pwl,
        file: Some(file.to_string()),
        ..Default::default()
    }
}

fn pulse() -> Waveform {
    wave(
        WaveformKind::Pulse,
        &[
            ("val0", "0"),
            ("val1", "1.8"),
            ("delay", "1n"),
            ("rise", "10p"),
            ("fall", "10p"),
            ("width", "1n"),
            ("period", "2n"),
        ],
    )
}

fn case(name: &str) -> Stimulus {
    let stimulus_cases = cases();
    for (case_name, stimulus) in stimulus_cases {
        if case_name == name {
            return stimulus;
        }
    }
    panic!("no case {name}")
}

fn cases() -> Vec<(&'static str, Stimulus)> {
    vec![
        ("dc", stim(Some("1.8"), None, None, None)),
        ("ac", stim(None, Some("1"), None, None)),
        (
            "dc_ac_phase",
            stim(Some("0.9"), Some("1"), Some("90"), None),
        ),
        ("pulse", stim(Some("0.9"), None, None, Some(pulse()))),
        (
            "sin",
            stim(
                Some("1m"),
                Some("1"),
                None,
                Some(wave(
                    WaveformKind::Sin,
                    &[
                        ("sinedc", "1m"),
                        ("ampl", "0.5m"),
                        ("freq", "1meg"),
                        ("delay", "0"),
                        ("theta", "0"),
                    ],
                )),
            ),
        ),
        (
            "exp",
            stim(
                None,
                None,
                None,
                Some(wave(
                    WaveformKind::Exp,
                    &[
                        ("val0", "0"),
                        ("val1", "1"),
                        ("delay", "1n"),
                        ("tau1", "2n"),
                        ("td2", "5n"),
                        ("tau2", "3n"),
                    ],
                )),
            ),
        ),
        (
            "sffm",
            stim(
                None,
                None,
                None,
                Some(wave(
                    WaveformKind::Sffm,
                    &[
                        ("sinedc", "0"),
                        ("ampl", "1"),
                        ("freq", "1k"),
                        ("modindex", "2"),
                        ("modfreq", "100"),
                    ],
                )),
            ),
        ),
        (
            "pwl",
            stim(None, None, None, Some(pwl_points(&["0", "0", "1n", "1.8"]))),
        ),
        (
            "pwl_file",
            stim(None, None, None, Some(pwl_file("data/w.txt"))),
        ),
        (
            "expression",
            stim(
                Some("vdd / 2"),
                None,
                None,
                Some(wave(
                    WaveformKind::Pulse,
                    &[
                        ("val0", "0"),
                        ("val1", "vdd"),
                        ("delay", "0"),
                        ("rise", "0"),
                        ("fall", "0"),
                        ("width", "w"),
                        ("period", "2 * w"),
                    ],
                )),
            ),
        ),
    ]
}

/// A Spectre param written as text, read the way kfnetlist reads a `str` param.
fn spectre_values(params: &[(String, String)]) -> IndexMap<String, ParameterValue> {
    let mut values = IndexMap::new();
    for (name, text) in params {
        values.insert(name.clone(), pv(text));
    }
    values
}

/// A primitive param written as text, read as Verilog-A (`1M` is mega).
fn verilog_values(params: &[(String, String)]) -> IndexMap<String, ParameterValue> {
    let mut values = IndexMap::new();
    for (name, text) in params {
        values.insert(name.clone(), verilog_value(text));
    }
    values
}

fn string_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, text)| (name.to_string(), text.to_string()))
        .collect()
}

fn element<'a>(circuit: &'a Circuit, name: &str) -> &'a ModuleReference {
    for module in &circuit.modules {
        for reference in &module.module_references {
            if reference.name == name {
                return reference;
            }
        }
    }
    panic!("no element {name}")
}

fn stimulus_named<'a>(circuit: &'a Circuit, name: &str) -> &'a Stimulus {
    stimulus_of(circuit, element(circuit, name)).expect("a stimulus")
}

fn waveform_of<'a>(circuit: &'a Circuit, name: &str) -> &'a Waveform {
    stimulus_named(circuit, name)
        .waveform
        .as_ref()
        .expect("a waveform")
}

// ── test_stimulus.py ───────────────────────────────────────────────────

#[test]
fn spice_text_reads_back() {
    for (name, stimulus) in cases() {
        let text = spice_text(&stimulus, Dialect::Ngspice);
        assert_eq!(stimulus::parse_spice(&text).unwrap(), stimulus, "{name}");
    }
}

#[test]
fn spectre_params_read_back() {
    for (name, stimulus) in cases() {
        let params = spectre_params(&stimulus, Dialect::Spectre);
        let back = from_spectre(&spectre_values(&params), Dialect::Spectre);
        assert_eq!(back, Some(stimulus), "{name}");
    }
}

#[test]
fn verilog_a_primitive_reads_back() {
    for (name, stimulus) in cases() {
        let (master, params) = primitive(&stimulus, false, Dialect::Vams);
        let file = primitive_file(&stimulus);
        let back = from_primitive(&master, &verilog_values(&params), file.as_deref());
        assert_eq!(back, Some(stimulus), "{name}");
    }
}

#[test]
fn spice_text_is_what_nyancad_writes() {
    let dc_ac = stim(Some("0.9"), Some("1"), None, None);
    assert_eq!(spice_text(&dc_ac, Dialect::Ngspice), "DC 0.9 AC 1");
    assert_eq!(
        spice_text(&case("pulse"), Dialect::Ngspice),
        "DC 0.9 PULSE(0 1.8 1n 10p 10p 1n 2n)"
    );
    assert_eq!(
        spice_text(&case("expression"), Dialect::Ngspice),
        "DC {vdd / 2} PULSE(0 {vdd} 0 0 0 {w} {2 * w})"
    );
}

#[test]
fn spice_text_without_a_dc_is_ac_only() {
    assert_eq!(spice_text(&case("ac"), Dialect::Ngspice), "AC 1");
    // a bare value is the DC value
    assert_eq!(stimulus::parse_spice("1.8").unwrap(), case("dc"));
}

#[test]
fn vacask_drops_what_inspice_leaves_out_and_counts_td2_from_the_delay() {
    let pulse: IndexMap<String, String> = spectre_params(&case("pulse"), Dialect::Vacask)
        .into_iter()
        .collect();
    assert_eq!(pulse["type"], "\"pulse\"");
    let sin: IndexMap<String, String> = spectre_params(&case("sin"), Dialect::Vacask)
        .into_iter()
        .collect();
    assert!(!sin.contains_key("delay") && !sin.contains_key("theta"));
    let exp: IndexMap<String, String> = spectre_params(&case("exp"), Dialect::Vacask)
        .into_iter()
        .collect();
    assert_eq!(exp["td2"], "4e-09"); // 5n - 1n
    let exp_spectre: IndexMap<String, String> = spectre_params(&case("exp"), Dialect::Spectre)
        .into_iter()
        .collect();
    assert_eq!(exp_spectre["td2"], "5n");
    let pulse_spectre: IndexMap<String, String> = spectre_params(&case("pulse"), Dialect::Spectre)
        .into_iter()
        .collect();
    assert_eq!(pulse_spectre["type"], "pulse");
}

#[test]
fn verilog_a_uses_the_lrm_primitive_names() {
    let (master, params) = primitive(&case("pulse"), false, Dialect::Vams);
    assert_eq!(master, "vpulse");
    assert_eq!(params[..2], string_pairs(&[("dc", "0.9"), ("val0", "0")]));
    assert!(params.contains(&("td".to_string(), "1n".to_string())));
    assert_eq!(
        primitive(&case("dc"), true, Dialect::Vams),
        ("isine".to_string(), string_pairs(&[("dc", "1.8")]))
    );
    let (master, params) = primitive(&case("sffm"), false, Dialect::Vams);
    assert_eq!(master, "vsine");
    assert!(params.contains(&("fmmodindex".to_string(), "2".to_string())));
    let (_, pwl_params) = primitive(&case("pwl"), false, Dialect::Vams);
    let pwl_params: IndexMap<String, String> = pwl_params.into_iter().collect();
    assert_eq!(pwl_params["wave"], "'{0, 0, 1n, 1.8}");
    assert_eq!(from_primitive("resistor", &IndexMap::new(), None), None);
}

fn named_stimulus(name: &str, dc: &str) -> Stimulus {
    Stimulus {
        name: name.to_string(),
        ..stim(Some(dc), None, None, None)
    }
}

#[test]
fn add_stimuli_is_add_stimulus_in_turn() {
    let existing = vec![named_stimulus("a", "1"), named_stimulus("b", "2")];
    let added = vec![
        named_stimulus("c", "3"),
        named_stimulus("a", "4"),
        named_stimulus("c", "5"),
    ];
    let mut one_by_one = Circuit {
        simulation: Some(oxymorph_schema::Simulation {
            stimuli: existing,
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut in_one_pass = one_by_one.clone();
    for stimulus in added.clone() {
        add_stimulus(&mut one_by_one, stimulus);
    }
    add_stimuli(&mut in_one_pass, added);
    assert_eq!(in_one_pass, one_by_one);
    let names: Vec<&str> = in_one_pass
        .simulation
        .as_ref()
        .unwrap()
        .stimuli
        .iter()
        .map(|stimulus| stimulus.name.as_str())
        .collect();
    assert_eq!(names, ["b", "a", "c"]);
    let mut empty = Circuit::default();
    add_stimuli(&mut empty, Vec::new());
    assert_eq!(empty.simulation, None);
}

/// Writers look each source's stimulus up by name instead of scanning every stimulus.
#[test]
fn writers_are_linear_in_sources() {
    const SOURCE_COUNT: usize = 20_000;
    let mut module = Module {
        name: "top".to_string(),
        ..Default::default()
    };
    let mut stimuli = Vec::new();
    for index in 0..SOURCE_COUNT {
        let mut reference = ModuleReference {
            name: format!("V{index}"),
            module_name: "vsource".to_string(),
            ..Default::default()
        };
        stimuli.push(stimulus::name_stimulus(
            "top",
            &mut reference,
            stim(Some(&index.to_string()), None, None, None),
        ));
        module.module_references.push(reference);
    }
    let mut circuit = Circuit {
        top_module: "top".to_string(),
        modules: vec![module],
        ..Default::default()
    };
    add_stimuli(&mut circuit, stimuli);
    let started = std::time::Instant::now();
    let writes = [
        write_spice(&circuit),
        write_spectre(&circuit),
        write_vams(&circuit),
        write_systemverilog(&circuit),
    ];
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let last = SOURCE_COUNT - 1;
    assert!(writes[0].contains(&format!("\nV{last} nc_V{last}_p nc_V{last}_n DC {last}\n")));
}

#[test]
fn attach_names_the_stimulus_after_its_source() {
    let mut circuit = Circuit {
        top_module: "top".to_string(),
        modules: vec![Module {
            name: "top".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut reference = ModuleReference {
        name: "V1".to_string(),
        module_name: "vsource".to_string(),
        ..Default::default()
    };
    attach(
        &mut circuit,
        "top",
        &mut reference,
        stim(Some("1"), None, None, None),
    );
    assert_eq!(reference.properties["stimulus"], "top/V1");
    let stimuli = &circuit.simulation.as_ref().unwrap().stimuli;
    assert_eq!(stimulus_of(&circuit, &reference), Some(&stimuli[0]));
    assert_eq!(stimuli[0].name, "top/V1");
    let resistor = ModuleReference {
        name: "R1".to_string(),
        ..Default::default()
    };
    assert_eq!(stimulus_of(&circuit, &resistor), None);
}

#[test]
fn a_pwl_file_in_every_dialect() {
    let stimulus = case("pwl_file");
    assert_eq!(
        spice_text(&stimulus, Dialect::Ngspice),
        "PWL FILE \"data/w.txt\""
    );
    assert_eq!(
        spectre_params(&stimulus, Dialect::Spectre),
        string_pairs(&[("type", "pwl"), ("file", "\"data/w.txt\"")])
    );
    assert_eq!(
        primitive(&stimulus, true, Dialect::Vams),
        ("ipwl".to_string(), vec![])
    );
    assert_eq!(primitive_file(&stimulus).as_deref(), Some("data/w.txt"));
    // written on Windows
    assert_eq!(
        stimulus::parse_spice("PWL FILE \"data\\w.txt\"").unwrap(),
        stimulus
    );
    assert_eq!(
        stimulus::parse_spice("PWL FILE=data/w.txt").unwrap(),
        stimulus
    );
}

#[test]
fn a_space_before_the_parenthesis_is_the_same_source() {
    assert_eq!(
        stimulus::parse_spice("DC 0.9 PULSE (0 1.8 1n 10p 10p 1n 2n)").unwrap(),
        case("pulse")
    );
    assert_eq!(
        stimulus::parse_spice("PWL ( 0 0 1n 1.8 )").unwrap(),
        case("pwl")
    );
}

#[test]
fn spectre_wave_given_as_a_value_keeps_its_points() {
    let raw = ParameterValue {
        expression: Some(Expression {
            raw: Some("[0 0 1n {vdd}]".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut params = IndexMap::new();
    params.insert("type".to_string(), pv("pwl"));
    params.insert("wave".to_string(), raw);
    let stimulus = from_spectre(&params, Dialect::Spectre).unwrap();
    let points = &stimulus.waveform.unwrap().points;
    assert_eq!(points.last(), Some(&pv("vdd")));
}

/// Task 2 review: `Waveform.args` read back from protobuf has no order, so writers
/// print positional args by position.
#[test]
fn source_args_are_written_by_position_whatever_their_order() {
    let mut reversed = pulse();
    reversed.args.reverse();
    let stimulus = stim(Some("0.9"), None, None, Some(reversed));
    assert_eq!(
        spice_text(&stimulus, Dialect::Ngspice),
        "DC 0.9 PULSE(0 1.8 1n 10p 10p 1n 2n)"
    );
    let names: Vec<String> = spectre_params(&stimulus, Dialect::Spectre)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["dc", "type", "val0", "val1", "delay", "rise", "fall", "width", "period"]
    );
    let circuit = parse_spice("* t\nV1 a 0 DC 0.9 PULSE(0 1.8 1n 10p 10p 1n 2n)\n.end\n").unwrap();
    let back = Circuit::from_proto_bytes(&circuit.to_proto_bytes()).unwrap();
    assert_eq!(write_spice(&back), write_spice(&circuit));
    assert_eq!(write_spectre(&back), write_spectre(&circuit));
}

// ── test_spice_family_sources.py ───────────────────────────────────────

#[test]
fn a_source_line_is_a_stimulus() {
    for (dialect, parse) in SPICE_PARSERS {
        let circuit = parse(
            "* t\nV1 a 0 DC 0.9 AC 1 PULSE(0 1.8 1n 10p 10p 1n 2n)\n\
             I1 a 0 SIN(1m 0.5m 1meg)\nR1 a 0 1k\n.end\n",
        )
        .unwrap();
        let v1 = element(&circuit, "V1");
        assert!(!v1.properties.contains_key("model_name"), "{dialect}"); // `AC` is no model
        let source = stimulus_of(&circuit, v1).unwrap();
        assert_eq!(source.name, "__top__/V1", "{dialect}");
        assert_eq!(
            (source.dc.clone(), source.ac_magnitude.clone()),
            (Some(pv("0.9")), Some(pv("1"))),
            "{dialect}"
        );
        let waveform = source.waveform.as_ref().unwrap();
        assert_eq!(waveform.kind, WaveformKind::Pulse, "{dialect}");
        assert_eq!(waveform.args["period"], pv("2n"), "{dialect}");
        assert_eq!(
            waveform_of(&circuit, "I1").args["freq"],
            pv("1meg"),
            "{dialect}"
        );
    }
}

#[test]
fn a_bare_value_is_a_reference_unless_a_model_has_its_name() {
    for (dialect, parse) in SPICE_PARSERS {
        let circuit =
            parse("* t\n.model rmod r\nR1 a b rload\nR2 a b rmod\nC1 a 0 1p\n.end\n").unwrap();
        let r1 = element(&circuit, "R1");
        let r2 = element(&circuit, "R2");
        assert_eq!(r1.parameters.len(), 1, "{dialect}");
        let value = r1.parameters[0].default_value.as_ref().unwrap();
        assert_eq!(
            value.expression,
            Some(Expression::reference("rload")),
            "{dialect}"
        );
        assert!(!r1.properties.contains_key("model_name"), "{dialect}");
        assert_eq!(r2.properties["model_name"], "rmod", "{dialect}");
        assert!(r2.parameters.is_empty(), "{dialect}");
    }
}

#[test]
fn top_level_elements_are_the_top_even_with_subcircuits() {
    for (dialect, parse) in SPICE_PARSERS {
        let circuit = parse("* t\n.subckt foo a\nR1 a 0 1k\n.ends foo\nX1 n foo\n.end\n").unwrap();
        assert_eq!(circuit.top_module, "__top__", "{dialect}");
        let circuit = parse("* t\n.subckt foo a\nR1 a 0 1k\n.ends foo\n.end\n").unwrap();
        assert_eq!(circuit.top_module, "foo", "{dialect}");
    }
}

#[test]
fn a_wrapped_value_is_an_expression() {
    let wrapped: [(&str, Parse, &str); 3] = [
        ("spice", parse_spice, "{w*2}"),
        ("xyce", parse_xyce, "{w*2}"),
        ("hspice", parse_hspice, "'w*2'"),
    ];
    for (dialect, parse, text) in wrapped {
        let circuit = parse(&format!("* t\nR1 a b {text}\n.end\n")).unwrap();
        let r1 = element(&circuit, "R1");
        assert_eq!(r1.parameters.len(), 1, "{dialect}");
        let value = r1.parameters[0].default_value.as_ref().unwrap();
        assert!(
            value.expression.as_ref().unwrap().binary.is_some(),
            "{dialect}"
        );
    }
}

// ── test_net_utils.py (TestNetMapToConnections) ────────────────────────

#[test]
fn single_port_net_is_a_source_only_connection() {
    let mut net_map = NetMap::new();
    net_map.add("floating", "R1", "p");
    let connections = net_map_to_connections(&net_map);
    assert_eq!(connections.len(), 1);
    let connection = &connections[0];
    assert_eq!(connection.name, "floating");
    assert_eq!(connection.source.as_ref().unwrap().instance_name, "R1");
    assert_eq!(connection.target, None);
}

#[test]
fn an_open_pin_node_is_no_connection() {
    assert_eq!(open_node("A", "S"), "nc_A_S");
    let mut net_map = NetMap::new();
    net_map.add("nc_A_S", "A", "S");
    assert!(net_map_to_connections(&net_map).is_empty());
}

#[test]
fn an_open_pin_node_read_with_positional_pins_is_no_connection() {
    // `XA coupling nc_A_S cell` with `cell` undefined
    let mut net_map = NetMap::new();
    net_map.add("nc_A_S", "A", "p1");
    assert!(net_map_to_connections(&net_map).is_empty());
}

#[test]
fn an_open_pin_is_written_on_its_own_node() {
    // 30a1621 / 5559e43: the node is named after the written instance name
    let mut circuit =
        parse_spice("* t\n.subckt cell a b\nR1 a b 1k\n.ends cell\nX1 n cell\n.end\n").unwrap();
    for module in &mut circuit.modules {
        module
            .connections
            .retain(|connection| connection.name != "n");
    }
    assert!(write_spice(&circuit).contains("X1 nc_X1_a nc_X1_b cell"));
    assert!(write_spectre(&circuit).contains("X1 (nc_X1_a nc_X1_b) cell"));
    let back = parse_spice(&write_spice(&circuit)).unwrap();
    let top = &back.modules[0];
    assert_eq!(top.name, "__top__");
    assert!(top.connections.is_empty());
}

// ── test_spectre_parser.py (TestSimulationFixtures) ────────────────────

#[test]
fn spectre_a_source_is_a_stimulus() {
    let circuit = parse_spectre(
        "V1 (a 0) vsource dc=0.9 mag=1 type=pulse val0=0 val1=1.8 period=2n\n\
         I1 (a 0) isource dc=1m type=\"sine\" sinedc=1m ampl=0.5m freq=1M\n\
         R1 (a 0) resistor r=1k\n",
    )
    .unwrap();
    let references = &circuit.modules[0].module_references;
    let (v1, i1) = (&references[0], &references[1]);
    assert!(v1.parameters.is_empty() && i1.parameters.is_empty());
    let source = stimulus_of(&circuit, v1).unwrap();
    assert_eq!(source.name, "__top__/V1");
    let waveform = source.waveform.as_ref().unwrap();
    assert_eq!(waveform.kind, WaveformKind::Pulse);
    assert_eq!(waveform.args["val1"], pv("1.8"));
    assert_eq!(source.ac_magnitude, Some(pv("1")));
    assert_eq!(waveform_of(&circuit, "I1").kind, WaveformKind::Sin);
}

#[test]
fn spectre_simulator_lang_sections_are_one_netlist() {
    let circuit = parse_spectre(
        "simulator lang=spice\n.subckt rdiv a b\nR1 a b 1k\n.ends rdiv\n\
         simulator lang=spectre\nX1 (in mid) rdiv\nX2 (mid out) rdiv\n",
    )
    .unwrap();
    let mut names: Vec<&str> = circuit
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["__top__", "rdiv"]);
    assert_eq!(circuit.top_module, "__top__");
}

#[test]
fn spectre_top_level_instances_are_the_top_even_with_subcircuits() {
    let circuit =
        parse_spectre("subckt foo (a)\nR1 (a 0) resistor r=1k\nends foo\nX1 (n) foo\n").unwrap();
    assert_eq!(circuit.top_module, "__top__");
}

// ── test_vams_parser.py (TestSimulationFixtures) ───────────────────────

#[test]
fn vams_a_source_primitive_is_a_stimulus() {
    let circuit = parse_vams(
        "module t (a);\n  inout a;\n\
         \x20 vpulse #(.dc(0.9), .val0(0), .val1(1.8), .td(1n)) V1 (.p(a), .n(gnd));\n\
         \x20 isine #(.dc(1m)) I1 (.p(a), .n(gnd));\nendmodule\n",
    )
    .unwrap();
    let references = &circuit.modules[0].module_references;
    let (v1, i1) = (&references[0], &references[1]);
    assert_eq!(
        (v1.module_name.as_str(), i1.module_name.as_str()),
        ("vsource", "isource")
    );
    assert!(v1.parameters.is_empty() && i1.parameters.is_empty());
    let waveform = waveform_of(&circuit, "V1");
    assert_eq!(waveform.kind, WaveformKind::Pulse);
    assert_eq!(waveform.args["delay"], pv("1n"));
    assert_eq!(stimulus_of(&circuit, i1).unwrap().dc, Some(pv("1m")));
}

// ── test_review_fixes.py ───────────────────────────────────────────────

#[test]
fn spectre_keeps_its_own_source_params() {
    let lines = [
        (
            "V1 (a 0) vsource type=sine sinedc=0 ampl=1 freq=1k damp=10\n",
            "10",
        ),
        (
            "V1 (a 0) vsource type=exp val0=0 val1=1 td1=1n tau1=2n\n",
            "1n",
        ),
    ];
    for (line, kept) in lines {
        let circuit = parse_spectre(line).unwrap();
        let waveform = waveform_of(&circuit, "V1");
        let kept_value = pv(kept);
        assert!(
            waveform.args.values().any(|value| *value == kept_value),
            "{line}"
        );
    }
}

#[test]
fn spectre_keeps_an_unknown_source_type() {
    let circuit = parse_spectre("V1 (a 0) vsource dc=1 type=prbs seed=3\n").unwrap();
    let text = write_spectre(&circuit);
    assert!(
        text.contains("type=prbs") && text.contains("seed=3"),
        "{text}"
    );
}

#[test]
fn a_value_then_a_name_is_a_value_and_a_model() {
    let circuit = parse_spice("* t\nR1 a b 1k rpoly\nR2 a b rpoly w=1u l=2u\n.end\n").unwrap();
    let references = &circuit.modules[0].module_references;
    let (r1, r2) = (&references[0], &references[1]);
    assert_eq!(r1.properties["model_name"], "rpoly");
    let names: Vec<&str> = r1
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    assert_eq!(names, ["value"]);
    assert_eq!(r2.properties["model_name"], "rpoly"); // geometry params: a model
}

#[test]
fn an_unknown_spice_source_function_is_an_error() {
    let result = parse_spice("* t\nV1 a 0 DC 1 AM(1 0 1k 10k 0)\n.end\n");
    match result {
        Err(Error::Validation(message)) => assert!(message.contains("AM"), "{message}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn verilog_a_mega_is_m_both_ways() {
    let circuit = parse_spice("* t\nI1 a 0 SIN(0 1m 1meg)\nR1 a 0 1k\n.end\n").unwrap();
    let text = write_vams(&circuit);
    assert!(
        !text.contains("meg") && text.contains(".freq(1M)"),
        "{text}"
    );
    let back = parse_vams(&text).unwrap();
    let i1 = element(&back, "I1");
    let freq = &stimulus_of(&back, i1)
        .unwrap()
        .waveform
        .as_ref()
        .unwrap()
        .args["freq"];
    assert_eq!(to_float(freq.prefixed_value.as_ref().unwrap()), 1e6);
    let resistor =
        parse_vams("module t;\n  resistor #(.r(1M)) R1 (.p(a), .n(b));\nendmodule\n").unwrap();
    let value = resistor.modules[0].module_references[0].parameters[0]
        .default_value
        .as_ref()
        .unwrap();
    assert_eq!(to_float(value.prefixed_value.as_ref().unwrap()), 1e6);
    assert!(write_spice(&back).contains("SIN(0 1m 1meg)")); // SPICE keeps its own spelling
}

// ── test_pwl_sources.py ────────────────────────────────────────────────

fn sources() -> Circuit {
    let mut references = vec![
        ModuleReference {
            name: "V1".to_string(),
            module_name: "vsource".to_string(),
            ..Default::default()
        },
        ModuleReference {
            name: "I2".to_string(),
            module_name: "isource".to_string(),
            ..Default::default()
        },
    ];
    let mut connections = Vec::new();
    for (net, pin) in [("a", "p"), ("0", "n")] {
        for reference in &references {
            connections.push(Connection {
                name: net.to_string(),
                source: Some(PortReference {
                    instance_name: reference.name.clone(),
                    port_name: pin.to_string(),
                }),
                ..Default::default()
            });
        }
    }
    let mut circuit = Circuit {
        top_module: "__top__".to_string(),
        ..Default::default()
    };
    let points = stim(None, None, None, Some(pwl_points(&["0", "0", "1n", "1.8"])));
    let file = stim(None, None, None, Some(pwl_file("data/w.txt")));
    attach(&mut circuit, "__top__", &mut references[0], points);
    attach(&mut circuit, "__top__", &mut references[1], file);
    circuit.modules.push(Module {
        name: "__top__".to_string(),
        module_references: references,
        connections,
        ..Default::default()
    });
    circuit
}

fn numbers(waveform: &Waveform) -> (WaveformKind, Vec<f64>, Option<String>) {
    let mut points = Vec::new();
    for point in &waveform.points {
        if let Some(prefixed_value) = &point.prefixed_value {
            points.push(to_float(prefixed_value));
        }
    }
    (waveform.kind, points, waveform.file.clone())
}

#[test]
fn pwl_points_and_files_round_trip() {
    let dialects: [(&str, Write, Parse); 7] = [
        ("ngspice", write_spice, parse_spice),
        ("hspice", write_hspice, parse_hspice),
        ("xyce", write_xyce, parse_xyce),
        ("spectre", write_spectre, parse_spectre),
        ("spectre_spice", write_spectre_spice, parse_spectre_spice),
        ("verilog-a", write_vams, parse_vams),
        ("systemverilog", write_systemverilog, parse_sv),
    ];
    for (dialect, write, parse) in dialects {
        let back = parse(&write(&sources())).unwrap();
        let mut by_name: IndexMap<String, &ModuleReference> = IndexMap::new();
        for module in &back.modules {
            for reference in &module.module_references {
                let name = reference.name.to_uppercase();
                by_name.insert(name.trim_start_matches('X').to_string(), reference);
            }
        }
        let v1 = stimulus_of(&back, by_name["V1"]).unwrap();
        let (kind, points, file) = numbers(v1.waveform.as_ref().unwrap());
        assert_eq!((kind, file), (WaveformKind::Pwl, None), "{dialect}");
        let expected = [0.0, 0.0, 1e-9, 1.8];
        assert_eq!(points.len(), expected.len(), "{dialect}");
        for (point, expected_point) in points.iter().zip(expected) {
            assert!((point - expected_point).abs() <= 1e-12, "{dialect}");
        }
        let i2 = stimulus_of(&back, by_name["I2"]).unwrap();
        assert_eq!(
            numbers(i2.waveform.as_ref().unwrap()),
            (WaveformKind::Pwl, vec![], Some("data/w.txt".to_string())),
            "{dialect}"
        );
    }
}

#[test]
fn spice_reads_spaced_functions_and_pwl_files() {
    for (dialect, parse) in SPICE_PARSERS {
        let circuit = parse(
            "* t\nV1 a 0 PULSE (0 1.8 1n 10p 10p 1n 2n)\nV2 b 0 PWL FILE \"data\\w.txt\"\n\
             V3 c 0 PWL (0 0 1n 1.8)\n.end\n",
        )
        .unwrap();
        assert_eq!(
            waveform_of(&circuit, "V1").args["period"],
            pv("2n"),
            "{dialect}"
        );
        assert_eq!(
            waveform_of(&circuit, "V2").file.as_deref(),
            Some("data/w.txt"),
            "{dialect}"
        );
        assert_eq!(waveform_of(&circuit, "V3").points.len(), 4, "{dialect}");
    }
}

#[test]
fn spectre_reads_wave_lists_and_files() {
    let circuit = parse_spectre(
        "V1 (a 0) vsource type=pwl wave=[0 0 1n vdd]\n\
         V2 (b 0) vsource type=pwl file=\"data/w.txt\"\n",
    )
    .unwrap();
    assert_eq!(waveform_of(&circuit, "V1").points.last(), Some(&pv("vdd")));
    assert_eq!(
        waveform_of(&circuit, "V2").file.as_deref(),
        Some("data/w.txt")
    );
    assert!(element(&circuit, "V1").parameters.is_empty());
    assert!(element(&circuit, "V2").parameters.is_empty());
}

#[test]
fn spice_reads_an_ltspice_pwl_file_assignment() {
    for (dialect, parse) in SPICE_PARSERS {
        let circuit =
            parse("* t\nV1 a 0 PWL file=w.txt\nV2 b 0 PWL FILE=\"data/w.txt\"\n.end\n").unwrap();
        assert_eq!(
            waveform_of(&circuit, "V1").file.as_deref(),
            Some("w.txt"),
            "{dialect}"
        );
        assert_eq!(
            waveform_of(&circuit, "V2").file.as_deref(),
            Some("data/w.txt"),
            "{dialect}"
        );
        assert!(element(&circuit, "V1").parameters.is_empty(), "{dialect}");
        assert!(element(&circuit, "V2").parameters.is_empty(), "{dialect}");
        assert_eq!(stimulus_named(&circuit, "V1").dc, None, "{dialect}");
    }
}

#[test]
fn verilog_a_reads_a_pwl_file_attribute() {
    let circuit = parse_vams(
        "module t;\n  (* pwl_file = \"data/w.txt\" *) vpwl #(.dc(0)) V1 (.p(a), .n(b));\nendmodule\n",
    )
    .unwrap();
    let v1 = &circuit.modules[0].module_references[0];
    let waveform = stimulus_of(&circuit, v1)
        .unwrap()
        .waveform
        .as_ref()
        .unwrap();
    assert_eq!(waveform.file.as_deref(), Some("data/w.txt"));
}

#[test]
fn systemverilog_pwl_file_attribute_forms() {
    let circuit = parse_sv(
        "module t (inout a, inout b);\n  (* pwl_file = \"w.txt\", keep *) vpwl#(.dc(0)) V1 (.p(a), .n(b));\n  foo #(.pwl_file(\"x.txt\")) U1 (.p(a), .n(b));\nendmodule\n",
    )
    .unwrap();
    let [v1, u1] = circuit.modules[0].module_references.as_slice() else {
        panic!("two instances");
    };
    let waveform = stimulus_of(&circuit, v1)
        .unwrap()
        .waveform
        .as_ref()
        .unwrap();
    assert_eq!(waveform.file.as_deref(), Some("w.txt"));
    let names: Vec<&str> = u1
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    assert_eq!(names, ["pwl_file"]); // not a source: kept
}

#[test]
fn a_spectre_wave_list_does_not_run_across_lines() {
    assert!(parse_spectre("V1 (a 0) vsource type=pwl wave=[0 0\n 1n 1]\n").is_err());
}

// ── Review Focus 3: Windows drive, UNC and `..` PWL files through the parsers ──

#[test]
fn pwl_files_are_stored_posix_through_every_parser() {
    let path_cases = include_str!("../../oxymorph-schema/tests/data/path_cases.json");
    let path_cases: Vec<serde_json::Value> = serde_json::from_str(path_cases).unwrap();
    for path_case in &path_cases {
        let file = path_case["file"].as_str().unwrap();
        let posix = path_case["posix"].as_str().unwrap();
        let decks = [
            format!("* t\nV1 a 0 PWL FILE \"{file}\"\n.end\n"),
            format!("* t\nV1 a 0 PWL file={file}\n.end\n"),
        ];
        for deck in &decks {
            for (dialect, parse) in SPICE_PARSERS {
                let circuit = parse(deck).unwrap();
                let stored = waveform_of(&circuit, "V1").file.clone();
                assert_eq!(stored.as_deref(), Some(posix), "{dialect}: {deck}");
                let back = parse(&write_spice(&circuit)).unwrap();
                assert_eq!(waveform_of(&back, "V1").file, stored, "{dialect}: {deck}");
            }
        }
        if file.contains("//") || posix.contains("//") {
            // Spectre reads `//` as a comment even inside quotes, as kfnetlist does
            // (gdsfactory/oxymorph#2): such a path, or a written UNC one, does not parse.
            continue;
        }
        let spectre = format!("V1 (a 0) vsource type=pwl file=\"{file}\"\n");
        let circuit = parse_spectre(&spectre).unwrap();
        let stored = waveform_of(&circuit, "V1").file.clone();
        assert_eq!(stored.as_deref(), Some(posix), "{spectre}");
        let back = parse_spectre(&write_spectre(&circuit)).unwrap();
        assert_eq!(waveform_of(&back, "V1").file, stored, "{spectre}");
    }
}
