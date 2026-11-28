//! `.gsch` schematics, ported from kfnetlist #37 (38d28f2 … b1153f6) and the gsch hunks of
//! 9766e84, 1adb071, 30a1621, 5559e43, 8f0a4b4.
//! from: kfnetlist tests/morphnet/test_gsch_parser.py, test_gsch_writer.py,
//! test_gsch_export.py, test_gsch_conversions.py, the gsch tests of test_review_fixes.py
//! and test_vams_writer.py::TestNativeSimulationForms
//!
//! The kfnetlist bridge (`morphnet.to_netlists` / `from_netlists`) is not ported: its tests
//! are skipped, and `edited(nl, inst, **settings)` is an edit of the IR parameter.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use oxymorph_netlist::expression::{
    format_expression, parameter_value, parse_expression, Dialect, Resolve,
};
use oxymorph_netlist::gsch::parser::parameter;
use oxymorph_netlist::gsch::sim::source_props;
use oxymorph_netlist::helpers::{
    connections_to_instance_nets, is_skipped, net_map_to_connections, sim_params, NetMap,
    SourceParams,
};
use oxymorph_netlist::stimulus::stimulus_of;
use oxymorph_netlist::{
    parse_gsch, parse_hspice, parse_spectre, parse_spectre_spice, parse_spice, parse_systemverilog,
    parse_vams, parse_xyce, read_gsch, write_gsch, write_hspice, write_spectre,
    write_spectre_spice, write_spice, write_vams, write_xyce, Error, GschOptions,
};
use oxymorph_schema::{
    Circuit, DirectiveKind, Expression, Module, ModuleReference, Operator, ParameterValue, Port,
    SignalDomain, Siprefix, Stimulus, WaveformKind,
};
use regex::Regex;
use serde_json::{json, Value};

const WAVEGUIDE: SignalDomain = SignalDomain::Waveguide;
const ELECTRICAL: SignalDomain = SignalDomain::Electrical;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/kf/gsch")
}

fn fixture_path(stem: &str) -> String {
    fixtures()
        .join(format!("{stem}.gsch"))
        .to_string_lossy()
        .into_owned()
}

fn fixture_text(stem: &str) -> String {
    std::fs::read_to_string(fixture_path(stem)).unwrap()
}

fn fixture_json(stem: &str) -> Value {
    serde_json::from_str(&fixture_text(stem)).unwrap()
}

/// Every `.gsch` fixture stem, sorted.
fn stems() -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("gsch") {
            found.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    found.sort();
    found
}

fn models() -> Value {
    let text = std::fs::read_to_string(fixtures().join("models.nyanlib")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// `CORNERS` of the kfnetlist tests (as in golden/regen.py).
fn corners_of(stem: &str) -> Vec<String> {
    if stem == "ihp_hier_top" {
        vec!["mos_ff".to_string()]
    } else {
        Vec::new()
    }
}

fn sim_options(sim: &str) -> GschOptions {
    GschOptions {
        sim: sim.to_string(),
        ..Default::default()
    }
}

fn model_options(models: Value) -> GschOptions {
    GschOptions {
        models: Some(models),
        ..Default::default()
    }
}

fn named(name: &str) -> GschOptions {
    GschOptions {
        name: Some(name.to_string()),
        ..Default::default()
    }
}

fn parse(text: &str) -> Circuit {
    parse_gsch(text, &GschOptions::default()).unwrap()
}

fn parse_doc(doc: &Value) -> Circuit {
    parse(&doc.to_string())
}

fn load(stem: &str) -> Circuit {
    parse(&fixture_text(stem))
}

fn write(circuit: &Circuit) -> Value {
    serde_json::from_str(&write_gsch(circuit).unwrap()).unwrap()
}

fn export(stem: &str, sim: &str) -> Circuit {
    let options = GschOptions {
        sim: sim.to_string(),
        corners: corners_of(stem),
        ..Default::default()
    };
    read_gsch(&fixture_path(stem), None, &options).unwrap()
}

/// A fresh directory for one test (`tmp_path`).
fn temporary_directory(test: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let number = COUNTER.fetch_add(1, Ordering::SeqCst);
    let directory = std::env::temp_dir().join(format!(
        "oxymorph-gsch-{}-{test}-{number}",
        std::process::id()
    ));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn write_file(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// IR views
// ---------------------------------------------------------------------------

type Pin = (String, String);

fn pin(instance: &str, port: &str) -> Pin {
    (instance.to_string(), port.to_string())
}

fn pins(pairs: &[(&str, &str)]) -> BTreeSet<Pin> {
    let mut found = BTreeSet::new();
    for (instance, port) in pairs {
        found.insert(pin(instance, port));
    }
    found
}

/// Net name -> {(instance, pin)}; the circuit's own pins have instance `""`.
fn nets(circuit: &Circuit) -> IndexMap<String, BTreeSet<Pin>> {
    let mut found: IndexMap<String, BTreeSet<Pin>> = IndexMap::new();
    for connection in &circuit.modules[0].connections {
        let members = found.entry(connection.name.clone()).or_default();
        for end in [&connection.source, &connection.target]
            .into_iter()
            .flatten()
        {
            members.insert(pin(&end.instance_name, &end.port_name));
        }
    }
    found
}

fn expected_nets(entries: &[(&str, &[(&str, &str)])]) -> IndexMap<String, BTreeSet<Pin>> {
    let mut found = IndexMap::new();
    for (net, members) in entries {
        found.insert(net.to_string(), pins(members));
    }
    found
}

fn ext_ports(circuit: &Circuit, name: &str) -> IndexMap<String, SignalDomain> {
    let mut found = IndexMap::new();
    for ext_module in &circuit.ext_modules {
        if ext_module.name == name {
            for port in &ext_module.ports {
                found.insert(port.name.clone(), port.domain);
            }
            return found;
        }
    }
    panic!("no external module {name}");
}

fn domains(pairs: &[(&str, SignalDomain)]) -> IndexMap<String, SignalDomain> {
    let mut found = IndexMap::new();
    for (name, domain) in pairs {
        found.insert(name.to_string(), *domain);
    }
    found
}

fn port_list(ports: &[Port]) -> Vec<(String, SignalDomain)> {
    let mut found = Vec::new();
    for port in ports {
        found.push((port.name.clone(), port.domain));
    }
    found
}

fn names_of(module: &Module) -> Vec<String> {
    let mut found = Vec::new();
    for reference in &module.module_references {
        found.push(reference.name.clone());
    }
    found
}

fn masters_of(module: &Module) -> Vec<String> {
    let mut found = Vec::new();
    for reference in &module.module_references {
        found.push(reference.module_name.clone());
    }
    found
}

fn info(module: &Module) -> Value {
    serde_json::from_str(&module.properties["__info__"]).unwrap()
}

fn reference<'a>(module: &'a Module, name: &str) -> &'a ModuleReference {
    for reference in &module.module_references {
        if reference.name == name {
            return reference;
        }
    }
    panic!("no instance {name}");
}

fn reference_mut<'a>(module: &'a mut Module, name: &str) -> &'a mut ModuleReference {
    for reference in &mut module.module_references {
        if reference.name == name {
            return reference;
        }
    }
    panic!("no instance {name}");
}

fn stimulus_named<'a>(circuit: &'a Circuit, name: &str) -> &'a Stimulus {
    for stimulus in &circuit.simulation.as_ref().unwrap().stimuli {
        if stimulus.name == name {
            return stimulus;
        }
    }
    panic!("no stimulus {name}");
}

fn stimulus_named_mut<'a>(circuit: &'a mut Circuit, name: &str) -> &'a mut Stimulus {
    for stimulus in &mut circuit.simulation.as_mut().unwrap().stimuli {
        if stimulus.name == name {
            return stimulus;
        }
    }
    panic!("no stimulus {name}");
}

fn prefixed_double(value: &Option<ParameterValue>) -> f64 {
    value
        .as_ref()
        .unwrap()
        .prefixed_value
        .as_ref()
        .unwrap()
        .double_value
}

fn param_value<'a>(reference: &'a ModuleReference, name: &str) -> &'a ParameterValue {
    for parameter in &reference.parameters {
        if parameter.name == name {
            return parameter.default_value.as_ref().unwrap();
        }
    }
    panic!("no param {name}");
}

/// `edited(nl, inst, **settings)` without the bridge: the instance's param takes the new
/// value, as a bridged setting edit does (its properties stay).
fn edit(circuit: &mut Circuit, instance: &str, name: &str, value: Value) {
    let top = circuit.top_module.clone();
    for module in &mut circuit.modules {
        if module.name != top {
            continue;
        }
        let reference = reference_mut(module, instance);
        for existing in &mut reference.parameters {
            if existing.name == name {
                existing.default_value = parameter(name, &value).default_value;
                return;
            }
        }
        reference.parameters.push(parameter(name, &value));
        return;
    }
}

fn lines_of(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        found.push(line.to_string());
    }
    found
}

fn spice_lines(circuit: &Circuit) -> Vec<String> {
    lines_of(&write_spice(circuit))
}

fn writer(sim: &str) -> fn(&Circuit) -> String {
    match sim {
        "ngspice" => write_spice,
        "xyce" => write_xyce,
        _ => write_spectre,
    }
}

