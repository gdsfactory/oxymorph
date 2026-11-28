//! Parity against the committed oracle corpus (`tests/corpus/cases.json`).

use std::collections::HashMap;
use std::path::PathBuf;

use oxymorph_netlist::hdl::bus_base;
use oxymorph_netlist::helpers::{domain_of, net_domain, open_node};
use oxymorph_schema::{Circuit, Connection, Module, PortReference};

type Transform = fn(&mut Circuit);

/// Systematic divergences: an R4 id and the function that turns the oracle's IR into the
/// IR we now produce. Applied to the expected IR of every case (corpus `json` and the
/// decoded oracle protobuf). Case-specific divergences are overrides instead.
const TRANSFORMS: &[(&str, Transform)] = &[
    ("kf-signal-domains", apply_signal_domains),
    ("kf-one-pin-nets", apply_one_pin_port_nets),
    ("kf-top-elements", apply_top_elements),
    ("kf-hdl-nets", apply_net_disciplines),
];

/// kf-signal-domains: ports take `domain_of` their name; a connection takes its net's domain.
fn apply_signal_domains(circuit: &mut Circuit) {
    for ext_module in &mut circuit.ext_modules {
        for port in &mut ext_module.ports {
            port.domain = domain_of(&port.name);
        }
    }
    for module in &mut circuit.modules {
        for port in &mut module.ports {
            port.domain = domain_of(&port.name);
        }
        let mut pins_by_net: HashMap<String, Vec<PortReference>> = HashMap::new();
        for connection in &module.connections {
            let pins = pins_by_net.entry(connection.name.clone()).or_default();
            for port_reference in [&connection.source, &connection.target]
                .into_iter()
                .flatten()
            {
                pins.push(port_reference.clone());
            }
        }
        for connection in &mut module.connections {
            connection.domain = net_domain(&pins_by_net[&connection.name]);
        }
    }
}

/// kf-one-pin-nets: a port no element uses is a one-pin net, a source-only connection
/// at its place in net order (the ports come first, in port order). One-pin nets of
/// instance pins are not in the oracle IR: those cases are overrides.
fn apply_one_pin_port_nets(circuit: &mut Circuit) {
    for module in &mut circuit.modules {
        module.connections = connections_with_port_nets(module);
    }
}

fn connections_with_port_nets(module: &Module) -> Vec<Connection> {
    let mut remaining = module.connections.iter().peekable();
    let mut connections = Vec::new();
    for port in &module.ports {
        let used = module
            .connections
            .iter()
            .any(|connection| connection.name == port.name);
        if used {
            // a port's net leads the connections, in port order
            while let Some(connection) = remaining.next_if(|next| next.name == port.name) {
                connections.push(connection.clone());
            }
        } else if !port.name.starts_with(&open_node("", "")) {
            connections.push(Connection {
                name: port.name.clone(),
                source: Some(PortReference {
                    instance_name: String::new(),
                    port_name: port.name.clone(),
                }),
                domain: domain_of(&port.name),
                ..Default::default()
            });
        }
    }
    connections.extend(remaining.cloned());
    connections
}

/// kf-top-elements: the deck's own elements (`__top__`) are the top module.
fn apply_top_elements(circuit: &mut Circuit) {
    if circuit
        .modules
        .iter()
        .any(|module| module.name == "__top__")
    {
        circuit.top_module = "__top__".to_string();
    }
}

/// kf-hdl-nets: a Verilog net records its declared discipline as the connection's
/// `discipline` property. The oracle IR keeps the disciplines of ports only (the corpus
/// declares no internal net's).
fn apply_net_disciplines(circuit: &mut Circuit) {
    for module in &mut circuit.modules {
        let mut disciplines: HashMap<String, String> = HashMap::new();
        for port in &module.ports {
            if !port.discipline.is_empty() {
                disciplines.insert(port.name.clone(), port.discipline.clone());
            }
        }
        for connection in &mut module.connections {
            if let Some(discipline) = disciplines.get(bus_base(&connection.name)) {
                connection
                    .properties
                    .insert("discipline".to_string(), discipline.clone());
            }
        }
    }
}