fn validation_message(error: Error) -> String {
    match error {
        Error::Validation(message) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// ===========================================================================
// test_gsch_parser.py
// ===========================================================================

#[test]
fn mosaic_rc_filter() {
    let circuit = load("ihp_rc_filter");
    // built-in symbols take the device templates' pins (P N -> p n)
    assert_eq!(
        nets(&circuit),
        expected_nets(&[
            ("W3", &[("V1", "p"), ("R1", "n")]),
            ("GND", &[("V1", "n"), ("C1", "n")]),
            ("P1", &[("R1", "p"), ("C1", "p"), ("", "P1")]),
        ])
    );
    let module = &circuit.modules[0];
    assert_eq!(
        (module.name.as_str(), circuit.top_module.as_str()),
        ("__top__", "__top__")
    );
    assert_eq!(info(module)["gsch_schema"], "demo");
    // the ground marker is not a pin, but its net keeps the name GND
    assert_eq!(
        port_list(&module.ports),
        vec![("P1".to_string(), ELECTRICAL)]
    );
    for connection in &module.connections {
        assert_eq!(connection.domain, ELECTRICAL);
    }
    assert_eq!(names_of(module), ["V1", "R1", "C1"]);
    assert_eq!(masters_of(module), ["vsource", "resistor", "capacitor"]);
    assert_eq!(
        ext_ports(&circuit, "resistor"),
        domains(&[("p", ELECTRICAL), ("n", ELECTRICAL)])
    );
}

#[test]
fn mosaic_props_become_si_values_or_expressions() {
    let circuit = load("ihp_rc_filter");
    let r1 = &circuit.modules[0].module_references[1];
    assert_eq!(r1.parameters.len(), 1); // nyancad: `resistance` is R's value
    let resistance = &r1.parameters[0];
    assert_eq!(resistance.name, "value");
    let value = resistance.default_value.as_ref().unwrap();
    let prefixed = value.prefixed_value.as_ref().unwrap();
    assert_eq!(prefixed.double_value, 1.0);
    assert_eq!(prefixed.prefix, Siprefix::Kilo);

    let circuit = load("ihp_cmos_inverter");
    let v2 = reference(&circuit.modules[0], "V2");
    assert!(v2.parameters.is_empty()); // its value is a stimulus of the simulation
    let stimulus = stimulus_named(&circuit, "__top__/V2");
    assert_eq!(prefixed_double(&stimulus.dc), 0.9);
    assert_eq!(prefixed_double(&stimulus.ac_magnitude), 1.0);
    assert!(stimulus.waveform.is_none()); // a `tran` string is no waveform: nyancad drops it
    let m1 = reference(&circuit.modules[0], "M1");
    let width = param_value(m1, "width");
    assert_eq!(width.prefixed_value.as_ref().unwrap().double_value, 0.15);
    assert_eq!(m1.properties["model_name"], "sg13_lv_nmos"); // the `model` prop
}

#[test]
fn mosaic_inverter_with_models_and_polylines() {
    let circuit = load("ihp_cmos_inverter");
    assert_eq!(
        nets(&circuit),
        expected_nets(&[
            ("GND", &[("M1", "b"), ("M1", "s"), ("V1", "n"), ("V2", "n")]),
            ("P1", &[("M1", "d"), ("M2", "s"), ("R1", "n"), ("", "P1")]),
            ("P2", &[("M1", "g"), ("M2", "g"), ("V2", "p"), ("", "P2")]),
            ("W5", &[("M2", "d"), ("M2", "b"), ("R1", "p"), ("V1", "p")]),
        ])
    );
    let module = &circuit.modules[0];
    let port_names: Vec<&str> = module.ports.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(port_names, ["P1", "P2"]);
    assert_eq!(reference(module, "M1").module_name, "mosfet"); // its model is not in `models`
    let mosfet_pins: Vec<String> = ext_ports(&circuit, "mosfet").keys().cloned().collect();
    assert_eq!(mosfet_pins, ["d", "g", "s", "b"]);
}

#[test]
fn livewire_polyline_joins_the_pins_it_ends_on() {
    let circuit = load("livewire_two_straights");
    assert_eq!(
        nets(&circuit),
        expected_nets(&[("net_1", &[("uuid-A:M1", "o2"), ("uuid-B:M2", "o1")])])
    );
    let module = &circuit.modules[0];
    assert_eq!(module.name, "__top__");
    assert_eq!(info(module)["gsch_schema"], Value::Null); // two different key prefixes
    assert_eq!(masters_of(module), ["gf.components.straight"; 2]);
    for connection in &module.connections {
        assert_eq!(connection.domain, WAVEGUIDE);
    }
    assert_eq!(
        ext_ports(&circuit, "gf.components.straight"),
        domains(&[("o1", WAVEGUIDE), ("o2", WAVEGUIDE)])
    );
}

#[test]
fn livewire_port_markers_attach_to_the_straight() {
    let circuit = load("livewire_port_markers");
    assert_eq!(
        nets(&circuit),
        expected_nets(&[
            ("net_in", &[("", "in"), ("straight_1", "o1")]),
            ("net_out", &[("", "out"), ("straight_1", "o2")]),
        ])
    );
    assert_eq!(
        port_list(&circuit.modules[0].ports),
        vec![
            ("in".to_string(), WAVEGUIDE),
            ("out".to_string(), WAVEGUIDE)
        ]
    );
}

#[test]
fn mixed_file_uses_device_port_types_over_pin_names() {
    let circuit = load("spclo_gc2");
    assert_eq!(
        nets(&circuit),
        expected_nets(&[
            ("W1", &[("X1", "o1"), ("X2", "o1")]),
            ("P1", &[("X1", "o2"), ("", "P1")]),
            ("P2", &[("X2", "o2"), ("", "P2")]),
        ])
    );
    assert_eq!(circuit.modules[0].name, "__top__");
    // `o2` is named like a waveguide pin but the device says electrical
    assert_eq!(
        ext_ports(&circuit, "myspclo.gc.gc"),
        domains(&[("o1", WAVEGUIDE), ("o2", ELECTRICAL)])
    );
    let mut net_domains = IndexMap::new();
    for connection in &circuit.modules[0].connections {
        net_domains.insert(connection.name.clone(), connection.domain);
    }
    assert_eq!(
        net_domains,
        domains(&[("W1", WAVEGUIDE), ("P1", ELECTRICAL), ("P2", ELECTRICAL)])
    );
    assert_eq!(
        port_list(&circuit.modules[0].ports),
        vec![
            ("P1".to_string(), ELECTRICAL),
            ("P2".to_string(), ELECTRICAL)
        ]
    );
}

#[test]
fn subcircuit_references_by_net_or_polyline() {
    let by_nets = load("ihp_subckt_parent_nets");
    assert_eq!(
        nets(&by_nets),
        expected_nets(&[("coupling", &[("A", "D"), ("B", "S")])])
    );
    assert_eq!(
        masters_of(&by_nets.modules[0]),
        ["ihp_subckt_child.gsch"; 2]
    );
    assert_eq!(
        nets(&load("ihp_subckt_parent_poly")),
        expected_nets(&[("net_1", &[("A", "D"), ("B", "S")])])
    );
}

#[test]
fn child_schematic_exposes_attached_ports_in_marker_order() {
    let circuit = load("ihp_subckt_child");
    let port_names: Vec<&str> = circuit.modules[0]
        .ports
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(port_names, ["D", "S"]);
    assert_eq!(
        nets(&circuit),
        expected_nets(&[
            ("net_1", &[("M1", "D"), ("", "D")]),
            ("net_2", &[("M1", "S"), ("", "S")]),
        ])
    );
    // G is on the device but unconnected: still a pin of the master
    let master_pins: Vec<String> = ext_ports(&circuit, "ihp.cells.fet_transistors.nmos")
        .keys()
        .cloned()
        .collect();
    assert_eq!(master_pins, ["D", "S", "G"]);
}

#[test]
fn via_joins_everything_that_ends_on_it() {
    let doc = json!({
        "a": {"type": "ckt", "name": "A", "model": "m"},
        "b": {"type": "ckt", "name": "B", "model": "m"},
        "v": {"type": "via", "name": "v1"},
        "p1": {"type": "polyline", "terminals": {
            "start": {"component_id": "a", "port_name": "e1"},
            "finish": {"component_id": "v", "port_name": "W"}}},
        "p2": {"type": "polyline", "terminals": {
            "start": {"component_id": "v", "port_name": "E"},
            "finish": {"component_id": "b", "port_name": "e1"}}},
    });
    let circuit = parse_doc(&doc);
    assert_eq!(
        nets(&circuit),
        expected_nets(&[("net_1", &[("A", "e1"), ("B", "e1")])])
    );
    assert_eq!(names_of(&circuit.modules[0]), ["A", "B"]); // no via instance
}

#[test]
fn all_polyline_terminal_forms_and_net_entries() {
    let doc = json!({
        "a": {"type": "ckt", "model": "m", "name": "A"},
        "b": {"type": "ckt", "model": "m", "name": "B"},
        "c": {"type": "ckt", "model": "m", "name": "C"},
        "d": {"type": "ckt", "model": "m", "name": "D"},
        "p1": {"type": "polyline", "terminals": {
            "start": {"device": "a", "port": "o2"}, "finish": ["b", "o1"]}},
        "p2": {"type": "polyline", "net": "bc", "terminals": {
            "start": {"instance": "b", "port": "o2"},
            "finish": {"component_id": "c", "port_name": "o1"}}},
        "n1": {"type": "net", "ports": [
            {"component_id": "c", "port_name": "o2"},
            {"component_id": "d", "port_name": "o1"}]},
    });
    assert_eq!(
        nets(&parse_doc(&doc)),
        expected_nets(&[
            ("net_1", &[("A", "o2"), ("B", "o1")]),
            ("bc", &[("B", "o2"), ("C", "o1")]),
            ("n1", &[("C", "o2"), ("D", "o1")]), // a net entry's key is the net name
        ])
    );
}

fn named_net_entry(entry: Value, pin_nets: Value) -> IndexMap<String, BTreeSet<Pin>> {
    let mut net_entry = json!({"type": "net", "ports": [
        {"component_id": "a", "port_name": "o1"},
        {"component_id": "b", "port_name": "o1"}]});
    for (key, value) in entry.as_object().unwrap() {
        net_entry[key] = value.clone();
    }
    let doc = json!({
        "a": {"type": "ckt", "name": "A", "model": "m", "nets": pin_nets},
        "b": {"type": "ckt", "name": "B", "model": "m"},
        "k": net_entry,
    });
    nets(&parse_doc(&doc))
}

#[test]
fn net_entry_name_is_id_then_key_and_ranks_like_a_mosaic_name() {
    let members: &[(&str, &str)] = &[("A", "o1"), ("B", "o1")];
    assert_eq!(
        named_net_entry(json!({}), json!({})),
        expected_nets(&[("k", members)])
    );
    assert_eq!(
        named_net_entry(json!({"id": "sig"}), json!({})),
        expected_nets(&[("sig", members)])
    );
    assert_eq!(
        named_net_entry(json!({"id": "sig", "_id": "other"}), json!({})),
        expected_nets(&[("sig", members)])
    );
    assert_eq!(
        named_net_entry(json!({"_id": "other"}), json!({})),
        expected_nets(&[("other", members)])
    );
    // a Mosaic `nets` name on one of the pins wins, as it does over any other name
    assert_eq!(
        named_net_entry(json!({"id": "sig"}), json!({"o1": "vdd"})),
        expected_nets(&[("vdd", members)])
    );
}

#[test]
fn duplicate_names_are_made_unique_without_losing_an_instance() {
    // the second entry's key `b` equals the first entry's name `b`
    let doc = json!({
        "x": {"type": "ckt", "name": "b", "model": "m", "nets": {"o1": "n"}},
        "b": {"type": "ckt", "name": "b", "model": "m", "nets": {"o1": "n"}},
        "c": {"type": "ckt", "name": "b_1", "model": "m"},
    });
    let circuit = parse_doc(&doc);
    let names = names_of(&circuit.modules[0]);
    let unique: BTreeSet<&String> = names.iter().collect();
    assert_eq!((names.len(), unique.len()), (3, 3));
    assert_eq!(names[0], "b");
    let found: Vec<BTreeSet<Pin>> = nets(&circuit).into_values().collect();
    assert_eq!(found, vec![pins(&[("b", "o1"), (names[1].as_str(), "o1")])]);
}

#[test]
fn models_argument_gives_port_order_and_domains() {
    let doc = json!({
        "x": {"type": "ckt", "name": "X", "model": "pdk.cell", "nets": {"b": "n", "a": "m"}},
    });
    let mut models = json!({
        "models:pdk.cell": {
            "type": "ckt",
            "ports": [
                {"name": "z", "type": "photonic"},
                {"name": "a", "type": "electric"},
            ],
            "models": [
                {"language": "spice", "spice-type": "X", "port-order": ["b", "a", "z"]}
            ],
        }
    });
    let circuit = parse_gsch(&doc.to_string(), &model_options(models.clone())).unwrap();
    let cell = ext_ports(&circuit, "pdk.cell");
    let order: Vec<&String> = cell.keys().collect();
    assert_eq!(order, ["b", "a", "z"]);
    assert_eq!(cell["z"], WAVEGUIDE);
    assert_eq!(cell["a"], ELECTRICAL);
    // without a model entry: an X call with the nyanlib ports sorted by name
    models["models:pdk.cell"]
        .as_object_mut()
        .unwrap()
        .remove("models");
    let circuit = parse_gsch(&doc.to_string(), &model_options(models)).unwrap();
    let order: Vec<String> = ext_ports(&circuit, "pdk.cell").keys().cloned().collect();
    assert_eq!(order, ["a", "z"]);
}

#[test]
fn inline_models_are_merged_under_the_argument_and_are_not_entries() {
    let inline = json!({"models:pdk.cell": {"ports": [{"name": "q", "type": "photonic"}]}});
    let doc = json!({"models": inline, "x": {"type": "ckt", "name": "X", "model": "pdk.cell"}});
    let circuit = parse_doc(&doc);
    assert_eq!(names_of(&circuit.modules[0]), ["X"]);
    assert_eq!(
        ext_ports(&circuit, "pdk.cell"),
        domains(&[("q", WAVEGUIDE)])
    );
    let override_models =
        json!({"models:pdk.cell": {"ports": [{"name": "r", "type": "electric"}]}});
    let circuit = parse_gsch(&doc.to_string(), &model_options(override_models)).unwrap();
    assert_eq!(
        ext_ports(&circuit, "pdk.cell"),
        domains(&[("r", ELECTRICAL)])
    );
}

#[test]
fn circuit_name_argument_wins_over_key_prefix() {
    let text = fixture_text("ihp_rc_filter");
    assert_eq!(parse_gsch(&text, &named("rc")).unwrap().top_module, "rc");
    let resistor = json!({"r": {"type": "resistor"}});
    assert_eq!(parse_doc(&resistor).top_module, "__top__");
}

fn stash_keys(stash: &Value, field: &str) -> Vec<String> {
    let mut found = Vec::new();
    for item in stash[field].as_array().unwrap() {
        found.push(item["key"].as_str().unwrap().to_string());
    }
    found
}

#[test]
fn raw_entries_are_stashed_for_the_writer() {
    let circuit = load("spclo_gc2");
    let module = &circuit.modules[0];
    let raw = fixture_json("spclo_gc2");
    let stash = info(module);
    assert_eq!(stash["gsch_schema"], "gc2");
    assert_eq!(stash_keys(&stash, "gsch_ports"), ["gc2:P1", "gc2:P2"]);
    assert_eq!(stash_keys(&stash, "gsch_wires"), ["gc2:W1"]);
    assert_eq!(stash_keys(&stash, "gsch_routes"), ["poly_1"]);
    assert_eq!(stash["gsch_routes"][0]["entry"], raw["poly_1"]);
    let x1: Value =
        serde_json::from_str(&module.module_references[0].properties["__info__"]).unwrap();
    assert_eq!(
        x1,
        json!({
            "__gsch__": {"key": "gc2:X1", "entry": raw["gc2:X1"]},
            "__model__": "myspclo.gc.gc",
        })
    );
}

#[test]
fn non_object_documents_are_rejected() {
    for bad in ["[]", "3"] {
        let error = parse_gsch(bad, &GschOptions::default()).unwrap_err();
        assert!(validation_message(error).contains("JSON object"), "{bad}");
    }
}

#[test]
fn a_dangling_mosaic_label_is_a_net_and_round_trips() {
    let doc = json!({
        "a": {"type": "ckt", "name": "A", "model": "m", "nets": {"o1": "x", "o2": "lone"}},
        "b": {"type": "ckt", "name": "B", "model": "m", "nets": {"o1": "x"}},
    });
    let circuit = parse_doc(&doc);
    assert_eq!(
        nets(&circuit),
        expected_nets(&[("x", &[("A", "o1"), ("B", "o1")]), ("lone", &[("A", "o2")])])
    );
    let out = write(&circuit);
    assert_eq!(out["a"]["nets"], json!({"o1": "x", "o2": "lone"}));
    assert_eq!(nets(&parse_doc(&out)), nets(&circuit));
}

// ===========================================================================
// test_gsch_writer.py
// ===========================================================================

/// Mosaic's accepted entry types (common.cljc `device-types` + `schematic-only-types`).
const MOSAIC_TYPES: &str = "pmos nmos npn pnp resistor capacitor inductor vsource isource diode \
    led photodiode modulator ckt amp die straight bend sbend taper transition terminator \
    crossing ring-single ring-double spiral splitter-1x2 coupler coupler-ring mmi-1x2 mmi-2x2 \
    mzi-1x2 mzi-2x2 grating-coupler wire port text polyline group via";

type Structure = (
    BTreeSet<BTreeSet<Pin>>,
    Vec<(String, SignalDomain)>,
    Vec<(String, String)>,
);

/// Everything the writer must preserve: nets, pins, instances. `fold_pin_case`: Mosaic
/// symbols spell pins uppercase (`p` -> `P`).
fn structure(circuit: &Circuit, fold_pin_case: bool) -> Structure {
    let module = &circuit.modules[0];
    let mut by_net: IndexMap<String, BTreeSet<Pin>> = IndexMap::new();
    for connection in &module.connections {
        let members = by_net.entry(connection.name.clone()).or_default();
        for end in [&connection.source, &connection.target]
            .into_iter()
            .flatten()
        {
            let port_name = if fold_pin_case {
                end.port_name.to_uppercase()
            } else {
                end.port_name.clone()
            };
            members.insert((end.instance_name.clone(), port_name));
        }
    }
    let mut instances = Vec::new();
    for reference in &module.module_references {
        instances.push((reference.name.clone(), reference.module_name.clone()));
    }
    (
        by_net.into_values().collect(),
        port_list(&module.ports),
        instances,
    )
}

#[test]
fn parse_write_parse_keeps_structure() {
    for stem in stems() {
        let circuit = load(&stem);
        let again =
            parse_gsch(&write_gsch(&circuit).unwrap(), &named(&circuit.top_module)).unwrap();
        assert_eq!(
            structure(&again, false),
            structure(&circuit, false),
            "{stem}"
        );
        // and the output is stable
        assert_eq!(
            write_gsch(&again).unwrap(),
            write_gsch(&circuit).unwrap(),
            "{stem}"
        );
    }
}

#[test]
fn stashed_entries_come_back_verbatim() {
    for stem in stems() {
        let raw = fixture_json(&stem);
        let out = write(&parse_doc(&raw));
        let raw_keys: BTreeSet<&String> = raw.as_object().unwrap().keys().collect();
        let out_keys: BTreeSet<&String> = out.as_object().unwrap().keys().collect();
        assert_eq!(out_keys, raw_keys, "{stem}"); // nothing lost, nothing synthesized
        for (key, entry) in raw.as_object().unwrap() {
            let kind = entry["type"].as_str().unwrap_or("");
            if ["wire", "text", "group", "via", "taper", "polyline"].contains(&kind) {
                assert_eq!(&out[key], entry, "{stem} {key}");
                continue;
            }
            // layout fields are kept; only name, nets, props are rewritten
            for (field, value) in entry.as_object().unwrap() {
                if !["name", "nets", "props"].contains(&field.as_str()) {
                    assert_eq!(&out[key][field], value, "{stem} {key} {field}");
                }
            }
            let empty = json!({});
            let raw_props = entry.get("props").unwrap_or(&empty);
            let out_props = out[key].get("props").unwrap_or(&empty);
            assert_eq!(out_props, raw_props, "{stem} {key}");
        }
    }
}

#[test]
fn pin_spelling_of_the_editor_is_restored() {
    let mut circuit = load("ihp_rc_filter");
    for connection in &mut circuit.modules[0].connections {
        for end in [&mut connection.source, &mut connection.target]
            .into_iter()
            .flatten()
        {
            if !end.instance_name.is_empty() {
                end.port_name = end.port_name.to_lowercase();
            }
        }
    }
    assert_eq!(
        write(&circuit)["demo:R1"]["nets"],
        json!({"P": "P1", "N": "W3"})
    );
}

#[test]
fn renamed_instance_and_changed_master_are_written() {
    let mut circuit = load("ihp_rc_filter");
    circuit.modules[0].module_references[1].module_name = "capacitor".to_string();
    let out = &write(&circuit)["demo:R1"];
    assert_eq!(out["type"], "capacitor");
    assert_eq!(out["props"], json!({"capacitance": "1k"})); // the value, under the new symbol
    assert_eq!(out["x"], 4); // layout kept
}

#[test]
fn polylines_are_rewritten_to_component_id_form_and_orphans_dropped() {
    let doc = json!({
        "a": {"type": "ckt", "model": "m", "x": 0, "y": 0, "name": "A"},
        "b": {"type": "ckt", "model": "m", "x": 0, "y": 0, "name": "B"},
        "p1": {"type": "polyline", "terminals": {
            "start": {"device": "a", "port": "o2"}, "finish": ["b", "o1"]}},
        "p2": {"type": "polyline", "terminals": {
            "start": {"instance": "b", "port": "o2"}, "finish": ["a", "o1"]}},
    });
    let mut circuit = parse_doc(&doc);
    let out = write(&circuit);
    for key in ["p1", "p2"] {
        for end in out[key]["terminals"].as_object().unwrap().values() {
            let fields: BTreeSet<&String> = end.as_object().unwrap().keys().collect();
            assert_eq!(
                fields,
                BTreeSet::from([&"component_id".to_string(), &"port_name".to_string()])
            );
        }
    }
    assert_eq!(
        out["p1"]["terminals"]["finish"],
        json!({"component_id": "b", "port_name": "o1"})
    );

    circuit.modules[0].module_references.pop(); // B is gone: both polylines end on it
    let keys: Vec<String> = write(&circuit)
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["a"]);
}

#[test]
fn net_entries_keep_only_live_endpoints() {
    let doc = json!({
        "a": {"type": "ckt", "model": "m", "x": 0, "y": 0, "name": "A"},
        "b": {"type": "ckt", "model": "m", "x": 0, "y": 0, "name": "B"},
        "n": {"type": "net", "ports": [
            {"component_id": "a", "port_name": "o1"},
            {"component_id": "b", "port_name": "o1"}]},
    });
    let mut circuit = parse_doc(&doc);
    circuit.modules[0].module_references.pop();
    assert_eq!(
        write(&circuit)["n"]["ports"],
        json!([{"component_id": "a", "port_name": "o1"}])
    );
}

#[test]
fn inline_models_survive() {
    let doc = json!({"models": {"models:m": {"ports": []}}, "a": {"type": "ckt", "name": "A"}});
    assert_eq!(write(&parse_doc(&doc))["models"], doc["models"]);
}

// ── writing from sources that never saw a .gsch ─────────────────────────────────

const RC: &str = ".subckt rc in out gnd\nR1 in out 1k\nC1 out gnd 100p\n.ends rc\n";
const PHOTONIC: &str = "module mzi (input o1, output o2, input e1);
  wg_straight X1 (.o1(o1), .o2(mid));
  wg_straight X2 (.o1(mid), .o2(o2));
  heater H (.e1(e1));
endmodule
";

fn parse_photonic() -> Circuit {
    parse_systemverilog(PHOTONIC, &Default::default()).unwrap()
}

fn check_synthesized(text: &str) -> Value {
    let out: Value = serde_json::from_str(text).unwrap();
    let mosaic_types: Vec<&str> = MOSAIC_TYPES.split_whitespace().collect();
    let mut places = BTreeSet::new();
    for (key, entry) in out.as_object().unwrap() {
        let kind = entry["type"].as_str().unwrap();
        assert!(mosaic_types.contains(&kind), "{key}");
        assert!(kind != "net" && kind != "subckt");
        if kind != "wire" {
            for field in ["name", "x", "y"] {
                assert!(entry.get(field).is_some(), "{key} {field}");
            }
            assert_eq!(entry["name"], json!(key));
        }
        if kind == "polyline" {
            for end in entry["terminals"].as_object().unwrap().values() {
                assert_eq!(end.as_object().unwrap().len(), 2);
            }
        }
        places.insert((entry["x"].to_string(), entry["y"].to_string()));
    }
    assert_eq!(places.len(), out.as_object().unwrap().len()); // no overlap
    out
}

#[test]
fn other_formats_write_a_valid_schematic_with_the_same_connectivity() {
    for circuit in [parse_spice(RC).unwrap(), parse_photonic()] {
        let text = write_gsch(&circuit).unwrap();
        let out = check_synthesized(&text);
        let mut names = BTreeSet::new();
        for entry in out.as_object().unwrap().values() {
            names.insert(entry["name"].as_str().unwrap().to_string());
        }
        let module = &circuit.modules[0];
        let mut expected = BTreeSet::new();
        for reference in &module.module_references {
            expected.insert(reference.name.clone());
        }
        for port in &module.ports {
            expected.insert(port.name.clone());
        }
        assert_eq!(names, expected);
        let reparsed = parse_gsch(&text, &named(&circuit.top_module)).unwrap();
        assert_eq!(structure(&reparsed, true), structure(&circuit, true));
    }
}

#[test]
fn rc_is_written_as_mosaic_symbols_with_values() {
    let out = write(&parse_spice(RC).unwrap());
    assert_eq!(out["R1"]["type"], "resistor");
    assert!(out["R1"].get("model").is_none());
    assert_eq!(out["R1"]["props"], json!({"resistance": "1k"})); // Mosaic's name for the value
                                                                 // Mosaic's resistor pins are `P` and `N`: SPICE's `p`/`n` would not connect
    assert_eq!(out["R1"]["nets"], json!({"P": "in", "N": "out"}));
    assert_eq!(out["in"]["nets"], json!({"P": "in"}));
    assert_eq!(
        out["in"]["attached_port"],
        json!({"component_id": "R1", "port_name": "P"})
    );
    assert_eq!(out["in"]["nature"], "electric");
}

#[test]
fn only_pins_of_the_mosaic_symbol_are_uppercased() {
    let mut circuit = parse_spice(RC).unwrap();
    circuit.modules[0].module_references[0].module_name = "nmos".to_string(); // pins D G S B
    let out = write(&circuit);
    assert_eq!(out["R1"]["nets"], json!({"p": "in", "n": "out"})); // not nmos pins: verbatim
    assert_eq!(out["C1"]["nets"], json!({"P": "out", "N": "gnd"})); // capacitor: uppercased
}

#[test]
fn photonic_module_is_written_as_ckt_models_and_photonic_ports() {
    let out = write(&parse_photonic());
    assert_eq!(
        (out["X1"]["type"].clone(), out["X1"]["model"].clone()),
        (json!("ckt"), json!("wg_straight"))
    );
    assert_eq!(out["o1"]["nature"], "photonic");
    assert_eq!(out["e1"]["nature"], "electric");
    let reparsed = parse(&write_gsch(&parse_photonic()).unwrap());
    let mut found = IndexMap::new();
    for port in &reparsed.modules[0].ports {
        found.insert(port.name.clone(), port.domain);
    }
    assert_eq!(
        found,
        domains(&[("o1", WAVEGUIDE), ("o2", WAVEGUIDE), ("e1", ELECTRICAL)])
    );
}

#[test]
fn colliding_names_do_not_overwrite_each_other() {
    let mut circuit = parse_spice(".subckt t a\nR1 a b 1k\nR2 b a 1k\n.ends t\n").unwrap();
    circuit.modules[0].module_references[0].name = "a".to_string(); // named like the port
    circuit.modules[0].connections.clear();
    let out = write(&circuit);
    let entries = out.as_object().unwrap();
    assert_eq!(entries.len(), 3);
    let mut names = BTreeSet::new();
    for entry in entries.values() {
        names.insert(entry["name"].as_str().unwrap().to_string());
    }
    assert_eq!(names, BTreeSet::from(["a".to_string(), "R2".to_string()]));
    // the port keeps its name; its key is suffixed and recorded as `id`
    assert_eq!(out["a"]["type"], "resistor");
    assert_eq!(
        (out["a_"]["name"].clone(), out["a_"]["id"].clone()),
        (json!("a"), json!("a_"))
    );
    assert!(out["a"].get("id").is_none() && out["R2"].get("id").is_none());
}

#[test]
fn duplicate_names_survive_parse_write_parse() {
    let doc = json!({
        "x": {"type": "ckt", "name": "b", "model": "m", "nets": {"o1": "n"}},
        "b": {"type": "ckt", "name": "b", "model": "m", "nets": {"o1": "n"}},
    });
    let circuit = parse_doc(&doc);
    let out = write(&circuit);
    let keys: BTreeSet<&String> = out.as_object().unwrap().keys().collect();
    assert_eq!(keys, BTreeSet::from([&"x".to_string(), &"b".to_string()])); // nothing overwritten
    let mut names = BTreeSet::new();
    for entry in out.as_object().unwrap().values() {
        names.insert(entry["name"].as_str().unwrap().to_string());
    }
    assert_eq!(names.len(), 2);
    assert_eq!(
        structure(&parse_doc(&out), false),
        structure(&circuit, false)
    );
    assert_eq!(structure(&circuit, false).2.len(), 2);
}

// ── connectivity edited on an IR that came from a .gsch ──────────────────────────

/// Rewire `instance.pin` to the pin `to` of the same instance.
fn move_pin(circuit: &mut Circuit, instance: &str, pin_name: &str, to: &str) {
    for connection in &mut circuit.modules[0].connections {
        for end in [&mut connection.source, &mut connection.target]
            .into_iter()
            .flatten()
        {
            if end.instance_name == instance && end.port_name == pin_name {
                end.port_name = to.to_string();
            }
        }
    }
}

/// Move the `(instance, pin)` pins onto the net called `net`.
fn rebind(circuit: &mut Circuit, net: &str, moved: &[(&str, &str)]) {
    let module = &mut circuit.modules[0];
    let moved_pins = pins(moved);
    let mut members: IndexMap<String, Vec<Pin>> = IndexMap::new();
    for connection in &module.connections {
        for end in [&connection.source, &connection.target]
            .into_iter()
            .flatten()
        {
            let end_pin = pin(&end.instance_name, &end.port_name);
            if moved_pins.contains(&end_pin) {
                continue;
            }
            let net_members = members.entry(connection.name.clone()).or_default();
            if !net_members.contains(&end_pin) {
                net_members.push(end_pin);
            }
        }
    }
    let mut net_map = NetMap::new();
    for (net_name, net_members) in &members {
        for (instance, port) in net_members {
            net_map.add(net_name, instance, port);
        }
    }
    for (instance, port) in moved {
        net_map.add(net, instance, port);
    }
    // an edited-away pin left alone on a net is no net (no source-only Connection)
    let mut connections = Vec::new();
    for connection in net_map_to_connections(&net_map) {
        if connection.target.is_some() {
            connections.push(connection);
        }
    }
    module.connections = connections;
}

fn parsed_nets(circuit: &Circuit) -> BTreeSet<BTreeSet<Pin>> {
    structure(&parse(&write_gsch(circuit).unwrap()), false).0
}

fn net_sets(entries: &[&[(&str, &str)]]) -> BTreeSet<BTreeSet<Pin>> {
    let mut found = BTreeSet::new();
    for members in entries {
        found.insert(pins(members));
    }
    found
}

#[test]
fn moving_a_pin_to_another_net_is_written_for_mosaic_nets() {
    let mut circuit = load("ihp_rc_filter");
    rebind(&mut circuit, "W3", &[("C1", "p")]); // C1.P leaves net P1 for W3
    assert_eq!(
        write(&circuit)["demo:C1"]["nets"],
        json!({"N": "GND", "P": "W3"})
    );
    assert_eq!(
        parsed_nets(&circuit),
        net_sets(&[
            &[("V1", "p"), ("R1", "n"), ("C1", "p")],
            &[("V1", "n"), ("C1", "n")],
            &[("R1", "p"), ("", "P1")],
        ])
    );
}

#[test]
fn port_marker_is_rebound_when_its_pin_moves() {
    let mut circuit = load("ihp_subckt_child");
    move_pin(&mut circuit, "M1", "D", "G"); // port D now sits on M1.G
    let out = write(&circuit);
    assert_eq!(
        out["port_D"]["attached_port"],
        json!({"component_id": "nmos_1", "port_name": "G"})
    );
    assert_eq!(out["port_S"]["attached_port"]["port_name"], "S"); // untouched
    assert!(parsed_nets(&circuit).contains(&pins(&[("", "D"), ("M1", "G")])));
}

#[test]
fn marker_loses_its_binding_when_no_instance_is_left_on_its_net() {
    let mut circuit = load("ihp_subckt_child");
    circuit.modules[0].module_references.pop(); // M1 is gone
    circuit.modules[0].connections.clear();
    let out = write(&circuit);
    assert!(out["port_D"].get("attached_port").is_none());
    assert_eq!(parsed_nets(&circuit), BTreeSet::new());
}

#[test]
fn polyline_is_dropped_when_its_pins_are_no_longer_one_net() {
    let mut circuit = load("ihp_subckt_parent_poly");
    assert!(write(&circuit).get("poly_coupling").is_some()); // unchanged: kept
    move_pin(&mut circuit, "A", "D", "S"); // A.D leaves the net it shared with B.S
    assert!(write(&circuit).get("poly_coupling").is_none());
    for net in parsed_nets(&circuit) {
        assert!(!net.contains(&pin("A", "D")));
    }
}

fn sig_circuit() -> Circuit {
    let mut doc = json!({});
    for key in ["a", "b", "c"] {
        doc[key] = json!({"type": "ckt", "model": "m", "x": 0, "y": 0, "name": key.to_uppercase()});
    }
    doc["sig"] = json!({"type": "net", "ports": [
        {"component_id": "a", "port_name": "o1"},
        {"component_id": "b", "port_name": "o1"},
        {"component_id": "c", "port_name": "o1"}]});
    parse_doc(&doc)
}

#[test]
fn net_entry_keeps_only_the_pins_still_on_its_net() {
    let mut circuit = sig_circuit();
    move_pin(&mut circuit, "B", "o1", "o2"); // B's o1 becomes o2: the net now holds B.o2
    let out = write(&circuit);
    let mut kept = Vec::new();
    for port in out["sig"]["ports"].as_array().unwrap() {
        kept.push(port["component_id"].as_str().unwrap().to_string());
    }
    assert_eq!(kept, ["a", "c"]);
    // B.o1 is not put back on `sig`, and the net keeps its name
    assert_eq!(
        nets(&parse_doc(&out)),
        expected_nets(&[("sig", &[("A", "o1"), ("B", "o2"), ("C", "o1")])])
    );
}

#[test]
fn net_entry_whose_pins_left_for_another_net_is_dropped() {
    let mut circuit = sig_circuit();
    rebind(&mut circuit, "other", &[("B", "o1"), ("C", "o1")]); // both leave `sig`
    let out = write(&circuit);
    assert!(out.get("sig").is_none()); // A.o1 alone is no net
    assert_eq!(
        nets(&parse_doc(&out)),
        expected_nets(&[("other", &[("B", "o1"), ("C", "o1")])])
    );
}

#[test]
fn net_entry_is_dropped_when_no_pin_is_left_on_its_net() {
    let mut circuit = sig_circuit();
    circuit.modules[0].connections.clear();
    assert!(write(&circuit).get("sig").is_none());
}

#[test]
fn net_entry_follows_a_renamed_net_of_its_surviving_pins() {
    let mut circuit = sig_circuit();
    for connection in &mut circuit.modules[0].connections {
        connection.name = "renamed".to_string();
    }
    let out = write(&circuit);
    assert_eq!(out["sig"]["ports"].as_array().unwrap().len(), 3); // still one net
    let names: Vec<String> = nets(&parse_doc(&out)).keys().cloned().collect();
    assert_eq!(names, ["renamed"]);
}

#[test]
fn edited_connectivity_round_trips_for_every_fixture() {
    for stem in stems() {
        let circuit = load(&stem);
        assert_eq!(
            parsed_nets(&circuit),
            structure(&circuit, false).0,
            "{stem}"
        );
    }
}

#[test]
fn a_netlist_without_circuits_is_rejected() {
    let error = write_gsch(&Circuit::default()).unwrap_err();
    assert!(validation_message(error).contains("no circuit"));
}

// ── Livewire-only entries: `nets` only where an edit needs it ───────────────────

fn sim_skips(circuit: &Circuit) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    for reference in &circuit.modules[0].module_references {
        found.push((
            reference.name.clone(),
            reference.properties.get("sim_skip").cloned(),
        ));
    }
    found
}

#[test]
fn livewire_only_entries_get_no_nets_and_simulate_the_same() {
    for stem in ["livewire_two_straights", "ihp_subckt_child"] {
        let raw = fixture_json(stem);
        let circuit = parse_doc(&raw);
        let out = write(&circuit);
        for (key, entry) in raw.as_object().unwrap() {
            if entry.get("nets").is_none() {
                assert!(out[key].get("nets").is_none(), "{stem} {key}");
            }
        }
        let again = parse_doc(&out);
        assert_eq!(sim_skips(&again), sim_skips(&circuit), "{stem}");
        assert_eq!(write_spice(&again), write_spice(&circuit), "{stem}");
    }
}

#[test]
fn editing_a_livewire_pin_adds_nets_for_that_pin_only() {
    let doc = json!({
        "a": {"type": "ckt", "name": "A", "model": "m", "nets": {"o1": "sig"}},
        "b": {"type": "ckt", "name": "B", "model": "m"},
        "c": {"type": "ckt", "name": "C", "model": "m"},
        "p1": {"type": "polyline", "terminals": {
            "start": {"component_id": "a", "port_name": "o1"},
            "finish": {"component_id": "b", "port_name": "o1"}}},
        "p2": {"type": "polyline", "terminals": {
            "start": {"component_id": "b", "port_name": "o2"},
            "finish": {"component_id": "c", "port_name": "o1"}}},
    });
    let mut circuit = parse_doc(&doc);
    assert!(write(&circuit)["b"].get("nets").is_none());
    rebind(&mut circuit, "sig", &[("C", "o1")]); // C.o1 leaves B.o2 for `sig`
    let out = write(&circuit);
    assert!(out["b"].get("nets").is_none()); // B.o1 is on `sig` through the kept polyline
    assert_eq!(out["c"]["nets"], json!({"o1": "sig"}));
    assert_eq!(
        nets(&parse_doc(&out)),
        expected_nets(&[("sig", &[("A", "o1"), ("B", "o1"), ("C", "o1")])])
    );
}