fn apply_transforms(circuit: &mut Circuit) {
    for (_, transform) in TRANSFORMS {
        transform(circuit);
    }
}

fn transformed_json(json: &serde_json::Value) -> serde_json::Value {
    let mut circuit = Circuit::from_json(&json.to_string()).expect("expected IR is a circuit");
    apply_transforms(&mut circuit);
    serde_json::from_str(&circuit.to_json().unwrap()).unwrap()
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/cases.json")
}

fn parse(format: &str, text: &str) -> Result<Circuit, oxymorph_netlist::Error> {
    match format {
        "spice" => oxymorph_netlist::parse_spice(text),
        "hspice" => oxymorph_netlist::parse_hspice(text),
        "xyce" => oxymorph_netlist::parse_xyce(text),
        "spectre" => oxymorph_netlist::parse_spectre(text),
        "spectre_spice" => oxymorph_netlist::parse_spectre_spice(text),
        "vams" => oxymorph_netlist::parse_vams(text),
        other => panic!("unknown format {other}"),
    }
}

fn write(format: &str, c: &Circuit) -> String {
    match format {
        "spice" => oxymorph_netlist::write_spice(c),
        "hspice" => oxymorph_netlist::write_hspice(c),
        "xyce" => oxymorph_netlist::write_xyce(c),
        "spectre_spice" => oxymorph_netlist::write_spectre_spice(c),
        "spectre" => oxymorph_netlist::write_spectre(c),
        "vams" => oxymorph_netlist::write_vams(c),
        other => panic!("unknown format {other}"),
    }
}

fn overrides_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/overrides.json")
}

fn parity_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../specs/parity.md")
}

fn load_cases() -> Vec<serde_json::Value> {
    let data = std::fs::read_to_string(corpus_path()).expect("corpus is committed");
    serde_json::from_str(&data).expect("valid corpus")
}

fn load_overrides() -> serde_json::Value {
    let data = std::fs::read_to_string(overrides_path()).expect("overrides are committed");
    serde_json::from_str(&data).expect("valid overrides")
}

fn case_key(case: &serde_json::Value) -> String {
    format!(
        "{}/{}",
        case["format"].as_str().unwrap(),
        case["name"].as_str().unwrap()
    )
}

/// Problems with the overrides: an `r4` id missing from the R4 table of `parity_md`,
/// or a key that is not a corpus case.
fn r4_ids(parity_md: &str) -> Vec<&str> {
    let r4_section = parity_md
        .split("## R4.")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap_or("");
    r4_section
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .map(str::trim)
        .collect()
}

fn check_overrides(
    overrides: &serde_json::Value,
    parity_md: &str,
    case_keys: &[String],
) -> Vec<String> {
    let r4_ids = r4_ids(parity_md);

    let mut problems = Vec::new();
    for (key, entry) in overrides.as_object().expect("overrides are an object") {
        if !case_keys.contains(key) {
            problems.push(format!("{key}: not a corpus case"));
        }
        match entry["r4"].as_str() {
            Some(id) if r4_ids.contains(&id) => {}
            Some(id) => problems.push(format!("{key}: R4 id {id} is not in specs/parity.md")),
            None => problems.push(format!("{key}: no r4 id")),
        }
    }
    problems
}

struct Expected<'a> {
    json: &'a serde_json::Value,
    writes: Vec<(&'a str, &'a str)>,
}

fn expected_for<'a>(case: &'a serde_json::Value, over: &'a serde_json::Value) -> Expected<'a> {
    let writes = case["writes"].as_object().into_iter().flatten();
    Expected {
        json: over.get("json").unwrap_or(&case["json"]),
        writes: writes
            .map(|(format, text)| {
                (
                    format.as_str(),
                    over["writes"].get(format).unwrap_or(text).as_str().unwrap(),
                )
            })
            .collect(),
    }
}

#[test]
fn expected_values_apply_overrides() {
    let case =
        // keys in sorted order: serde_json keeps insertion order (`preserve_order`)
        serde_json::json!({"json": {"a": 1}, "writes": {"spectre": "old x", "spice": "old s"}});

    let none = serde_json::json!(null);
    let plain = expected_for(&case, &none);
    assert_eq!(plain.json, &serde_json::json!({"a": 1}));
    assert_eq!(plain.writes, [("spectre", "old x"), ("spice", "old s")]);

    let write_only = serde_json::json!({"r4": "kf-x", "writes": {"spice": "new s"}});
    let expected = expected_for(&case, &write_only);
    assert_eq!(expected.writes, [("spectre", "old x"), ("spice", "new s")]);

    let json_override = serde_json::json!({"r4": "kf-x", "json": {"a": 2}});
    let expected = expected_for(&case, &json_override);
    assert_eq!(expected.json, &serde_json::json!({"a": 2}));
    assert_eq!(expected.writes, [("spectre", "old x"), ("spice", "old s")]);
}

#[test]
fn override_check_reports_unknown_r4_ids_and_cases() {
    let parity_md = "## R4. Known\n\n| id | x |\n|---|---|\n| kf-known | y |\n\n## R5. Next\n\n| kf-other | z |\n";
    let case_keys = vec!["spice/a".to_string()];

    let valid = serde_json::json!({"spice/a": {"r4": "kf-known"}});
    assert!(check_overrides(&valid, parity_md, &case_keys).is_empty());

    let unknown_r4 = serde_json::json!({"spice/a": {"r4": "kf-other"}});
    assert_eq!(check_overrides(&unknown_r4, parity_md, &case_keys).len(), 1);

    let unknown_case = serde_json::json!({"spice/b": {"r4": "kf-known"}});
    assert_eq!(
        check_overrides(&unknown_case, parity_md, &case_keys).len(),
        1
    );

    let missing_r4 = serde_json::json!({"spice/a": {}});
    assert_eq!(check_overrides(&missing_r4, parity_md, &case_keys).len(), 1);
}