#[test]
fn an_edit_without_a_named_part_names_all_its_livewire_pins() {
    let mut circuit = load("livewire_port_markers");
    rebind(&mut circuit, "net_in", &[("straight_1", "o2")]);
    let out = write(&circuit);
    // the marker names `net_in` only weakly (it has an attached_port)
    assert_eq!(
        out["straight_1"]["nets"],
        json!({"o1": "net_in", "o2": "net_in"})
    );
    assert_eq!(
        nets(&parse_doc(&out))["net_in"],
        pins(&[("", "in"), ("straight_1", "o1"), ("straight_1", "o2")])
    );
}

// ── params written back to props (fix round 1) ─────────────────────────────────

#[test]
fn a_value_set_on_a_parsed_resistor_without_one_is_written() {
    let doc = json!({
        "r": {"type": "resistor", "name": "R1", "nets": {"P": "a", "N": "b"}},
        "c": {"type": "capacitor", "name": "C1", "nets": {"P": "a", "N": "b"},
              "props": {"capacitance": "1p"}},
    });
    let mut circuit = parse_doc(&doc);
    circuit.modules[0].module_references[0].parameters = vec![parameter("value", &json!("2k"))];
    assert_eq!(write(&circuit)["r"]["props"], json!({"resistance": "2k"}));
}

#[test]
fn a_param_added_to_a_skipped_instance_is_written() {
    let mut circuit = load("livewire_port_markers");
    let straight = &mut circuit.modules[0].module_references[0];
    assert_eq!(straight.properties["sim_skip"], "1");
    straight.parameters.push(parameter("width", &json!(0.5)));
    assert_eq!(
        write(&circuit)["straight_1"]["props"],
        json!({"length": 20, "width": 0.5})
    );
}

#[test]
fn model_derived_params_and_defaults_stay_out_of_props() {
    let text = fixture_text("ihp_hier_top");
    let raw = fixture_json("ihp_hier_top");
    let out: Value = serde_json::from_str(
        &write_gsch(&parse_gsch(&text, &model_options(models())).unwrap()).unwrap(),
    )
    .unwrap();
    for key in ["top:X2", "top:M2", "top:V1", "top:I1"] {
        // defaults, l={..}, PULSE
        assert_eq!(out[key]["props"], raw[key]["props"], "{key}");
    }
}

// ===========================================================================
// test_gsch_export.py
// ===========================================================================

fn goldens() -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(fixtures().join("golden")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.matches('.').count() == 2 && (name.ends_with(".cir") || name.ends_with(".scs")) {
            found.push(name);
        }
    }
    found.sort();
    found
}

/// A number (any SI suffix or unit) as its value to 9 digits; anything else without spaces.
fn view_value(text: &str, fold: bool) -> String {
    static NUMBER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let number = NUMBER.get_or_init(|| {
        Regex::new(r"(?i)^([+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)(meg|[tgkmunpf])?(v|a|s|hz)?$")
            .unwrap()
    });
    let text = text.trim();
    if let Some(captures) = number.captures(text) {
        let scale = match captures
            .get(2)
            .map(|suffix| suffix.as_str().to_lowercase())
            .as_deref()
        {
            Some("t") => 1e12,
            Some("g") => 1e9,
            Some("meg") => 1e6,
            Some("k") => 1e3,
            Some("m") => 1e-3,
            Some("u") => 1e-6,
            Some("n") => 1e-9,
            Some("p") => 1e-12,
            Some("f") => 1e-15,
            _ => 1.0,
        };
        let value: f64 = captures[1].parse::<f64>().unwrap() * scale;
        return format!("#{value:.8e}");
    }
    let squeezed: String = text.split_whitespace().collect();
    if fold {
        squeezed.to_lowercase()
    } else {
        squeezed
    }
}

/// One outer `{…}`/`'…'` is dialect wrapping, not circuit content.
fn unwrap_value(text: &str) -> &str {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() >= 2
        && matches!(
            (bytes[0], bytes[bytes.len() - 1]),
            (b'{', b'}') | (b'\'', b'\'')
        )
    {
        return &text[1..text.len() - 1];
    }
    text
}

/// `(start, end)` of each whitespace-separated token of `text`.
fn token_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some(begin) = start.take() {
                spans.push((begin, index));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        spans.push((begin, text.len()));
    }
    spans
}

/// Where `token` has an `=` after its first character (`\S+=`).
fn assignment_at(token: &str) -> Option<usize> {
    for (index, character) in token.char_indices().skip(1) {
        if character == '=' {
            return Some(index);
        }
    }
    None
}

/// Python `re.findall(r"(\S+?)=(.*?)(?=\s+\S+=|$)", tail.strip())`.
fn assignments(tail: &str) -> Vec<(String, String)> {
    let text = tail.trim();
    let spans = token_spans(text);
    let mut starts = Vec::new();
    for (index, (start, end)) in spans.iter().enumerate() {
        if let Some(equals) = assignment_at(&text[*start..*end]) {
            starts.push((index, start + equals));
        }
    }
    let mut found = Vec::new();
    for (position, (token_index, equals)) in starts.iter().enumerate() {
        let key = &text[spans[*token_index].0..*equals];
        let value_end = match starts.get(position + 1) {
            Some((next_index, _)) => spans[next_index - 1].1,
            None => text.len(),
        };
        found.push((key.to_string(), text[equals + 1..value_end].to_string()));
    }
    found
}

type ViewParams = Vec<(String, String)>;

fn view_params(tail: &str, fold: bool) -> ViewParams {
    let mut found = BTreeMap::new();
    for (key, value) in assignments(tail) {
        let key = if fold { key.to_lowercase() } else { key };
        found.insert(key, view_value(unwrap_value(&value), fold));
    }
    found.into_iter().collect()
}

/// `head` (positional tokens) and `tail` (`k=v` params) of a line: split before the first
/// whitespace that precedes a `k=` token (Python `re.search(r"\s\S+=", line)`).
fn split_line(line: &str) -> (String, String) {
    let line = line.replace(" PARAMS:", ""); // Xyce keyword
    for (start, end) in token_spans(&line) {
        if start > 0 && assignment_at(&line[start..end]).is_some() {
            let whitespace_start = line[..start].char_indices().last().unwrap().0;
            return (
                line[..whitespace_start].to_string(),
                line[whitespace_start..].to_string(),
            );
        }
    }
    (line, String::new())
}

type Element = (String, Vec<String>, Vec<String>, ViewParams);

#[derive(Debug, Default, PartialEq)]
struct NetlistView {
    libraries: BTreeSet<Vec<String>>,
    headers: BTreeMap<String, (Vec<String>, ViewParams)>,
    parameters: BTreeMap<String, ViewParams>,
    elements: BTreeMap<String, BTreeSet<Element>>,
}

fn node_count(letter: char) -> Option<usize> {
    match letter {
        'R' | 'C' | 'L' | 'V' | 'I' | 'D' => Some(2),
        'Q' => Some(3),
        'M' => Some(4),
        _ => None,
    }
}

fn spice_view(text: &str, golden: bool) -> NetlistView {
    static SEPARATORS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let separators = SEPARATORS.get_or_init(|| Regex::new(r"[\s()]+").unwrap());
    let mut view = NetlistView::default();
    let mut scope = "__top__".to_string();
    for line in text.lines() {
        let line = line.trim();
        let low = line.to_lowercase();
        if low.starts_with(".lib ") {
            let mut library = Vec::new();
            for token in line.split_whitespace() {
                library.push(token.trim_matches('"').to_string());
            }
            view.libraries.insert(library);
        } else if low.starts_with(".include") {
            let path = line.split_once(char::is_whitespace).unwrap().1.trim();
            view.libraries.insert(vec![
                ".include".to_string(),
                path.trim_matches('"').to_string(),
            ]);
        } else if low.starts_with(".subckt") {
            let (head, tail) = split_line(line);
            let words: Vec<&str> = head.split_whitespace().collect();
            scope = words[1].to_string();
            let ports = words[2..].iter().map(|word| word.to_string()).collect();
            view.headers
                .insert(scope.clone(), (ports, view_params(&tail, false)));
        } else if low.starts_with(".ends") {
            scope = "__top__".to_string();
        } else if !line.is_empty() && !line.starts_with('*') && !line.starts_with('.') {
            let (head, tail) = split_line(line);
            let pieces: Vec<&str> = separators
                .split(head.trim_matches(|c| c == ')' || c == ' '))
                .collect();
            let mut name = pieces[0].to_string();
            let rest = &pieces[1..];
            let mut letters = name.chars();
            let first = letters.next().map(|c| c.to_ascii_lowercase());
            let second = letters.next().map(|c| c.to_ascii_lowercase());
            if golden && first.is_some() && first == second {
                name = name[1..].to_string(); // InSpice prepends the letter even when present
            }
            let letter = name.chars().next().unwrap().to_ascii_uppercase();
            let nodes = match node_count(letter) {
                Some(count) => count.min(rest.len()),
                None => rest.len().saturating_sub(1), // X: all but the master
            };
            let mut positional = Vec::new();
            for token in &rest[nodes..] {
                positional.push(view_value(token, false));
            }
            let element = (
                name,
                rest[..nodes].iter().map(|node| node.to_string()).collect(),
                positional,
                view_params(&tail, false),
            );
            view.elements
                .entry(scope.clone())
                .or_default()
                .insert(element);
        }
    }
    view
}