#[test]
fn overrides_reference_r4_rows_and_name_existing_cases() {
    let case_keys: Vec<String> = load_cases().iter().map(case_key).collect();
    let parity_md = std::fs::read_to_string(parity_md_path()).expect("parity spec exists");
    let problems = check_overrides(&load_overrides(), &parity_md, &case_keys);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn transforms_reference_r4_rows() {
    let parity_md = std::fs::read_to_string(parity_md_path()).expect("parity spec exists");
    let known_ids = r4_ids(&parity_md);
    for (id, _) in TRANSFORMS {
        assert!(
            known_ids.contains(id),
            "transform {id} is not in specs/parity.md R4"
        );
    }
}

#[test]
fn signal_domain_transform_follows_the_nets() {
    let mut circuit =
        oxymorph_netlist::parse_spice(".subckt top o1 e1\nX1 o1 e1 cell\n.ends top\n").unwrap();
    for connection in &mut circuit.modules[0].connections {
        connection.domain = oxymorph_schema::SignalDomain::Unspecified;
    }
    let mut transformed = circuit.clone();
    apply_signal_domains(&mut transformed);
    let domains: Vec<(&str, oxymorph_schema::SignalDomain)> = transformed.modules[0]
        .connections
        .iter()
        .map(|connection| (connection.name.as_str(), connection.domain))
        .collect();
    assert_eq!(
        domains,
        [
            ("o1", oxymorph_schema::SignalDomain::Waveguide),
            ("e1", oxymorph_schema::SignalDomain::Electrical)
        ]
    );
}

#[test]
fn one_pin_port_net_transform_keeps_net_order() {
    let circuit = oxymorph_netlist::parse_spice(
        ".subckt top a unused b\nR1 a b 1k\nR2 b c 1k\nR3 c a 1k\n.ends top\n",
    )
    .unwrap();
    let mut oracle = circuit.clone();
    oracle.modules[0]
        .connections
        .retain(|connection| connection.target.is_some());
    assert_ne!(oracle, circuit);
    apply_one_pin_port_nets(&mut oracle);
    assert_eq!(oracle, circuit);
}

#[test]
fn net_discipline_transform_matches_the_parser() {
    let circuit = oxymorph_netlist::parse_vams(
        "module top (a, b);\n  inout [1:0] a;\n  inout b;\n  electrical [1:0] a;\n  r R1 (.p(a[0]), .n(b));\nendmodule\n",
    )
    .unwrap();
    let mut oracle = circuit.clone();
    for connection in &mut oracle.modules[0].connections {
        connection.properties.clear();
    }
    assert_ne!(oracle, circuit);
    apply_net_disciplines(&mut oracle);
    assert_eq!(oracle, circuit);
}

#[test]
fn parse_and_write_match_the_oracle() {
    let cases = load_cases();
    let overrides = load_overrides();

    let mut failures: Vec<String> = Vec::new();
    let mut parse_checked = 0usize;
    let mut write_checked = 0usize;
    let mut proto_checked = 0usize;
    let mut proto_cases = 0usize;

    for case in &cases {
        let format = case["format"].as_str().unwrap();
        let name = case["name"].as_str().unwrap();
        let input = case["input"].as_str().unwrap();
        let over = &overrides[case_key(case)];
        let expected = expected_for(case, over);

        let circuit = match parse(format, input) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{format}/{name}: parse failed: {e}"));
                continue;
            }
        };
        parse_checked += 1;

        let actual: serde_json::Value = serde_json::from_str(&circuit.to_json().unwrap()).unwrap();
        if actual != transformed_json(expected.json) {
            failures.push(format!("{format}/{name}: IR differs from oracle"));
        }

        if let Some(proto_hex) = case["proto"].as_str() {
            proto_cases += 1;
            // The oracle protobuf encodes the oracle JSON (no override): decoding it gives
            // that IR; parsing is checked against the (overridden) JSON above.
            let mut oracle_json =
                Circuit::from_json(&case["json"].to_string()).expect("oracle IR is a circuit");
            apply_transforms(&mut oracle_json);
            match Circuit::from_proto_bytes(&hex_decode(proto_hex)) {
                Ok(mut oracle) => {
                    apply_transforms(&mut oracle);
                    if oracle == oracle_json {
                        proto_checked += 1;
                    } else {
                        failures.push(format!("{format}/{name}: protobuf interchange differs"));
                    }
                }
                Err(e) => failures.push(format!("{format}/{name}: protobuf decode failed: {e}")),
            }
            match Circuit::from_proto_bytes(&circuit.to_proto_bytes()) {
                Ok(c) if c == circuit => {}
                _ => failures.push(format!("{format}/{name}: protobuf round-trip differs")),
            }
        }

        for (wfmt, expected_text) in &expected.writes {
            let got = write(wfmt, &circuit);
            write_checked += 1;
            if got != *expected_text {
                failures.push(format!("{format}/{name}: write[{wfmt}] differs"));
            }
        }
    }

    assert!(parse_checked > 300, "corpus too small: {parse_checked}");
    assert!(
        proto_cases > 300,
        "protobuf corpus too small: {proto_cases}"
    );
    if !failures.is_empty() {
        let shown: Vec<String> = failures.iter().take(40).cloned().collect();
        panic!(
            "{} failures ({} parse, {} write, {} proto checked):\n{}",
            failures.len(),
            parse_checked,
            write_checked,
            proto_checked,
            shown.join("\n")
        );
    }
    // every case with an oracle protobuf has it compared, overridden or not
    assert_eq!(proto_checked, proto_cases);
}