fn spectre_view(text: &str, golden: bool) -> NetlistView {
    static INCLUDE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static SUBCKT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static INSTANCE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let include = INCLUDE.get_or_init(|| Regex::new(r#"^include\s+"([^"]*)"(.*)"#).unwrap());
    let subckt = SUBCKT.get_or_init(|| Regex::new(r"^subckt\s+(\S+?)\s*\(([^)]*)\)(.*)").unwrap());
    let instance =
        INSTANCE.get_or_init(|| Regex::new(r"^(\S+?)\s*\(([^)]*)\)\s*(\S+)(.*)").unwrap());
    let mut view = NetlistView::default();
    let mut scope = "__top__".to_string();
    let mut control = false;
    for line in text.lines() {
        let line = line.trim();
        let word = line.split(' ').next().unwrap().split('(').next().unwrap();
        if word == "control" || word == "endc" {
            control = word == "control";
        } else if control || line.is_empty() || line.starts_with("//") {
            continue;
        } else if word == "include" {
            let captures = include.captures(line).unwrap();
            let mut library = vec![captures[1].to_string()];
            let mut options: Vec<String> = captures[2]
                .split_whitespace()
                .map(|option| option.to_string())
                .collect();
            options.sort();
            library.extend(options);
            view.libraries.insert(library);
        } else if word == "subckt" {
            let captures = subckt.captures(line).unwrap();
            scope = captures[1].to_string();
            let ports = captures[2]
                .split_whitespace()
                .map(|port| port.to_string())
                .collect();
            view.headers
                .insert(scope.clone(), (ports, view_params(&captures[3], true)));
        } else if word == "parameters" && scope != "__top__" {
            // VACASK subckt params, not on the header
            view.parameters
                .insert(scope.clone(), view_params(&line[10..], true));
        } else if word == "ends" {
            scope = "__top__".to_string();
        } else if !["ground", "load", "model", "parameters"].contains(&word) {
            let captures = instance.captures(line).unwrap();
            let name = if golden {
                captures[1][1..].to_lowercase() // InSpice: letter + name
            } else {
                captures[1].to_lowercase()
            };
            let element = (
                name,
                captures[2]
                    .split_whitespace()
                    .map(|node| node.to_string())
                    .collect(),
                vec![captures[3].to_lowercase()],
                view_params(&captures[4], true),
            );
            view.elements
                .entry(scope.clone())
                .or_default()
                .insert(element);
        }
    }
    view
}

#[test]
fn view_params_split_like_python() {
    assert_eq!(
        assignments("l={0.13 * 1e-6} m=1 w=2"),
        vec![
            ("l".to_string(), "{0.13 * 1e-6}".to_string()),
            ("m".to_string(), "1".to_string()),
            ("w".to_string(), "2".to_string()),
        ]
    );
    assert_eq!(
        split_line("XX1 vin vout vdd hier_cell PARAMS: rload=2k"),
        (
            "XX1 vin vout vdd hier_cell".to_string(),
            " rload=2k".to_string()
        )
    );
}

#[test]
fn export_matches_nyancad() {
    let mut failures = Vec::new();
    for golden in goldens() {
        let parts: Vec<&str> = golden.split('.').collect();
        let (stem, sim) = (parts[0], parts[1]);
        let golden_text = std::fs::read_to_string(fixtures().join("golden").join(&golden)).unwrap();
        let ours_text = writer(sim)(&export(stem, sim));
        let (ours, theirs) = if sim == "vacask" {
            (
                spectre_view(&ours_text, false),
                spectre_view(&golden_text, true),
            )
        } else {
            (
                spice_view(&ours_text, false),
                spice_view(&golden_text, true),
            )
        };
        if ours != theirs {
            failures.push(format!(
                "{golden}:\nours   {ours:?}\ntheirs {theirs:?}\n--- ours ---\n{ours_text}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn goldens_cover_every_fixture_nyancad_can_netlist() {
    let all: BTreeSet<String> = stems().into_iter().collect();
    let skipped_text = std::fs::read_to_string(fixtures().join("golden/SKIPPED.txt")).unwrap();
    let mut skipped = BTreeSet::new();
    for line in skipped_text.split('\n') {
        if !line.is_empty() {
            skipped.insert(line.split('.').next().unwrap().to_string());
        }
    }
    // nyancad: KeyError, pin S has no net
    assert_eq!(
        skipped,
        BTreeSet::from(["ihp_subckt_parent_nets".to_string()])
    );
    let mut golden_stems = BTreeSet::new();
    for golden in goldens() {
        golden_stems.insert(golden.split('.').next().unwrap().to_string());
    }
    let netlisted: BTreeSet<String> = all.difference(&skipped).cloned().collect();
    assert_eq!(golden_stems, netlisted);
    assert_eq!(goldens().len(), 3 * netlisted.len());
}

fn libraries(circuit: &Circuit) -> Vec<(DirectiveKind, String, String)> {
    let mut found = Vec::new();
    for directive in &circuit.directives {
        found.push((
            directive.kind,
            directive.value.clone(),
            directive.name.clone(),
        ));
    }
    found
}

fn library(kind: DirectiveKind, value: &str, name: &str) -> (DirectiveKind, String, String) {
    (kind, value.to_string(), name.to_string())
}

const LIB: DirectiveKind = DirectiveKind::Lib;
const INCLUDE: DirectiveKind = DirectiveKind::Include;

fn library_doc(models: &[&str]) -> Value {
    let mut doc = json!({});
    for model in models {
        doc[format!("top:{model}")] = json!({
            "type": "ckt", "name": model, "model": model, "nets": {"a": "x", "b": "y"}
        });
    }
    doc
}

fn library_models(sections: &[(&str, &[&str])]) -> Value {
    let mut models = json!({});
    for (name, library_sections) in sections {
        models[format!("models:{name}")] = json!({
            "name": name,
            "ports": [{"name": "a"}, {"name": "b"}],
            "models": [{
                "language": "spice",
                "name": name,
                "spice-type": "X",
                "library": format!("{name}.lib"),
                "sections": library_sections,
            }],
        });
    }
    models
}

fn corner_options(models: Value, corners: &[&str]) -> GschOptions {
    GschOptions {
        models: Some(models),
        corners: corners.iter().map(|corner| corner.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn corner_is_the_first_of_the_callers_list_the_library_has() {
    let models = library_models(&[("mos", &["tt", "ss", "ff"])]);
    let doc = library_doc(&["mos"]).to_string();
    let circuit = parse_gsch(&doc, &corner_options(models.clone(), &["xx", "ff", "ss"])).unwrap();
    assert_eq!(libraries(&circuit), [library(LIB, "mos.lib ff", "")]);
    let circuit = parse_gsch(&doc, &corner_options(models, &["ss"])).unwrap();
    assert_eq!(libraries(&circuit), [library(LIB, "mos.lib ss", "")]);
}

#[test]
fn corner_falls_back_to_the_first_section_then_to_an_include() {
    let models = library_models(&[("mos", &["tt", "ss"]), ("plain", &[])]);
    let doc = library_doc(&["mos", "plain"]).to_string();
    let circuit = parse_gsch(&doc, &corner_options(models, &["ff"])).unwrap();
    assert_eq!(
        libraries(&circuit),
        [
            library(LIB, "mos.lib tt", ""),
            library(INCLUDE, "plain.lib", "")
        ]
    );
    assert_eq!(
        spice_lines(&circuit)[1..3],
        [".lib mos.lib tt", ".include \"plain.lib\""]
    );
}

#[test]
fn each_model_family_takes_its_own_corner() {
    let models = library_models(&[
        ("mos", &["mos_tt", "mos_ff"]),
        ("res", &["res_typ", "res_wcs"]),
    ]);
    let doc = library_doc(&["mos", "res"]).to_string();
    let circuit = parse_gsch(&doc, &corner_options(models, &["mos_ff", "res_wcs"])).unwrap();
    assert_eq!(
        libraries(&circuit),
        [
            library(LIB, "mos.lib mos_ff", ""),
            library(LIB, "res.lib res_wcs", "")
        ]
    );
}

#[test]
fn shared_libraries_are_written_once() {
    let circuit = export("ihp_cmos_inverter", "ngspice"); // nmos and pmos: one library
    assert_eq!(
        libraries(&circuit),
        [library(
            LIB,
            "ihp/models/ngspice/models/cornerMOSlv.lib mos_tt",
            ""
        )]
    );
}

#[test]
fn vacask_takes_the_native_entry_then_a_foreign_library_with_its_dialect() {
    let circuit = export("ihp_cmos_inverter", "vacask");
    assert_eq!(
        libraries(&circuit),
        [
            library(LIB, "ihp/models/vacask/models/cornerMOSlv.lib mos_tt", ""),
            library(
                LIB,
                "ihp/models/ngspice/models/cornerMOSlv.lib mos_tt",
                "lang=ngspice"
            ),
        ]
    );
    assert_eq!(circuit.properties["dialect"], "vacask");
}

#[test]
fn ground_net_is_node_zero() {
    let circuit = export("ihp_rc_filter", "ngspice");
    assert_eq!(circuit.properties["ground_net"], "GND");
    let lines = spice_lines(&circuit);
    assert!(lines.contains(&"C1 P1 0 0.1u".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"V1 W3 0 DC 1 AC 1".to_string()),
        "{lines:?}"
    );
}

#[test]
fn child_schematics_become_subcircuits_and_hoist_their_libraries() {
    let circuit = export("ihp_hier_top", "ngspice");
    let module_names: Vec<&str> = circuit
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    assert_eq!(module_names, ["hier_cell", "__top__"]);
    let cell = &circuit.modules[0];
    let port_names: Vec<&str> = cell.ports.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(port_names, ["in", "out", "vdd"]); // sorted model ports
    assert_eq!(cell.parameters.len(), 1);
    let rload = &cell.parameters[0];
    assert_eq!(rload.name, "rload");
    assert!(rload
        .default_value
        .as_ref()
        .unwrap()
        .prefixed_value
        .is_some()); // a number
                     // top first, then the child's; the child's nmos shares the top pmos library
    assert_eq!(
        libraries(&circuit),
        [
            library(LIB, "ihp/models/ngspice/models/cornerMOSlv.lib mos_ff", ""),
            library(INCLUDE, "pdk/models/mprim.lib", ""),
        ]
    );
    for ext_module in &circuit.ext_modules {
        assert_ne!(ext_module.name, "hier_cell");
    }
    // vacask: only the child's nmos uses the native library, still at the top
    assert!(
        libraries(&export("ihp_hier_top", "vacask")).contains(&library(
            LIB,
            "ihp/models/vacask/models/cornerMOSlv.lib mos_ff",
            ""
        ))
    );
}

#[test]
fn devices_nyancad_does_not_netlist_stay_in_the_ir_but_are_not_written() {
    let circuit = export("ihp_hier_top", "ngspice");
    let led = reference(circuit.modules.last().unwrap(), "LED1");
    assert_eq!(led.properties["sim_skip"], "1");
    for line in spice_lines(&circuit) {
        let first = line.split_whitespace().next().unwrap_or("");
        assert!(first != "LED1" && first != "XLED1", "{line}");
    }
    let circuit = export("livewire_two_straights", "ngspice"); // no `nets`: nothing to netlist
    for reference in &circuit.modules[0].module_references {
        assert_eq!(
            reference.properties.get("sim_skip").map(String::as_str),
            Some("1")
        );
    }
}

#[test]
fn missing_mos_bulk_is_tied_to_ground() {
    let lines = spice_lines(&export("ihp_hier_top", "ngspice"));
    assert!(
        lines.contains(&"M4 vout2 vin 0 0 nch w=1u".to_string()),
        "{lines:?}"
    );
}

#[test]
fn models_keep_their_fqn_for_write_gsch() {
    let circuit = export("ihp_cmos_inverter", "ngspice");
    let m1 = reference(&circuit.modules[0], "M1");
    assert_eq!(
        (m1.module_name.as_str(), m1.properties.get("sim_skip")),
        ("sg13_lv_nmos", None)
    );
    let stash: Value = serde_json::from_str(&m1.properties["__info__"]).unwrap();
    assert_eq!(stash["__model__"], "ihp.cells.fet_transistors.nmos");
    let out = &write(&circuit)["demo2:M1"];
    let raw = &fixture_json("ihp_cmos_inverter")["demo2:M1"];
    assert_eq!(
        (
            out["type"].clone(),
            out["model"].clone(),
            out["props"].clone()
        ),
        (
            json!("nmos"),
            json!("ihp.cells.fet_transistors.nmos"),
            raw["props"].clone()
        )
    );
}

#[test]
fn top_circuit_is_written_flat_and_keeps_its_schema_name() {
    let circuit = export("ihp_rc_filter", "ngspice");
    assert_eq!(circuit.top_module, "__top__");
    assert_eq!(circuit.modules[0].name, "__top__");
    assert_eq!(info(&circuit.modules[0])["gsch_schema"], "demo");
    for line in spice_lines(&circuit) {
        assert!(!line.starts_with(".subckt"), "{line}");
    }
}

#[test]
fn read_gsch_finds_the_models_of_a_project() {
    let directory = temporary_directory("models");
    std::fs::create_dir_all(directory.join("build")).unwrap();
    std::fs::copy(
        fixtures().join("models.nyanlib"),
        directory.join("build/models.nyanlib"),
    )
    .unwrap();
    write_file(&directory.join("models.nyanlib"), "{}"); // build/ wins
    for name in ["ihp_hier_top.gsch", "ihp_hier_cell.gsch"] {
        std::fs::copy(fixtures().join(name), directory.join(name)).unwrap();
    }
    let top = path_text(&directory.join("ihp_hier_top.gsch"));
    let options = GschOptions {
        corners: vec!["mos_ff".to_string()],
        ..Default::default()
    };
    let circuit = read_gsch(&top, None, &options).unwrap();
    let module_names: Vec<&str> = circuit
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    assert_eq!(module_names, ["hier_cell", "__top__"]);
    assert!(circuit
        .directives
        .iter()
        .any(|directive| directive.value.ends_with("mos_ff")));
    // a child missing from models.nyanlib is named after its file
    write_file(&directory.join("build/models.nyanlib"), "{}");
    let circuit = read_gsch(&top, None, &GschOptions::default()).unwrap();
    let module_names: Vec<&str> = circuit
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    assert_eq!(module_names, ["ihp_hier_cell", "__top__"]);
    let port_names: Vec<&str> = circuit.modules[0]
        .ports
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(port_names, ["in", "out", "vdd"]);
}

/// kfnetlist's `test_read_gsch_stops_at_a_cycle` reads the cycle silently; here a schematic
/// that instantiates itself, directly or not, is an error (untrusted input).
#[test]
fn gsch_cycles_are_an_error() {
    let directory = temporary_directory("cycles");
    let direct = json!({
        "a:X": {"type": "ckt", "name": "X", "model": "a.gsch", "nets": {"p": "n"}},
        "a:p": {"type": "port", "name": "p", "nets": {"P": "n"}},
    });
    write_file(&directory.join("a.gsch"), &direct.to_string());
    let error = read_gsch(
        &path_text(&directory.join("a.gsch")),
        None,
        &GschOptions::default(),
    )
    .unwrap_err();
    assert!(validation_message(error).contains("a.gsch"));

    // b -> c -> ./b (spelled differently)
    let b = json!({"b:X": {"type": "ckt", "name": "X", "model": "sub/c.gsch", "nets": {"p": "n"}}});
    let c = json!({"c:X": {"type": "ckt", "name": "X", "model": "./b.gsch", "nets": {"p": "n"}}});
    write_file(&directory.join("b.gsch"), &b.to_string());
    write_file(&directory.join("sub/c.gsch"), &c.to_string());
    // `./b.gsch` resolves against the project root, which is the top's directory
    let error = read_gsch(
        &path_text(&directory.join("b.gsch")),
        None,
        &GschOptions::default(),
    )
    .unwrap_err();
    assert!(validation_message(error).contains("b.gsch"));
}

#[test]
fn gsch_depth_is_limited() {
    let directory = temporary_directory("depth");
    let levels = 70;
    for level in 0..levels {
        let mut doc = json!({"p": {"type": "port", "name": "p", "nets": {"P": "n"}}});
        if level + 1 < levels {
            doc["X"] = json!({
                "type": "ckt", "name": "X", "model": format!("level{}.gsch", level + 1),
                "nets": {"p": "n"}
            });
        }
        write_file(
            &directory.join(format!("level{level}.gsch")),
            &doc.to_string(),
        );
    }
    let top = path_text(&directory.join("level0.gsch"));
    let error = read_gsch(&top, None, &GschOptions::default()).unwrap_err();
    assert!(validation_message(error).contains("64"));
    // 64 levels below the top still read
    let shallow = path_text(&directory.join(format!("level{}.gsch", levels - 65)));
    let circuit = read_gsch(&shallow, None, &GschOptions::default()).unwrap();
    assert_eq!(circuit.modules.len(), 65);
}

#[test]
fn read_gsch_rejects_a_missing_child() {
    let directory = temporary_directory("missing");
    let doc =
        json!({"a:X": {"type": "ckt", "name": "X", "model": "gone.gsch", "nets": {"p": "n"}}});
    write_file(&directory.join("a.gsch"), &doc.to_string());
    let error = read_gsch(
        &path_text(&directory.join("a.gsch")),
        None,
        &GschOptions::default(),
    )
    .unwrap_err();
    assert!(validation_message(error).contains("gone.gsch"));
}

#[test]
fn read_gsch_rejects_two_children_with_one_circuit_name() {
    let directory = temporary_directory("duplicate");
    let child = json!({"c:p": {"type": "port", "name": "p", "nets": {"P": "n"}}});
    for name in ["a/cell.gsch", "b/cell.gsch"] {
        write_file(&directory.join(name), &child.to_string());
    }
    let mut doc = json!({});
    for (index, model) in ["a/cell.gsch", "b/cell.gsch"].iter().enumerate() {
        doc[format!("t:X{index}")] = json!({
            "type": "ckt", "name": format!("X{index}"), "model": model, "nets": {"p": "n"}
        });
    }
    write_file(&directory.join("top.gsch"), &doc.to_string());
    let error = read_gsch(
        &path_text(&directory.join("top.gsch")),
        None,
        &GschOptions::default(),
    )
    .unwrap_err();
    assert!(validation_message(error).contains("b/cell.gsch"));
}

#[test]
fn read_gsch_resolves_children_against_the_project_root() {
    let directory = temporary_directory("root");
    let child = json!({"c:p": {"type": "port", "name": "p", "nets": {"P": "n"}}});
    write_file(&directory.join("cells/cell.gsch"), &child.to_string());
    let top = json!({"t:X": {"type": "ckt", "name": "X", "model": "cells/cell.gsch", "nets": {"p": "n"}}});
    write_file(&directory.join("top/top.gsch"), &top.to_string());
    let root = path_text(&directory);
    let circuit = read_gsch(
        &path_text(&directory.join("top/top.gsch")),
        Some(&root),
        &GschOptions::default(),
    )
    .unwrap();
    let module_names: Vec<&str> = circuit
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    assert_eq!(module_names, ["cell", "__top__"]);
}

#[test]
fn unknown_simulator_is_rejected() {
    for sim in ["spectre", "", "hspice"] {
        let error = parse_gsch("{}", &sim_options(sim)).unwrap_err();
        assert!(validation_message(error).contains("sim"), "{sim}");
    }
}

#[test]
fn simulator_names_are_case_insensitive() {
    let text = fixture_text("ihp_rc_filter");
    assert_eq!(
        write_spectre(&parse_gsch(&text, &sim_options("VACASK")).unwrap()),
        write_spectre(&parse_gsch(&text, &sim_options("vacask")).unwrap())
    );
}

#[test]
fn ngspice_runs_the_rc_filter() {
    let found = std::process::Command::new("ngspice")
        .arg("--version")
        .output();
    if found.is_err() {
        return; // ngspice not installed
    }
    let directory = temporary_directory("ngspice");
    let deck = directory.join("rc.cir");
    let text = write_spice(&export("ihp_rc_filter", "ngspice"));
    write_file(&deck, &text.replace(".end", ".op\n.end"));
    let run = std::process::Command::new("ngspice")
        .arg("-b")
        .arg(&deck)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!stdout.to_lowercase().contains("error"), "{stdout}");
}

#[test]
fn vacask_multiplier_is_mfactor_and_left_out_when_one() {
    let nets = json!({"D": "d", "G": "g", "S": "s", "B": "b"});
    let mut doc = json!({});
    for (name, multiplier) in [("M1", 2), ("M2", 1)] {
        doc[format!("t:{name}")] = json!({
            "type": "nmos", "name": name, "nets": nets, "props": {"model": "nch", "m": multiplier}
        });
    }
    let text = write_spectre(&parse_gsch(&doc.to_string(), &sim_options("vacask")).unwrap());
    let lines = lines_of(&text);
    assert!(
        lines.contains(&"m1 (d g s b) nch $mfactor=2".to_string()),
        "{text}"
    );
    assert!(lines.contains(&"m2 (d g s b) nch".to_string()), "{text}");
    assert!(write_spice(&parse_doc(&doc)).contains("m=2"));
}

#[test]
fn an_edited_value_goes_back_to_its_mosaic_prop() {
    let mut circuit = export("ihp_rc_filter", "ngspice");
    let r1 = reference_mut(&mut circuit.modules[0], "R1");
    r1.parameters = vec![parameter("value", &json!("2k"))];
    assert_eq!(
        write(&circuit)["demo:R1"]["props"],
        json!({"resistance": "2k"})
    );
}

#[test]
fn an_unwired_device_does_not_add_pins_to_a_simulated_master() {
    let doc = json!({
        "t:R1": {"type": "resistor", "name": "R1", "nets": {"P": "a", "N": "b"},
                 "props": {"resistance": "1k"}},
        "t:R2": {"type": "resistor", "name": "R2", "nets": {"P": "a", "N": "b"}},
        "t:R3": {"type": "resistor", "name": "R3", "ports": [{"name": "X"}]},
    });
    let circuit = parse_doc(&doc);
    let port_names: Vec<&str> = circuit.ext_modules[0]
        .ports
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(port_names, ["p", "n"]);
    assert!(spice_lines(&circuit).contains(&"R1 a b 1k".to_string()));
}

// ── open pins, source fields (fix round 1) ─────────────────────────────────────

#[test]
fn open_pins_keep_their_own_named_nodes() {
    let doc = json!({
        "t:R1": {"type": "resistor", "name": "R1", "nets": {"P": "a", "N": "net7"},
                 "props": {"resistance": "1k"}},
        "t:R2": {"type": "resistor", "name": "R2", "nets": {"P": "a", "N": "net8"},
                 "props": {"resistance": "1k"}},
    });
    let lines = spice_lines(&parse_doc(&doc));
    assert!(lines.contains(&"R1 a net7 1k".to_string()), "{lines:?}");
    assert!(lines.contains(&"R2 a net8 1k".to_string()), "{lines:?}");
    for line in &lines {
        assert!(!line.contains('?'), "{line}");
    }
}

#[test]
fn a_lone_tied_bulk_is_node_zero() {
    let doc = json!({
        "t:M1": {"type": "nmos", "name": "M1",
                 "nets": {"D": "d", "G": "g", "S": "s"}, "props": {"model": "nch"}},
        "t:R1": {"type": "resistor", "name": "R1", "nets": {"P": "d", "N": "g"}},
    });
    assert!(spice_lines(&parse_doc(&doc)).contains(&"M1 d g s 0 nch".to_string()));
}

fn pulse(tran: Value) -> Value {
    let mut waveform = json!({"type": "pulse"});
    for (key, value) in tran.as_object().unwrap() {
        waveform[key] = value.clone();
    }
    json!({
        "t:V1": {"type": "vsource", "name": "V1", "nets": {"P": "a", "N": "GND"},
                 "props": {"tran": waveform}},
        "t:R1": {"type": "resistor", "name": "R1", "nets": {"P": "a", "N": "GND"}},
    })
}

#[test]
fn an_expression_keeps_its_slot_in_a_waveform() {
    let doc = pulse(json!({"initial": 0, "pulsed": "{vdd}", "width": "5n", "period": "10n"}));
    let lines = spice_lines(&parse_doc(&doc));
    assert!(
        lines.contains(&"V1 a 0 DC 0 PULSE(0 {vdd} 0 0 0 5n 10n)".to_string()),
        "{lines:?}"
    );
    let text = write_spectre(&parse_gsch(&doc.to_string(), &sim_options("vacask")).unwrap());
    // `{}` is SPICE wrapping: VACASK writes the expression bare
    assert!(
        text.contains(
            "v1 (a 0) vsource dc=0 type=\"pulse\" val0=0 val1=vdd fall=0 width=5n period=10n"
        ),
        "{text}"
    );
}

#[test]
fn an_unparsed_derived_expression_keeps_its_ngspice_braces() {
    let doc = json!({
        "t:M1": {"type": "nmos", "name": "M1", "model": "nch", "props": {"W": "1u"},
                 "nets": {"D": "d", "G": "g", "S": "GND", "B": "GND"}},
    });
    let mut variants = Vec::new();
    for language in ["spice", "spectre"] {
        // `^` is not parsed: the derived `w` stays raw
        variants.push(json!({
            "language": language, "name": "nch", "spice-type": "M", "params": {"w": "W^2"}
        }));
    }
    let models = json!({
        "models:nch": {
            "name": "nch",
            "ports": [{"name": "D"}, {"name": "G"}, {"name": "S"}, {"name": "B"}],
            "models": variants,
        }
    });
    for (sim, want) in [("ngspice", "w={1u^2}"), ("vacask", "w=1u^2")] {
        let options = GschOptions {
            models: Some(models.clone()),
            sim: sim.to_string(),
            ..Default::default()
        };
        let circuit = parse_gsch(&doc.to_string(), &options).unwrap();
        let text = writer(sim)(&circuit);
        assert!(text.contains(want), "{sim}: {text}");
    }
}

#[test]
fn a_positional_raw_value_is_not_wrapped() {
    let doc = json!({"t:V1": {"type": "vsource", "name": "V1", "nets": {"P": "a", "N": "GND"},
                              "props": {"dc": "0.9", "ac": "1"}}});
    assert!(spice_lines(&parse_doc(&doc)).contains(&"V1 a 0 DC 0.9 AC 1".to_string()));
}

#[test]
fn a_missing_required_waveform_field_is_an_error() {
    let doc = pulse(json!({"initial": 0, "width": "5n", "period": "10n"}));
    let error = parse_gsch(&doc.to_string(), &GschOptions::default()).unwrap_err();
    let message = validation_message(error);
    assert!(
        Regex::new("V1.*pulsed").unwrap().is_match(&message),
        "{message}"
    );
}

// ── parse -> write_gsch is lossless (final fix wave) ───────────────────────────

#[test]
fn an_unedited_schematic_is_written_back_unchanged() {
    for sim in ["ngspice", "xyce", "vacask"] {
        for stem in stems() {
            let text = fixture_text(&stem);
            let options = GschOptions {
                models: Some(models()),
                sim: sim.to_string(),
                corners: corners_of(&stem),
                name: None,
            };
            let circuit = parse_gsch(&text, &options).unwrap();
            let out: Value = serde_json::from_str(&write_gsch(&circuit).unwrap()).unwrap();
            assert_eq!(out, fixture_json(&stem), "{stem} {sim}");
        }
    }
}

#[test]
fn a_model_backed_primitive_keeps_the_pins_of_its_entry() {
    let doc = json!({
        "t:V1": {"type": "vsource", "name": "V1", "nets": {"P": "in", "N": "GND"},
                 "props": {"dc": "1"}},
        "t:R1": {"type": "ckt", "model": "pdk.rpoly", "name": "R1",
                 "nets": {"A": "in", "B": "out"}, "props": {"width": 2}},
    });
    let mut circuit = parse_gsch(&doc.to_string(), &model_options(models())).unwrap();
    assert!(spice_lines(&circuit).contains(&"R1 in out w=2".to_string())); // SPICE R pins p/n
    assert_eq!(write(&circuit), doc);
    for connection in &mut circuit.modules[0].connections {
        // an edit still lands on the entry's pins
        if connection.name == "out" {
            connection.name = "in".to_string();
        }
    }
    assert_eq!(
        write(&circuit)["t:R1"]["nets"],
        json!({"A": "in", "B": "in"})
    );
}

#[test]
fn a_tied_bulk_is_written_only_once_moved_off_ground() {
    let doc = json!({
        "t:M1": {"type": "nmos", "name": "M1",
                 "nets": {"D": "d", "G": "g", "S": "s"}, "props": {"model": "nch"}},
        "t:R1": {"type": "resistor", "name": "R1", "nets": {"P": "d", "N": "s"}},
    });
    let mut circuit = parse_doc(&doc);
    assert_eq!(write(&circuit), doc);
    for connection in &mut circuit.modules[0].connections {
        if connection.name == "GND" {
            // the bulk alone: move it to `s`
            connection.name = "s".to_string();
        }
    }
    assert_eq!(
        write(&circuit)["t:M1"]["nets"],
        json!({"D": "d", "G": "g", "S": "s", "B": "s"})
    );
}

// ── source props and model-derived params (#4) ─────────────────────────────────

fn inverter() -> Circuit {
    parse_gsch(&fixture_text("ihp_cmos_inverter"), &model_options(models())).unwrap()
}

fn line_starting(circuit: &Circuit, prefix: &str) -> String {
    let mut found = Vec::new();
    for line in spice_lines(circuit) {
        if line.starts_with(prefix) {
            found.push(line);
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

#[test]
fn model_params_are_expressions_over_the_source_props() {
    let circuit = inverter();
    let m1 = reference(&circuit.modules[0], "M1");
    let mut sources = BTreeSet::new();
    for parameter in &m1.parameters {
        if parameter.properties.get("role").map(String::as_str) == Some("source") {
            sources.insert(parameter.name.as_str());
        }
    }
    // `m = m`: one plain param
    assert_eq!(sources, BTreeSet::from(["width", "length", "nf", "model"]));
    assert_eq!(
        param_value(m1, "w").expression,
        Some(parse_expression("width * 1e-6").unwrap())
    );
    assert_eq!(
        param_value(m1, "ng").expression,
        Some(Expression::reference("nf"))
    );
    assert_eq!(
        Some(param_value(m1, "width").clone()),
        parameter("width", &json!(0.15)).default_value
    );
    for parameter in &m1.parameters {
        if parameter.name == "m" {
            assert!(!parameter.properties.contains_key("role"));
        }
    }
}

#[test]
fn an_edited_source_prop_reaches_the_gsch_and_the_simulation() {
    let raw = &fixture_json("ihp_cmos_inverter")["demo2:M1"];
    let mut back = inverter();
    edit(&mut back, "M1", "width", json!(0.3));
    let mut expected = raw["props"].clone();
    expected["width"] = json!(0.3);
    assert_eq!(write(&back)["demo2:M1"]["props"], expected);
    let m1 = line_starting(&back, "XM1 ");
    assert!(m1.ends_with(" w={0.3 * 1e-6}"), "{m1}");
    assert!(!m1.contains("width=0.3")); // a source prop is not a simulation param
}

#[test]
fn an_edited_derived_param_is_an_error_not_a_silent_drop() {
    let mut back = inverter();
    edit(&mut back, "M1", "w", json!("0.2 * 1e-6"));
    let error = write_gsch(&back).unwrap_err();
    assert_eq!(
        validation_message(error),
        "M1: 'w' is derived from model expression 'width * 1e-6'; edit 'width' instead"
    );
}

#[test]
fn an_edited_model_default_is_written_and_a_computed_param_is_an_error() {
    let text = fixture_text("ihp_hier_top");
    let mut circuit = parse_gsch(&text, &model_options(models())).unwrap();
    let module = &mut circuit.modules[0];
    // X2's `rload` is the model's default, not in the entry's props
    let x2 = reference_mut(module, "X2");
    let x2_params: Vec<&str> = x2
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    assert_eq!(x2_params, ["rload"]);
    x2.parameters = vec![parameter("rload", &json!("2k"))];
    assert_eq!(write(&circuit)["top:X2"]["props"]["rload"], "2k");
    let m3 = reference_mut(&mut circuit.modules[0], "M3");
    for existing in &mut m3.parameters {
        if existing.name == "w" {
            *existing = parameter("w", &json!("9u"));
        }
    }
    let error = write_gsch(&circuit).unwrap_err();
    assert!(validation_message(error).contains("M3: 'w' is derived from"));
}

fn colliding_models(params: &[(&str, &str)]) -> Value {
    let mut models = models();
    for variant in models["models:ihp.cells.fet_transistors.nmos"]["models"]
        .as_array_mut()
        .unwrap()
    {
        for (name, expression) in params {
            variant["params"][*name] = json!(expression);
        }
    }
    models
}

#[test]
fn a_model_param_named_like_its_prop_survives_the_bridge() {
    let models = colliding_models(&[("w", "w * 2")]);
    let mut doc = fixture_json("ihp_cmos_inverter");
    doc["demo2:M1"]["props"]["w"] = json!(0.5);
    let circuit = parse_gsch(&doc.to_string(), &model_options(models)).unwrap();
    let m1 = line_starting(&circuit, "XM1 ");
    assert!(m1.ends_with(" w={0.5 * 2}"), "{m1}");
    // the bridge round trip (`from_netlists(to_netlists(nl))`) is not ported
    assert_eq!(write(&circuit), doc);
    let mut edited = circuit.clone();
    edit(&mut edited, "M1", "w", json!("3"));
    let error = write_gsch(&edited).unwrap_err();
    assert_eq!(
        validation_message(error),
        "M1: 'w' is derived from model expression 'w * 2'; edit 'w' instead"
    );
}

// ── inline nyanlib `code` blocks are macros (#7) ───────────────────────────────

fn code_models(codes: &[(&str, &str)]) -> Value {
    let mut models = json!({});
    for (name, code) in codes {
        models[format!("models:{name}")] = json!({
            "name": name,
            "ports": [{"name": "a"}, {"name": "b"}],
            "models": [{
                "language": "spice", "name": name, "spice-type": "SUBCKT",
                "code": format!("\n{code}\n"),
            }],
        });
    }
    models
}

#[test]
fn inline_code_is_a_macro_deduplicated_across_the_hierarchy() {
    let directory = temporary_directory("macros");
    let mut models = code_models(&[
        ("rdiv", ".subckt rdiv a b\n.ends rdiv"),
        ("rtop", ".param r=1"),
    ]);
    models["models:cell.gsch"] = json!({"name": "cell", "ports": [{"name": "p"}]});
    let child = json!({
        "c:X1": {"type": "ckt", "name": "X1", "model": "rdiv", "nets": {"a": "p", "b": "GND"}},
        "c:p": {"type": "port", "name": "p", "nets": {"P": "p"}},
    });
    let mut top = library_doc(&["rtop", "rdiv"]);
    top["top:C"] = json!({"type": "ckt", "name": "C", "model": "cell.gsch", "nets": {"p": "x"}});
    write_file(&directory.join("cell.gsch"), &child.to_string());
    write_file(&directory.join("top.gsch"), &top.to_string());
    let circuit = read_gsch(
        &path_text(&directory.join("top.gsch")),
        None,
        &model_options(models),
    )
    .unwrap();
    let mut macros = Vec::new();
    for code in &circuit.macros {
        macros.push((
            code.name.as_str(),
            code.language.as_str(),
            code.body.as_str(),
        ));
    }
    assert_eq!(
        macros,
        [
            ("rtop", "spice", ".param r=1"), // stripped, top first, rdiv once
            ("rdiv", "spice", ".subckt rdiv a b\n.ends rdiv"),
        ]
    );
    assert!(spice_lines(&circuit).contains(&".param r=1".to_string()));
}

#[test]
fn vacask_skips_the_spectre_code_nyancad_drops() {
    let text = write_spectre(&export("demo_inline_code", "vacask"));
    assert!(
        lines_of(&text).contains(&"// skipped macro rdiv (spectre)".to_string()),
        "{text}"
    );
}

#[test]
fn an_expression_reading_a_colliding_prop_gets_its_value() {
    let models = colliding_models(&[("w", "w * 2"), ("ad", "w * 0.5")]);
    let mut doc = fixture_json("ihp_cmos_inverter");
    doc["demo2:M1"]["props"]["w"] = json!(0.5);
    let circuit = parse_gsch(&doc.to_string(), &model_options(models)).unwrap();
    let m1 = line_starting(&circuit, "XM1 ");
    assert!(format!("{m1} ").contains(" w={0.5 * 2} "), "{m1}");
    assert!(format!(" {m1}").contains(" ad={0.5 * 0.5}"), "{m1}");
    assert_eq!(write(&circuit), doc);
    let mut edited = circuit.clone();
    edit(&mut edited, "M1", "ad", json!("3"));
    let error = write_gsch(&edited).unwrap_err();
    assert_eq!(
        validation_message(error),
        "M1: 'ad' is derived from model expression 'w * 0.5'; edit 'w' instead"
    );
}

#[test]
fn same_named_code_with_different_bodies_is_all_kept() {
    // kfnetlist also warns ("macro 'shared'"); oxymorph has no warnings channel
    let mut models = code_models(&[("ra", ".param ra=1"), ("rb", ".param rb=2")]);
    for model in models.as_object_mut().unwrap().values_mut() {
        model["models"][0]["name"] = json!("shared");
    }
    let doc = library_doc(&["ra", "rb"]).to_string();
    let lines = spice_lines(&parse_gsch(&doc, &model_options(models)).unwrap());
    assert!(
        lines.contains(&".param ra=1".to_string()) && lines.contains(&".param rb=2".to_string())
    );
}

#[test]
fn nameless_code_entries_are_not_collapsed() {
    let mut models = code_models(&[("ra", ".param ra=1"), ("rb", ".param rb=2")]);
    for model in models.as_object_mut().unwrap().values_mut() {
        model["models"][0].as_object_mut().unwrap().remove("name");
    }
    let doc = library_doc(&["ra", "rb"]).to_string();
    let circuit = parse_gsch(&doc, &model_options(models)).unwrap();
    let bodies: Vec<&str> = circuit
        .macros
        .iter()
        .map(|code| code.body.as_str())
        .collect();
    assert_eq!(bodies, [".param ra=1", ".param rb=2"]);
}

#[test]
fn code_without_a_language_is_spice() {
    let mut models = code_models(&[("ra", ".param ra=1")]);
    let entry = models["models:ra"]["models"][0].as_object_mut().unwrap();
    entry.remove("language");
    entry.insert("implementation".to_string(), json!("ngspice"));
    let circuit = parse_gsch(&library_doc(&["ra"]).to_string(), &model_options(models)).unwrap();
    let languages: Vec<&str> = circuit
        .macros
        .iter()
        .map(|code| code.language.as_str())
        .collect();
    assert_eq!(languages, ["spice"]);
    assert!(spice_lines(&circuit).contains(&".param ra=1".to_string()));
}

#[test]
fn sources_are_stimuli_of_the_simulation() {
    let circuit = export("ihp_hier_top", "ngspice");
    let mut names = BTreeSet::new();
    for stimulus in &circuit.simulation.as_ref().unwrap().stimuli {
        names.insert(stimulus.name.as_str());
    }
    assert_eq!(
        names,
        BTreeSet::from(["__top__/V1", "__top__/V2", "__top__/I1"])
    );
    let v1 = reference(circuit.modules.last().unwrap(), "V1");
    assert_eq!(v1.properties["stimulus"], "__top__/V1");
    assert!(v1.parameters.is_empty());
    let pulse = stimulus_named(&circuit, "__top__/V1")
        .waveform
        .as_ref()
        .unwrap();
    assert_eq!(pulse.kind, WaveformKind::Pulse);
    assert_eq!(pulse.args["val1"], parameter_value("1.8"));
    let sine = stimulus_named(&circuit, "__top__/I1");
    assert_eq!(sine.ac_magnitude, Some(parameter_value("1")));
    assert_eq!(
        sine.waveform.as_ref().unwrap().args["sinedc"],
        parameter_value("1m")
    );
}

#[test]
fn the_simulator_does_not_change_a_stimulus() {
    let ngspice = export("ihp_hier_top", "ngspice").simulation;
    assert_eq!(ngspice, export("ihp_hier_top", "vacask").simulation);
}

#[test]
fn an_edited_stimulus_goes_back_to_its_props() {
    let mut circuit = export("ihp_hier_top", "ngspice");
    stimulus_named_mut(&mut circuit, "__top__/V2").dc = Some(parameter_value("2.5"));
    assert_eq!(write(&circuit)["top:V2"]["props"], json!({"dc": "2.5"}));
    assert!(spice_lines(&circuit).contains(&"V2 vdd 0 DC 2.5".to_string()));
    let i1 = stimulus_named_mut(&mut circuit, "__top__/I1");
    i1.waveform
        .as_mut()
        .unwrap()
        .args
        .insert("ampl".to_string(), parameter_value("2m"));
    assert_eq!(
        write(&circuit)["top:I1"]["props"]["tran"],
        json!({"type": "sin", "offset": "1m", "amplitude": "2m", "frequency": "1meg"})
    );
}

#[test]
fn a_source_in_a_child_schematic_is_named_after_its_circuit() {
    let directory = temporary_directory("child-source");
    let child = json!({
        "c:V1": {"type": "vsource", "name": "V1", "nets": {"P": "p", "N": "GND"},
                 "props": {"dc": "1"}},
        "c:p": {"type": "port", "name": "p", "nets": {"P": "p"}},
    });
    let top = json!({
        "t:X": {"type": "ckt", "name": "X", "model": "cell.gsch", "nets": {"p": "n"}},
        "t:R": {"type": "resistor", "name": "R", "nets": {"P": "n", "N": "GND"},
                "props": {"resistance": "1k"}},
    });
    write_file(&directory.join("cell.gsch"), &child.to_string());
    write_file(&directory.join("top.gsch"), &top.to_string());
    let circuit = read_gsch(
        &path_text(&directory.join("top.gsch")),
        None,
        &GschOptions::default(),
    )
    .unwrap();
    let names: Vec<&str> = circuit
        .simulation
        .as_ref()
        .unwrap()
        .stimuli
        .iter()
        .map(|stimulus| stimulus.name.as_str())
        .collect();
    assert_eq!(names, ["cell/V1"]);
    let v1 = &circuit.modules[0].module_references[0];
    assert_eq!(
        (
            circuit.modules[0].name.as_str(),
            v1.properties["stimulus"].as_str()
        ),
        ("cell", "cell/V1")
    );
    assert!(write_spice(&circuit).contains("V1 p 0 DC 1"));
}

#[test]
fn an_open_pin_is_its_own_named_node() {
    let circuit = export("ihp_subckt_parent_nets", "ngspice");
    let writers: [(Write, (&str, &str)); 3] = [
        (write_spice, ("nc_XA_S", "nc_XB_D")),
        (write_xyce, ("nc_XA_S", "nc_XB_D")),
        (write_spectre, ("nc_A_S", "nc_B_D")),
    ];
    for (write_format, (first, second)) in writers {
        let text = write_format(&circuit);
        // named after the instance as written
        assert!(text.contains(first) && text.contains(second), "{text}");
        assert!(!text.contains('?'), "{text}");
    }
}

// ===========================================================================
// test_review_fixes.py (gsch), test_vams_writer.py (TestNativeSimulationForms gsch case)
// ===========================================================================

#[test]
fn write_gsch_keeps_the_sources_of_a_spice_netlist() {
    let circuit = parse_spice("* t\nV1 a 0 DC 0.5\nI1 a 0 1m\nR1 a 0 1k\n.end\n").unwrap();
    let doc = write(&circuit);
    let mut props = BTreeMap::new();
    for entry in doc.as_object().unwrap().values() {
        if let Some(name) = entry.get("name").and_then(Value::as_str) {
            props.insert(name.to_string(), entry.get("props").cloned());
        }
    }
    assert_eq!(props["V1"], Some(json!({"dc": "0.5"})));
    assert_eq!(props["I1"], Some(json!({"dc": "1m"})));
}

#[test]
fn gsch_source_props_refuse_an_ac_phase() {
    let stimulus = Stimulus {
        ac_magnitude: Some(parameter_value("1")),
        ac_phase: Some(parameter_value("90")),
        ..Default::default()
    };
    let error = source_props("V1", &stimulus, &serde_json::Map::new()).unwrap_err();
    assert!(validation_message(error).contains("phase"));
}

#[test]
fn a_gsch_schematic_is_written_with_native_verilog_ams() {
    let options = GschOptions {
        corners: vec!["mos_ff".to_string()],
        ..Default::default()
    };
    let circuit = read_gsch(&fixture_path("ihp_hier_top"), None, &options).unwrap();
    let text = write_vams(&circuit);
    assert!(text.contains("Mprim #("), "{text}"); // a `.model` name is the module (E.2.2.1)
    assert!(text.contains("paramset Mprim mosfet;"), "{text}"); // ... of a primitive (7.5)
    assert!(text.contains("vsine #(.dc(1.8)) V2"), "{text}"); // a DC source (E.2.2.3)
    assert!(
        text.contains("vpulse #(.dc(0.9), .val0(0), .val1(1.8), .td(1n)"),
        "{text}"
    );
    assert!(
        text.contains("(* spice_lib = \"ihp/models/ngspice/models/cornerMOSlv.lib mos_ff"),
        "{text}"
    );
    assert!(text.contains("ground GND;"), "{text}");
    let back = parse_vams(&text).unwrap();
    let mut directives = BTreeSet::new();
    for directive in &back.directives {
        directives.insert((format!("{:?}", directive.kind), directive.value.clone()));
    }
    assert!(directives.contains(&(
        "Lib".to_string(),
        "ihp/models/ngspice/models/cornerMOSlv.lib mos_ff".to_string()
    )));
    assert!(directives.contains(&("Include".to_string(), "pdk/models/mprim.lib".to_string())));
    assert_eq!(back.simulation.as_ref().unwrap().stimuli.len(), 3);
}

// ===========================================================================
// test_gsch_conversions.py
// ===========================================================================

type Parse = fn(&str) -> Result<Circuit, Error>;
type Write = fn(&Circuit) -> String;

fn formats() -> Vec<(&'static str, Write, Parse)> {
    vec![
        ("ngspice", write_spice, parse_spice),
        ("hspice", write_hspice, parse_hspice),
        ("xyce", write_xyce, parse_xyce),
        ("spectre", write_spectre, parse_spectre),
        ("spectre_spice", write_spectre_spice, parse_spectre_spice),
        ("verilog-a", write_vams, parse_vams),
    ]
}

/// The number an arithmetic expression comes to, None if it reads anything else.
fn evaluate(expression: Option<&Expression>, resolve: Option<&SourceParams>) -> Option<f64> {
    let expression = expression?;
    if let Some(literal) = &expression.literal {
        return Some(oxymorph_netlist::expression::to_float(literal));
    }
    if let Some(name) = &expression.reference {
        let target = resolve?.resolve(name)?;
        return evaluate(Some(&target), None);
    }
    if let Some(unary) = &expression.unary {
        if unary.op == Operator::Neg {
            return evaluate(unary.operand.as_deref(), resolve).map(|operand| -operand);
        }
        return None;
    }
    if let Some(binary) = &expression.binary {
        let lhs = evaluate(binary.lhs.as_deref(), resolve)?;
        let rhs = evaluate(binary.rhs.as_deref(), resolve)?;
        return match binary.op {
            Operator::Add => Some(lhs + rhs),
            Operator::Sub => Some(lhs - rhs),
            Operator::Mul => Some(lhs * rhs),
            Operator::Div => Some(lhs / rhs),
            Operator::Pow => Some(lhs.powf(rhs)),
            _ => None,
        };
    }
    None
}

/// `float(f"{x:.9g}")`.
fn nine_digits(value: f64) -> Value {
    json!(format!("{value:.8e}").parse::<f64>().unwrap())
}

fn number_or_text(value: Option<&ParameterValue>, resolve: Option<&SourceParams>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    if let Some(prefixed) = &value.prefixed_value {
        return nine_digits(oxymorph_netlist::expression::to_float(prefixed));
    }
    if let Some(expression) = &value.expression {
        if let Some(number) = evaluate(Some(expression), resolve) {
            return nine_digits(number);
        }
        let lookup = |name: &str| resolve.and_then(|source| source.resolve(name));
        let resolver: Option<Resolve> = if resolve.is_some() {
            Some(&lookup)
        } else {
            None
        };
        let text = format_expression(expression, Dialect::Spectre, resolver, false);
        let stripped = text.trim_matches(|c| matches!(c, '{' | '}' | '\'' | '"'));
        let squeezed: String = stripped.split_whitespace().collect();
        return json!(squeezed.to_lowercase());
    }
    json!(format!("{value:?}"))
}

fn view_net(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower == "gnd" || lower == "0" {
        "0".to_string()
    } else {
        lower
    }
}

fn element_nets(module: &Module) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ((instance, _), net) in connections_to_instance_nets(&module.connections, "") {
        if !instance.is_empty() {
            found
                .entry(instance.to_lowercase())
                .or_default()
                .push(view_net(&net));
        }
    }
    found
}

fn stimulus_view(stimulus: Option<&Stimulus>) -> Value {
    let Some(stimulus) = stimulus else {
        return Value::Null;
    };
    let waveform = match &stimulus.waveform {
        None => Value::Null,
        Some(waveform) => {
            let mut args = serde_json::Map::new();
            for (name, value) in &waveform.args {
                args.insert(name.clone(), number_or_text(Some(value), None));
            }
            let mut points = Vec::new();
            for point in &waveform.points {
                points.push(number_or_text(Some(point), None));
            }
            json!({
                "kind": format!("{:?}", waveform.kind).to_uppercase(),
                "args": args,
                "points": points,
            })
        }
    };
    json!({
        "dc": number_or_text(stimulus.dc.as_ref(), None),
        "mag": number_or_text(stimulus.ac_magnitude.as_ref(), None),
        "phase": number_or_text(stimulus.ac_phase.as_ref(), None),
        "waveform": waveform,
    })
}

fn is_empty_view(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

/// Everything a simulator needs from `circuit`, spelled format-neutrally.
fn simulation_view(circuit: &Circuit, flat_top: bool) -> Value {
    let mut libraries = serde_json::Map::new();
    let mut directives: Vec<_> = circuit.directives.iter().collect();
    for module in &circuit.modules {
        directives.extend(module.directives.iter());
    }
    for directive in directives {
        if !matches!(directive.kind, DirectiveKind::Lib | DirectiveKind::Include) {
            continue;
        }
        let mut parts = vec![format!("{:?}", directive.kind).to_uppercase()];
        let cleaned = directive.value.replace('"', "").replace("section=", "");
        for part in cleaned.split_whitespace() {
            parts.push(part.to_string());
        }
        libraries.insert(parts.join(" "), Value::Bool(true));
    }
    let mut circuits = serde_json::Map::new();
    for module in &circuit.modules {
        let top = module.name == circuit.top_module;
        let nets = element_nets(module);
        let mut elements = serde_json::Map::new();
        for reference in &module.module_references {
            if is_skipped(reference) {
                continue;
            }
            let (params, resolve) = sim_params(reference);
            let mut param_view = serde_json::Map::new();
            for parameter in params {
                param_view.insert(
                    parameter.name.to_lowercase(),
                    number_or_text(parameter.default_value.as_ref(), Some(&resolve)),
                );
            }
            let master = reference
                .properties
                .get("model_name")
                .filter(|model| !model.is_empty())
                .unwrap_or(&reference.module_name);
            let mut element_net_list = nets
                .get(&reference.name.to_lowercase())
                .cloned()
                .unwrap_or_default();
            element_net_list.sort();
            elements.insert(
                reference.name.to_lowercase(),
                json!({
                    "master": master.to_lowercase(),
                    "nets": element_net_list,
                    "params": param_view,
                    "stimulus": stimulus_view(stimulus_of(circuit, reference)),
                }),
            );
        }
        let mut ports = Vec::new();
        if !(top && flat_top) {
            for port in &module.ports {
                ports.push(port.name.to_lowercase());
            }
        }
        let mut header_params = serde_json::Map::new();
        for parameter in &module.parameters {
            header_params.insert(
                parameter.name.to_lowercase(),
                number_or_text(parameter.default_value.as_ref(), None),
            );
        }
        let content = json!({"ports": ports, "params": header_params, "elements": elements});
        let empty = is_empty_view(&content["ports"])
            && is_empty_view(&content["params"])
            && is_empty_view(&content["elements"]);
        if !empty {
            let key = if top {
                "<top>".to_string()
            } else {
                module.name.to_lowercase()
            };
            circuits.insert(key, content);
        }
    }
    json!({"libraries": libraries, "circuits": circuits})
}

/// Map `XM1` back to `M1` where SPICE added the element letter.
fn letterless(written: &Value, expected: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for (name, element) in written.as_object().unwrap() {
        let mut key = name.clone();
        if expected.get(name).is_none() && name.len() > 1 && expected.get(&name[1..]).is_some() {
            key = name[1..].to_string();
        }
        out.insert(key, element.clone());
    }
    Value::Object(out)
}

/// Every field of `expected` absent from or different in `got`.
fn missing(expected: &Value, got: &Value, path: &str) -> Vec<String> {
    if let (Value::Object(want), Value::Object(have)) = (expected, got) {
        let mut out = Vec::new();
        for (key, value) in want {
            let place = format!("{path}/{key}");
            match have.get(key) {
                None => out.push(format!("{place}: missing")),
                Some(found) => out.extend(missing(value, found, &place)),
            }
        }
        return out;
    }
    if expected == got {
        Vec::new()
    } else {
        vec![format!("{path}: {expected} became {got}")]
    }
}

#[test]
fn conversion_loses_nothing() {
    let mut passed = 0;
    let mut failures = Vec::new();
    for stem in stems() {
        for (format_name, write_format, parse_format) in formats() {
            let options = GschOptions {
                corners: corners_of(&stem),
                ..Default::default()
            };
            let circuit = read_gsch(&fixture_path(&stem), None, &options).unwrap();
            let text = write_format(&circuit);
            let back = match parse_format(&text) {
                Ok(back) => back,
                Err(error) => {
                    failures.push(format!("{stem} {format_name}: {error}\n{text}"));
                    continue;
                }
            };
            let flat = format_name != "verilog-a";
            let expected = simulation_view(&circuit, flat);
            let mut got = simulation_view(&back, flat);
            let back_top = back.top_module.to_lowercase();
            let circuits = got["circuits"].as_object_mut().unwrap();
            if !back_top.is_empty() {
                if let Some(top) = circuits.shift_remove(&back_top) {
                    circuits.insert("<top>".to_string(), top);
                }
            }
            for (name, module_view) in circuits.iter_mut() {
                if let Some(want) = expected["circuits"].get(name) {
                    let renamed = letterless(&module_view["elements"], &want["elements"]);
                    module_view["elements"] = renamed;
                }
            }
            let mut lost = missing(&expected, &got, "");
            let mut macros: Vec<_> = circuit.macros.iter().collect();
            for module in &circuit.modules {
                macros.extend(module.macros.iter());
            }
            for code in macros {
                let mut present = true;
                for line in code.body.lines() {
                    if !text.contains(line.trim()) {
                        present = false;
                    }
                }
                if !present {
                    lost.push(format!("/macros/{}: missing", code.name));
                }
            }
            if lost.is_empty() {
                passed += 1;
            } else {
                failures.push(format!(
                    "{stem} {format_name}:\n{}\n--- {format_name} ---\n{text}",
                    lost.join("\n")
                ));
            }
        }
    }
    assert_eq!(passed, 66, "{}", failures.join("\n\n"));
}

#[test]
fn the_complete_fixture_uses_every_device_kind() {
    let doc = fixture_json("ihp_hier_top");
    let mut kinds = BTreeSet::new();
    for entry in doc.as_object().unwrap().values() {
        if let Some(kind) = entry.get("type").and_then(Value::as_str) {
            kinds.insert(kind.to_string());
        }
    }
    for kind in [
        "capacitor",
        "inductor",
        "vsource",
        "isource",
        "ckt",
        "nmos",
        "pmos",
        "npn",
        "diode",
    ] {
        assert!(kinds.contains(kind), "{kind}");
    }
}

#[test]
fn gsch_library_paths_stay_on_one_line() {
    let mut models = library_models(&[("mos", &["tt\n.control"])]);
    models["models:mos"]["models"][0]["library"] = json!("x.lib\nshell rm");
    let doc = library_doc(&["mos"]).to_string();
    let circuit = parse_gsch(&doc, &model_options(models.clone())).unwrap();
    let lines = spice_lines(&circuit);
    assert!(
        lines.contains(&".lib x.lib shell rm tt .control".to_string()),
        "{lines:?}"
    );
    for line in &lines {
        assert!(line != ".control" && line != "shell rm", "{lines:?}");
    }
    // a Windows path keeps its backslashes; a trailing one would continue the line
    models["models:mos"]["models"][0]["library"] = json!("C:\\models\\x.lib\\");
    let circuit = parse_gsch(&doc, &model_options(models)).unwrap();
    assert_eq!(circuit.directives[0].value, "C:\\models\\x.lib tt .control");
}
