//! SystemVerilog and bit-level HDL nets, ported from kfnetlist 1deb1e0 (SystemVerilog
//! parts), 28b80e0 and 4c98724 (#35), f886515, 650763b, 9766e84, b26cd6b, d5b82b2 and 30a1621.
//! from: kfnetlist tests/morphnet/test_systemverilog_parser.py, test_systemverilog_writer.py,
//! and the #35 and later additions to test_vams_parser.py, test_vams_preprocess.py,
//! test_vams_roundtrip.py and test_vams_writer.py

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use oxymorph_netlist::expression::{parse_expression, to_float};
use oxymorph_netlist::hdl::{sv_name, ExpansionBudget, HdlOptions, MAX_EXPANDED_WIDTH};
use oxymorph_netlist::helpers::PinDomains;
use oxymorph_netlist::preprocess::preprocess_vams;
use oxymorph_netlist::stimulus::stimulus_of;
use oxymorph_netlist::{
    parse_spice, parse_systemverilog, parse_vams, parse_vams_with, write_spectre, write_spice,
    write_systemverilog, write_vams, Error,
};
use oxymorph_schema::{
    Circuit, DirectiveKind, InitialCondition, Module, ModuleReference, Parameter, ParameterValue,
    PortDirection, SignalDomain, Simulation,
};

const WG: SignalDomain = SignalDomain::Waveguide;
const EL: SignalDomain = SignalDomain::Electrical;

const SWITCH_SV: &str = include_str!("data/kf/systemverilog/switch_2x2.sv");
const SWITCH_VAMS: &str = include_str!("data/kf/vams/switch_2x2.vams");

/// A domain's name: sets need an order, which the schema enum does not have.
fn name_of(domain: SignalDomain) -> String {
    format!("{domain:?}")
}

fn parse(text: &str) -> Circuit {
    parse_systemverilog(text, &HdlOptions::default()).unwrap()
}

fn parse_error(text: &str) -> String {
    match parse_systemverilog(text, &HdlOptions::default()) {
        Err(Error::Validation(message)) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

fn options(defines: &[(&str, &str)], disciplines: &[(&str, SignalDomain)]) -> HdlOptions {
    let mut hdl_options = HdlOptions::default();
    for (name, value) in defines {
        hdl_options
            .defines
            .insert(name.to_string(), value.to_string());
    }
    for (name, domain) in disciplines {
        hdl_options.disciplines.insert(name.to_string(), *domain);
    }
    hdl_options
}

fn pin_domains(cells: &[(&str, &[(&str, SignalDomain)])]) -> PinDomains {
    let mut domains = PinDomains::new();
    for (cell, pins) in cells {
        let mut by_pin = IndexMap::new();
        for (pin, domain) in pins.iter() {
            by_pin.insert(pin.to_string(), *domain);
        }
        domains.insert(cell.to_string(), by_pin);
    }
    domains
}

fn module<'a>(circuit: &'a Circuit, name: &str) -> &'a Module {
    circuit
        .modules
        .iter()
        .find(|module| module.name == name)
        .unwrap_or_else(|| panic!("no module {name}"))
}

/// The nets of `cell` as sets of members: a port by its pin name, an instance pin as
/// `instance.pin` (the kfnetlist test helper's view through `to_netlists`).
fn nets(circuit: &Circuit, cell: &str) -> BTreeSet<BTreeSet<String>> {
    let mut by_net: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for connection in &module(circuit, cell).connections {
        let members = by_net.entry(connection.name.clone()).or_default();
        for port_reference in [&connection.source, &connection.target]
            .into_iter()
            .flatten()
        {
            if port_reference.instance_name.is_empty() {
                members.insert(port_reference.port_name.clone());
            } else {
                members.insert(format!(
                    "{}.{}",
                    port_reference.instance_name, port_reference.port_name
                ));
            }
        }
    }
    by_net.into_values().collect()
}

fn net_set(groups: &[&[&str]]) -> BTreeSet<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    for group in groups {
        set.insert(group.iter().map(|member| member.to_string()).collect());
    }
    set
}

fn domains_by_net(module: &Module) -> BTreeMap<String, SignalDomain> {
    let mut domains = BTreeMap::new();
    for connection in &module.connections {
        domains.insert(connection.name.clone(), connection.domain);
    }
    domains
}

fn expected_domains(pairs: &[(&str, SignalDomain)]) -> BTreeMap<String, SignalDomain> {
    let mut domains = BTreeMap::new();
    for (name, domain) in pairs {
        domains.insert(name.to_string(), *domain);
    }
    domains
}

fn value(parameter: &Parameter) -> f64 {
    parameter
        .default_value
        .as_ref()
        .unwrap()
        .prefixed_value
        .as_ref()
        .unwrap()
        .double_value
}

fn params_by_name(parameters: &[Parameter]) -> BTreeMap<&str, &Parameter> {
    let mut by_name = BTreeMap::new();
    for parameter in parameters {
        by_name.insert(parameter.name.as_str(), parameter);
    }
    by_name
}

// ── test_systemverilog_parser.py: TestPorts ──────────────────────────────

fn port_rows(module: &Module) -> Vec<(String, PortDirection, String)> {
    let mut rows = Vec::new();
    for port in &module.ports {
        rows.push((port.name.clone(), port.direction, port.discipline.clone()));
    }
    rows
}

fn rows(expected: &[(&str, PortDirection, &str)]) -> Vec<(String, PortDirection, String)> {
    let mut rows = Vec::new();
    for (name, direction, discipline) in expected {
        rows.push((name.to_string(), *direction, discipline.to_string()));
    }
    rows
}

#[test]
fn ansi_ports_inherit_direction_and_type() {
    let circuit = parse("module m (input wire [3:0] a, b, output logic y, inout z);\nendmodule\n");
    assert_eq!(
        port_rows(&circuit.modules[0]),
        rows(&[
            ("a", PortDirection::Input, "wire"),
            ("b", PortDirection::Input, "wire"),
            ("y", PortDirection::Output, "logic"),
            ("z", PortDirection::Inout, ""),
        ])
    );
}

#[test]
fn non_ansi_ports() {
    let circuit = parse(
        "\
module m (a, b, y);
  input a, b;
  output y;
  wire y;
endmodule
",
    );
    assert_eq!(
        port_rows(&circuit.modules[0]),
        rows(&[
            ("a", PortDirection::Input, ""),
            ("b", PortDirection::Input, ""),
            ("y", PortDirection::Output, "wire"),
        ])
    );
}

#[test]
fn user_type_port() {
    let circuit = parse("module m (inout optical o1, input e1);\nendmodule\n");
    assert_eq!(circuit.modules[0].ports[0].discipline, "optical");
    assert_eq!(circuit.modules[0].ports[1].discipline, "");
}

// ── TestParameters ───────────────────────────────────────────────────────

#[test]
fn header_and_body_parameters() {
    let circuit = parse(
        "\
module m #(parameter real gap = 200e-9, length = 10e-6, parameter int n = 4'b1010) ();
  localparam W = 2 * n + 1;
endmodule
",
    );
    let params = params_by_name(&circuit.modules[0].parameters);
    assert!((value(params["gap"]) - 200e-9).abs() < 1e-18);
    assert_eq!(params["length"].properties["type"], "real");
    assert_eq!(value(params["n"]), 10.0);
    assert_eq!(
        params["W"].default_value,
        Some(ParameterValue {
            expression: Some(parse_expression("2 * n + 1").unwrap()),
            ..Default::default()
        })
    );
    let mut localparam = IndexMap::new();
    localparam.insert("localparam".to_string(), "true".to_string());
    assert_eq!(params["W"].properties, localparam);
}

#[test]
fn instance_parameter_overrides() {
    let text = "\
module wg #(parameter length = 1, width = 2) (inout o1, o2);
endmodule
module top (inout o1, o2);
  wg #(.length(10u)) X1 (.o1(o1), .o2(n1));
  wg #(5, 6) X2 (n1, o2);
endmodule
";
    let circuit = parse(text);
    let [x1, x2] = circuit.modules[1].module_references.as_slice() else {
        panic!("two instances");
    };
    assert_eq!(x1.parameters[0].name, "length");
    assert_eq!(value(&x1.parameters[0]), 10.0);
    let names: Vec<&str> = x2
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    assert_eq!(names, ["length", "width"]);
}

// ── TestInstances ────────────────────────────────────────────────────────

#[test]
fn connection_styles() {
    let text = "\
module cell (input a, b, output y);
endmodule
module top (input a, b, output y, z);
  wire n1;
  cell U1 (.a, .b(b), .y(n1));
  cell U2 (.*);
  cell U3 (n1, , z), U4 (.a(1'b0), .b(), .y(y));
endmodule
";
    assert_eq!(
        nets(&parse(text), "top"),
        net_set(&[
            &["a", "U1.a", "U2.a"],
            &["b", "U1.b", "U2.b"],
            &["U1.y", "U3.a"],
            &["y", "U2.y", "U4.y"],
            &["z", "U3.y"],
            &["U4.a"], // tied to 1'b0: a node of its own
        ])
    );
}

#[test]
fn positional_unknown_module_uses_pin_index() {
    let circuit = parse("module top (inout a, b);\n  ext X1 (a, b);\nendmodule\n");
    let mut pins = BTreeSet::new();
    for connection in &circuit.modules[0].connections {
        if let Some(target) = &connection.target {
            pins.insert(target.port_name.as_str());
        }
    }
    assert_eq!(pins, BTreeSet::from(["p0", "p1"]));
}

#[test]
fn wildcard_needs_definition() {
    let message = parse_error("module top (inout a);\n  ext X1 (.*);\nendmodule\n");
    assert!(message.contains("needs the definition"), "{message}");
}

#[test]
fn bit_select_net_names() {
    let text = "\
module top (input [1:0] bus);
  wire [1:0] n;
  buf_cell U1 (.a(bus[0]), .y(n[1])), U2 (.a(n[1]), .y(bus[1]));
endmodule
";
    let circuit = parse(text);
    assert_eq!(
        nets(&circuit, "top"),
        net_set(&[&["bus[0]", "U1.a"], &["bus[1]", "U2.y"], &["U1.y", "U2.a"]])
    );
    let top = &circuit.modules[0];
    assert!(top
        .connections
        .iter()
        .any(|connection| connection.name == "n[1]"));
    let mut range = IndexMap::new();
    range.insert("range".to_string(), "1:0".to_string());
    assert_eq!((top.ports[0].width, &top.ports[0].properties), (2, &range));
}

#[test]
fn bus_errors_raise() {
    let cases = [
        ("buf_cell U1 (.a(n[1]));", "not a bus"), // undeclared bus
        ("buf_cell U1 (.a(bus[2]));", "outside bus"), // out of range
        ("wire [2:0] w;\n  assign w = bus;", "width mismatch"),
        ("sub U1 (.a(bus));", "2 bits for 1 instance"), // sub.a is 1 bit
    ];
    for (body, error) in cases {
        let text = format!(
            "module sub (input a);\nendmodule\nmodule top (input [1:0] bus);\n  {body}\nendmodule\n"
        );
        let message = parse_error(&text);
        assert!(message.contains(error), "{body}: {message}");
    }
}

// ── TestNetMerging ───────────────────────────────────────────────────────

#[test]
fn assign_and_alias_merge_nets() {
    let text = "\
module top (inout a, b);
  wire n1, n2, n3;
  wire n4 = n3;
  assign n1 = a;
  alias n2 = n3 = b;
  r X1 (.p(n1), .n(n2));
  r X2 (.p(n4), .n(a));
endmodule
";
    assert_eq!(
        nets(&parse(text), "top"),
        net_set(&[&["a", "X1.p", "X2.n"], &["b", "X1.n", "X2.p"]])
    );
}

// ── TestPreprocessing ────────────────────────────────────────────────────

#[test]
fn behavioral_blocks_are_stored() {
    let text = "\
module top (input clk, output reg q);
  /* block comment */
  (* keep *) always_ff @(posedge clk) begin
    if (q) q <= 1'b0; else q <= 1'b1;
  end
  always @(*) case (q) 1'b0: q = 1; default: q = 0; endcase
  initial q = 0;
  function automatic int f(int x); return x; endfunction
  generate for (genvar i = 0; i < 2; i++) begin : g end endgenerate
  assign q = clk & q;
  typedef struct { logic a; logic b; } pair_t;
  r X1 (.p(clk), .n(q));  // trailing comment
endmodule
";
    let circuit = parse(text);
    let module = &circuit.modules[0];
    let blocks: Vec<&String> = module.properties.values().collect();
    assert_eq!(blocks.len(), 6);
    assert!(blocks[0].starts_with("always_ff") && blocks[0].ends_with("end"));
    assert!(blocks[1].ends_with("endcase"));
    assert_eq!(blocks[5], "assign q = clk & q;");
    let names: Vec<&str> = module
        .module_references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(names, ["X1"]);
}

#[test]
fn directives() {
    let circuit = parse("`timescale 1ns/1ps\n`include \"cells.sv\"\nmodule m ();\nendmodule\n");
    let found: Vec<(DirectiveKind, &str)> = circuit
        .directives
        .iter()
        .map(|directive| (directive.kind, directive.value.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (DirectiveKind::Timescale, "1ns/1ps"),
            (DirectiveKind::Include, "cells.sv"),
        ]
    );
}

#[test]
fn top_is_the_uninstantiated_module() {
    let text = "\
module top (inout a);
  leaf X1 (.a);
endmodule : top
module leaf (inout a);
endmodule
";
    assert_eq!(parse(text).top_module, "top");
}

#[test]
fn electrical_rc_divider() {
    let text = "\
module rc (input vin, output vout, inout gnd);
  resistor #(.r(1k)) R1 (.p(vin), .n(vout));
  capacitor #(.c(100p)) C1 (.p(vout), .n(gnd));
endmodule
";
    let circuit = parse(text);
    let rc = &circuit.modules[0];
    assert_eq!(rc.module_references[0].module_name, "resistor");
    let capacitance = rc.module_references[1].parameters[0]
        .default_value
        .as_ref()
        .unwrap();
    assert!((to_float(capacitance.prefixed_value.as_ref().unwrap()) - 1e-10).abs() < 1e-20);
    assert_eq!(
        nets(&circuit, "rc"),
        net_set(&[
            &["vin", "R1.p"],
            &["vout", "R1.n", "C1.p"],
            &["gnd", "C1.n"]
        ])
    );
    let domains: BTreeSet<String> = rc
        .connections
        .iter()
        .map(|connection| name_of(connection.domain))
        .collect();
    assert_eq!(domains, BTreeSet::from([name_of(EL)]));
}

#[test]
fn waveguide_mzi_with_heater() {
    let text = "\
module dc_coupler #(parameter real gap = 200e-9) (input o1, o2, output o3, o4);
  wg_directional_coupler Xcoupler (.o1, .o2, .o3, .o4);
endmodule

module mzi (input o1, o2, output o3, o4, input e1, inout vdd);
  dc_coupler #(.gap(150n)) Xdc1 (o1, o2, top_a, bot_a);
  dc_coupler Xdc2 (.o1(top_b), .o2(bot_b), .o3(o3), .o4(o4));
  wg_heater Xarm_top (.o1(top_a), .o2(top_b), .e1(e1), .e2(vdd));
  wg_delay Xarm_bot (.o1(bot_a), .o2(bot_b));
endmodule
";
    let circuit = parse(text);
    assert_eq!(circuit.top_module, "mzi");
    let mzi = &circuit.modules[1];
    let port_domains: BTreeMap<String, SignalDomain> = mzi
        .ports
        .iter()
        .map(|port| (port.name.clone(), port.domain))
        .collect();
    assert_eq!(
        port_domains,
        expected_domains(&[
            ("o1", WG),
            ("o2", WG),
            ("o3", WG),
            ("o4", WG),
            ("e1", EL),
            ("vdd", EL)
        ])
    );
    assert_eq!(
        domains_by_net(mzi),
        expected_domains(&[
            ("o1", WG),
            ("o2", WG),
            ("o3", WG),
            ("o4", WG),
            ("e1", EL),
            ("vdd", EL),
            ("top_a", WG),
            ("bot_a", WG),
            ("top_b", WG),
            ("bot_b", WG),
        ])
    );
    assert_eq!(
        nets(&circuit, "mzi"),
        net_set(&[
            &["o1", "Xdc1.o1"],
            &["o2", "Xdc1.o2"],
            &["o3", "Xdc2.o3"],
            &["o4", "Xdc2.o4"],
            &["e1", "Xarm_top.e1"],
            &["vdd", "Xarm_top.e2"],
            &["Xdc1.o3", "Xarm_top.o1"],
            &["Xdc1.o4", "Xarm_bot.o1"],
            &["Xarm_top.o2", "Xdc2.o1"],
            &["Xarm_bot.o2", "Xdc2.o2"],
        ])
    );
}

/// Review Focus 5: an `o1` pin of a child module makes the parent's net waveguide, and
/// the parent's untyped port on that net too, up the hierarchy.
#[test]
fn waveguide_domain_propagates_up_the_hierarchy() {
    let text = "\
module leaf (inout o1, inout e1);
endmodule
module middle (inout a, inout b);
  leaf X1 (.o1(a), .e1(b));
endmodule
module top (inout p, inout q);
  middle M1 (.a(p), .b(q));
endmodule
";
    for circuit in [
        parse(text),
        parse_vams(
            "module leaf (o1, e1);\nendmodule\nmodule middle (a, b);\n  leaf X1 (.o1(a), .e1(b));\nendmodule\nmodule top (p, q);\n  middle M1 (.a(p), .b(q));\nendmodule\n",
        )
        .unwrap(),
    ] {
        let middle = module(&circuit, "middle");
        assert_eq!(middle.ports[0].domain, WG);
        assert_eq!(middle.ports[1].domain, EL);
        assert_eq!(domains_by_net(middle), expected_domains(&[("a", WG), ("b", EL)]));
        let top = module(&circuit, "top");
        assert_eq!(top.ports[0].domain, WG);
        assert_eq!(top.ports[1].domain, EL);
        assert_eq!(domains_by_net(top), expected_domains(&[("p", WG), ("q", EL)]));
    }
}

// ── TestBuses ────────────────────────────────────────────────────────────

fn switch_nets() -> BTreeSet<BTreeSet<String>> {
    net_set(&[
        &["in[1]", "I_I[1].in0"],
        &["in[0]", "I_I[0].in0"],
        &["out[1]", "I_O[1].out0"],
        &["out[0]", "I_O[0].out0"],
        &["sel[3]", "I_O[1].sel"],
        &["sel[2]", "I_O[0].sel"],
        &["sel[1]", "I_I[1].sel"],
        &["sel[0]", "I_I[0].sel"],
        &["I_I[0].out0", "I_O[0].in0"], // a[0]
        &["I_I[0].out1", "I_O[1].in0"], // a[1]
        &["I_I[1].out0", "I_O[0].in1"], // a[2]
        &["I_I[1].out1", "I_O[1].in1"], // a[3]
    ])
}

fn switch_domains() -> BTreeMap<String, SignalDomain> {
    expected_domains(&[
        ("in[1]", WG),
        ("in[0]", WG),
        ("out[1]", WG),
        ("out[0]", WG),
        ("a[0]", WG),
        ("a[1]", WG),
        ("a[2]", WG),
        ("a[3]", WG),
        ("sel[3]", EL),
        ("sel[2]", EL),
        ("sel[1]", EL),
        ("sel[0]", EL),
    ])
}

fn switch_types() -> [(&'static str, SignalDomain); 2] {
    [("connection", WG), ("mzmselect", EL)]
}

#[test]
fn switch_simulation_view() {
    let circuit =
        parse_systemverilog(SWITCH_SV, &options(&[("IS_SIM", "1")], &switch_types())).unwrap();
    let switch = &circuit.modules[0];
    let ports: Vec<(&str, u32, &str)> = switch
        .ports
        .iter()
        .map(|port| (port.name.as_str(), port.width, port.discipline.as_str()))
        .collect();
    assert_eq!(
        ports,
        [
            ("in", 2, "connection"),
            ("out", 2, "connection"),
            ("sel", 4, "mzmselect")
        ]
    );
    let names: Vec<&str> = switch
        .module_references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(names, ["I_I[1]", "I_I[0]", "I_O[1]", "I_O[0]"]);
    assert_eq!(nets(&circuit, "SWITCH_2X2"), switch_nets());
    assert_eq!(domains_by_net(switch), switch_domains());
}

#[test]
fn switch_default_view_is_electrical() {
    // without IS_SIM the `else branch declares plain wires: no types to map
    let circuit = parse_systemverilog(SWITCH_SV, &options(&[], &switch_types())).unwrap();
    assert_eq!(nets(&circuit, "SWITCH_2X2"), switch_nets());
    let domains: BTreeSet<String> = circuit.modules[0]
        .connections
        .iter()
        .map(|connection| name_of(connection.domain))
        .collect();
    assert_eq!(domains, BTreeSet::from([name_of(EL)]));
}

#[test]
fn leaf_pin_domains() {
    // plain-wire view: only the leaf cells' pins say what is optical
    let pins = pin_domains(&[
        (
            "MZM_1X2",
            &[("in0", WG), ("out0", WG), ("out1", WG), ("sel", EL)],
        ),
        (
            "MZM_2X1",
            &[("in0", WG), ("in1", WG), ("out0", WG), ("sel", EL)],
        ),
    ]);
    let hdl_options = HdlOptions {
        pin_domains: Some(pins),
        ..Default::default()
    };
    let circuit = parse_systemverilog(SWITCH_SV, &hdl_options).unwrap();
    let switch = &circuit.modules[0];
    assert_eq!(domains_by_net(switch), switch_domains());
    let domains: Vec<SignalDomain> = switch.ports.iter().map(|port| port.domain).collect();
    assert_eq!(domains, [WG, WG, EL]);
}

#[test]
fn pin_domains_override_types_of_defined_cells() {
    let text = "\
module cell (input connection o1, inout connection e1);
endmodule
module top (inout a);
  cell X1 (.o1(a), .e1());
endmodule
";
    let hdl_options = HdlOptions {
        pin_domains: Some(pin_domains(&[("cell", &[("o1", EL)])])),
        ..options(&[], &[("connection", WG)])
    };
    let circuit = parse_systemverilog(text, &hdl_options).unwrap();
    let domains: Vec<SignalDomain> = circuit.modules[0]
        .ports
        .iter()
        .map(|port| port.domain)
        .collect();
    assert_eq!(domains, [EL, WG]);
    let net_domains: BTreeSet<String> = circuit.modules[1]
        .connections
        .iter()
        .map(|connection| name_of(connection.domain))
        .collect();
    assert_eq!(net_domains, BTreeSet::from([name_of(EL)]));
}

#[test]
fn arrays_of_defined_modules_slice_or_broadcast() {
    let text = "\
module cell (input [1:0] a, input en, output y);
endmodule
module top (input [3:0] bus, input en, output [1:0] ys);
  cell C[1:0] (.a(bus), .en(en), .y(ys));
  leaf L (.x({2{en}}));
endmodule
";
    assert_eq!(
        nets(&parse(text), "top"),
        net_set(&[
            &["bus[3]", "C[1].a[1]"],
            &["bus[2]", "C[1].a[0]"],
            &["bus[1]", "C[0].a[1]"],
            &["bus[0]", "C[0].a[0]"],
            &["en", "C[1].en", "C[0].en", "L.x[1]", "L.x[0]"],
            &["ys[1]", "C[1].y"],
            &["ys[0]", "C[0].y"],
        ])
    );
}

#[test]
fn packed_and_unpacked_user_types() {
    let text = "\
module m (input connection [1:0] a, b, output connection c [2:0], d);
  input connection [4:1] e;
endmodule
";
    let circuit = parse(text);
    let ports = &circuit.modules[0].ports;
    let rows: Vec<(&str, u32, &str)> = ports
        .iter()
        .map(|port| (port.name.as_str(), port.width, port.discipline.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("a", 2, "connection"),
            ("b", 2, "connection"),
            ("c", 3, "connection"),
            ("d", 0, "connection"),
        ]
    );
    assert_eq!(ports[2].properties["range"], "2:0");
}

/// Untrusted text: replications, ranges and instance arrays are capped at
/// `MAX_EXPANDED_WIDTH` bits or instances, and nesting is bounded.
#[test]
fn hdl_expansion_is_bounded() {
    let too_many = MAX_EXPANDED_WIDTH + 1;
    let limit = "exceed the limit";
    let nesting = "deeper than";
    let cases = [
        (
            format!("module top (inout a);\n  leaf L (.x({{{too_many}{{a}}}}));\nendmodule\n"),
            limit,
        ),
        (
            "module top (inout a);\n  leaf L (.x({1000{{1001{a}}}}));\nendmodule\n".to_string(),
            limit,
        ),
        (format!("module top (input [{too_many}:1] a);\nendmodule\n"), limit),
        ("module top (input [99999999999999:0] a);\nendmodule\n".to_string(), limit),
        (
            format!("module top (inout a);\n  leaf L[{too_many}:1] (.x(a));\nendmodule\n"),
            limit,
        ),
        // 1001 instances × a 1000-bit port broadcast: past a million pins
        (
            "module cell (input [999:0] x);\nendmodule\nmodule top (input [999:0] a);\n  cell C[1000:0] (.x(a));\nendmodule\n"
                .to_string(),
            limit,
        ),
        (
            format!(
                "module top (inout a);\n  leaf L (.x({}a{}));\nendmodule\n",
                "{".repeat(300),
                "}".repeat(300)
            ),
            nesting,
        ),
        (
            format!(
                "module top (inout a);\n  leaf #(.p({}1{})) L (.x(a));\nendmodule\n",
                "(".repeat(300),
                ")".repeat(300)
            ),
            nesting,
        ),
        (
            format!(
                "module top (inout a);\n  initial {} x = 1;\nendmodule\n",
                "if (a) ".repeat(300)
            ),
            nesting,
        ),
    ];
    for (text, reason) in &cases {
        match parse_systemverilog(&with_budget_room(text), &HdlOptions::default()) {
            Err(Error::Validation(message)) => assert!(message.contains(reason), "{message}"),
            other => panic!("{}: {other:?}", &text[..text.len().min(120)]),
        }
    }
    let vams =
        format!("module top (a);\n  inout a;\n  leaf L (.x({{{too_many}{{a}}}}));\nendmodule\n");
    assert!(matches!(parse_vams(&vams), Err(Error::Validation(_))));
    let at_the_limit = format!(
        "module top (inout a);\n  leaf L (.x({{{MAX_EXPANDED_WIDTH}{{a}}}}));\nendmodule\n"
    );
    let at_the_limit = with_budget_room(&at_the_limit);
    assert!(parse_systemverilog(&at_the_limit, &HdlOptions::default()).is_ok());
}

/// `text` with a 100 kB comment: a per-parse expansion budget of 6.4M units, so the
/// per-expression cap is what a case meets.
fn with_budget_room(text: &str) -> String {
    format!("{text}/*{}*/\n", "x".repeat(100_000))
}

fn budget_error(result: Result<Circuit, Error>) -> String {
    match result {
        Err(Error::Validation(message)) if message.contains("expansion budget") => message,
        other => panic!("expected the expansion budget error, got {other:?}"),
    }
}

/// Small texts that each stay under the per-expression cap but would amplify to many
/// millions of bits, pins or bytes: the per-parse budget refuses them.
#[test]
fn hdl_budget_bounds_amplification() {
    let mut inherited = Vec::new();
    for index in 0..100 {
        inherited.push(format!("a{index}"));
    }
    let ansi_inheritance = format!(
        "module top (input [999999:0] {});\nendmodule\n",
        inherited.join(", ")
    );
    let wide_cell = "module c (input [99999:0] x);\nendmodule\n";
    let mut broadcasts = String::from(wide_cell);
    let mut unconnected = String::from(wide_cell);
    broadcasts.push_str("module top ();\n");
    unconnected.push_str("module top ();\n");
    for index in 0..20 {
        broadcasts.push_str(&format!("  c u{index} (1'b0);\n"));
        unconnected.push_str(&format!("  c u{index} ();\n"));
    }
    broadcasts.push_str("endmodule\n");
    unconnected.push_str("endmodule\n");
    let mut params = Vec::new();
    for index in 0..20 {
        params.push(format!(".p{index}({index})"));
    }
    let array_with_params = format!(
        "module top (inout n);\n  leaf #({}) L[99999:0] (.x(n));\nendmodule\n",
        params.join(", ")
    );
    let define_blow_up = format!(
        "`define A {}\nmodule top (inout a);\n  {}\nendmodule\n",
        "x".repeat(10_000),
        "`A ".repeat(200)
    );
    for text in [
        &ansi_inheritance,
        &broadcasts,
        &unconnected,
        &array_with_params,
        &define_blow_up,
    ] {
        assert!(text.len() < 20_000, "{}", text.len());
        budget_error(parse_systemverilog(text, &HdlOptions::default()));
    }
    let vams_define_blow_up = format!(
        "`define A {}\nmodule top (a);\n  {}\nendmodule\n",
        "x".repeat(10_000),
        "`A ".repeat(200)
    );
    budget_error(parse_vams(&vams_define_blow_up));
    let mut vams_unconnected =
        String::from("module c (x);\n  inout [99999:0] x;\nendmodule\nmodule top ();\n");
    for index in 0..20 {
        vams_unconnected.push_str(&format!("  c u{index} (.x());\n"));
    }
    vams_unconnected.push_str("endmodule\n");
    budget_error(parse_vams(&vams_unconnected));

    // a realistic netlist stays far inside the budget
    let simulation = options(&[("IS_SIM", "1")], &switch_types());
    assert!(parse_systemverilog(SWITCH_SV, &simulation).is_ok());
    assert!(parse_vams_with(SWITCH_VAMS, &simulation).is_ok());
}

/// A header port named twice would expand its bits once per occurrence: both parsers
/// refuse it (kfnetlist keeps duplicates).
#[test]
fn duplicate_header_ports_are_an_error() {
    let repeated = vec!["a"; 150];
    let names = repeated.join(", ");
    let vams = format!("module c ({names});\n  inout [9999:0] a;\nendmodule\n");
    let systemverilog = [
        format!("module c ({names});\n  inout [9999:0] a;\nendmodule\n"),
        "module c (inout [3:0] a, b, a);\nendmodule\n".to_string(),
    ];
    expect_duplicate_port(parse_vams(&vams));
    for text in &systemverilog {
        expect_duplicate_port(parse_systemverilog(text, &HdlOptions::default()));
    }
}

fn expect_duplicate_port(result: Result<Circuit, Error>) {
    match result {
        Err(Error::Validation(message)) => {
            assert!(message.contains("port 'a' appears twice"), "{message}")
        }
        other => panic!("expected a duplicate-port error, got {other:?}"),
    }
}

/// IR from untrusted JSON: a port wider than the cap, or with a range no `i64` walk
/// should cover, is written as a scalar, quickly and without a panic.
#[test]
fn writers_cap_untrusted_port_width() {
    let ports = r#"[
        {"name": "wide", "direction": 1, "width": 4000000000},
        {"name": "huge", "direction": 1, "width": 2,
         "properties": {"range": "9223372036854775807:-9223372036854775808"}}
    ]"#;
    let json = format!(
        r#"{{"top_module": "top", "modules": [
            {{"name": "cell", "ports": {ports}}},
            {{"name": "top", "module_references": [{{"name": "u0", "module_name": "cell"}}]}}
        ]}}"#
    );
    let circuit = Circuit::from_json(&json).unwrap();
    let started = std::time::Instant::now();
    let systemverilog = write_systemverilog(&circuit);
    let vams = write_vams(&circuit);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(
        systemverilog.contains("  input wide,\n  input huge\n"),
        "{systemverilog}"
    );
    assert!(
        systemverilog.contains("cell u0 (.wide(), .huge());"),
        "{systemverilog}"
    );
    assert!(vams.contains("  input wide, huge;\n"), "{vams}");
    assert!(vams.contains("cell u0 (.wide(), .huge());"), "{vams}");
}

/// IR from JSON: unconnected ports as wide as the cap cost nothing to write, since a port
/// an instance has no pin on is not expanded bit by bit.
#[test]
fn writers_do_not_expand_unconnected_wide_ports() {
    const PORT_COUNT: usize = 20;
    const INSTANCE_COUNT: usize = 5;
    let mut ports = Vec::new();
    for port_index in 0..PORT_COUNT {
        ports.push(format!(
            r#"{{"name": "p{port_index}", "width": {MAX_EXPANDED_WIDTH}}}"#
        ));
    }
    let mut references = Vec::new();
    for instance_index in 0..INSTANCE_COUNT {
        references.push(format!(
            r#"{{"name": "x{instance_index}", "module_name": "foo"}}"#
        ));
    }
    let json = format!(
        r#"{{"top_module": "top",
            "ext_modules": [{{"name": "foo", "ports": [{}]}}],
            "modules": [{{"name": "top", "module_references": [{}]}}]}}"#,
        ports.join(", "),
        references.join(", ")
    );
    let circuit = Circuit::from_json(&json).unwrap();
    let mut open_ports = Vec::new();
    for port_index in 0..PORT_COUNT {
        open_ports.push(format!(".p{port_index}()"));
    }
    let expected = format!("  foo x4 ({});", open_ports.join(", "));
    let started = std::time::Instant::now();
    let systemverilog = write_systemverilog(&circuit);
    let vams = write_vams(&circuit);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(systemverilog.contains(&expected), "{systemverilog}");
    assert!(vams.contains(&expected), "{vams}");
}

/// Every writer finds an instance's pins in a per-module map, not by scanning the
/// connections, so a parsed instance array costs time linear in its size.
#[test]
fn writers_are_linear_in_instances() {
    const INSTANCE_COUNT: usize = 20_000;
    let text = format!(
        "module top(input a); foo x[0:{}] (.p(a)); endmodule\n",
        INSTANCE_COUNT - 1
    );
    let circuit = parse(&text);
    let started = std::time::Instant::now();
    let systemverilog = write_systemverilog(&circuit);
    let vams = write_vams(&circuit);
    let spice = write_spice(&circuit);
    let spectre = write_spectre(&circuit);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let last = INSTANCE_COUNT - 1;
    let hdl_line = format!("  foo \\x[{last}]  (.p(a));\n");
    assert!(systemverilog.contains(&hdl_line));
    assert!(vams.contains(&hdl_line));
    assert!(spice.contains(&format!("\nx[{last}] a foo\n")));
    assert!(spectre.contains(&format!("\nx[{last}] (a) foo\n")));
}

/// Bit indices of net names from JSON span all of i64: regrouping them into runs must not
/// overflow (`a[i64::MIN]` next to `a[1]`).
#[test]
fn format_bits_takes_extreme_indices() {
    let json = r#"{"top_module": "top", "modules": [{"name": "top",
        "module_references": [{"name": "x", "module_name": "foo"}],
        "connections": [
            {"name": "a[-9223372036854775808]", "source": {"instance_name": "x", "port_name": "p[1]"},
             "target": {"instance_name": "", "port_name": "a"}},
            {"name": "a[1]", "source": {"instance_name": "x", "port_name": "p[0]"},
             "target": {"instance_name": "", "port_name": "b"}},
            {"name": "c[9223372036854775807]", "source": {"instance_name": "x", "port_name": "q[1]"},
             "target": {"instance_name": "", "port_name": "c"}},
            {"name": "c[-1]", "source": {"instance_name": "x", "port_name": "q[0]"},
             "target": {"instance_name": "", "port_name": "d"}}
        ]}]}"#;
    let circuit = Circuit::from_json(json).unwrap();
    let systemverilog = write_systemverilog(&circuit);
    assert!(
        systemverilog.contains(
            "foo x (.p({a[-9223372036854775808], a[1]}), .q({c[9223372036854775807], c[-1]}));"
        ),
        "{systemverilog}"
    );
    write_vams(&circuit);
}

// ── test_systemverilog_writer.py ─────────────────────────────────────────

const MZI: &str = "\
`timescale 1ns/1ps
module dc #(parameter real gap = 200e-9, parameter int n = 4'b1010) (
  input o1, o2, output o3, o4);
  localparam W = 2*n+1;
  wg_directional_coupler Xc (.o1, .o2, .o3, .o4);
endmodule
module mzi (input optical o1, o2, output o3, o4, input e1, inout vdd);
  dc #(.gap(150n)) X1 (o1, o2, a, b);
  dc X2 (.o1(a), .o2(b), .o3(o3), .o4());
  heater H (.e1(e1), .e2(vdd));
  always @(posedge e1) vdd <= 1;
endmodule
";

const INVERTER: &str = "\
.subckt inv in out vdd vss
M1 out in vdd vdd pmos w=2u l=180n
M2 out in vss vss nmos w=1u l=180n
.ends inv
";

fn written() -> String {
    write_systemverilog(&parse(MZI))
}

#[test]
fn header_ports_and_parameters() {
    let written = written();
    assert!(written.contains("`timescale 1ns/1ps"), "{written}");
    assert!(
        written.contains("module dc #(parameter real gap = 2e-07, parameter int n = 10) ("),
        "{written}"
    );
    assert!(written.contains("  input optical o1,\n"), "{written}");
    assert!(written.contains("  localparam W = 2 * n + 1;"), "{written}");
}

#[test]
fn instances_use_named_connections() {
    let written = written();
    assert!(
        written.contains("  dc #(.gap(1.5e-07)) X1 (.o1(o1), .o2(o2), .o3(a), .o4(b));"),
        "{written}"
    );
    assert!(
        written.contains("  dc X2 (.o1(a), .o2(b), .o3(o3), .o4());"),
        "{written}"
    );
}

#[test]
fn behavioral_blocks_are_written_back() {
    let written = written();
    assert!(
        written.contains("  always @(posedge e1) vdd <= 1;\nendmodule"),
        "{written}"
    );
}

#[test]
fn write_parse_write_is_stable() {
    let written = written();
    assert_eq!(write_systemverilog(&parse(&written)), written);
}

#[test]
fn spice_devices_become_modules() {
    let text = write_systemverilog(&parse_spice(INVERTER).unwrap());
    assert!(
        text.contains("mosfet #(.w(2e-06), .l(1.8e-07)) M1 (.d(out), .g(in), .s(vdd), .b(vdd));"),
        "{text}"
    );
}

#[test]
fn escaped_names() {
    assert_eq!(sv_name("net1"), "net1");
    assert_eq!(sv_name("0"), "\\0 ");
    assert_eq!(sv_name("wire"), "\\wire ");
    let circuit = parse(
        "module t (inout a);\n  r R1 (.p(a), .n(\\0 ));\n  r R2 (.p(\\0 ), .n(a));\nendmodule\n",
    );
    assert_eq!(write_systemverilog(&circuit).matches(".n(\\0 )").count(), 1);
}

#[test]
fn devices_without_declaration_keep_pin_order() {
    // Verilog-AMS / SystemVerilog carry no device declarations: positional writers must
    // still use the d, g, s, b template.
    let writers: [fn(&Circuit) -> String; 2] = [write_spice, write_spectre];
    for write in writers {
        let text = write(&parse_vams(&write_vams(&parse_spice(INVERTER).unwrap())).unwrap());
        assert!(text.contains("out in vdd vdd"), "{text}");
    }
}

type ConnectionRow = (String, String, (String, String), (String, String));

fn connection_set(circuit: &Circuit) -> BTreeSet<ConnectionRow> {
    let mut rows = BTreeSet::new();
    for module in &circuit.modules {
        for connection in &module.connections {
            let (Some(source), Some(target)) = (&connection.source, &connection.target) else {
                continue;
            };
            rows.insert((
                connection.name.clone(),
                name_of(connection.domain),
                (source.instance_name.clone(), source.port_name.clone()),
                (target.instance_name.clone(), target.port_name.clone()),
            ));
        }
    }
    rows
}

#[test]
fn buses_round_trip() {
    let types = [("connection", WG)];
    let parsed = parse_systemverilog(SWITCH_SV, &options(&[("IS_SIM", "1")], &types)).unwrap();
    let text = write_systemverilog(&parsed);
    assert!(text.contains("  input connection in [1:0],\n"), "{text}");
    assert!(text.contains("  connection a [3:0];\n"), "{text}");
    assert!(
        text.contains("MZM_1X2 \\I_I[1]  (.in0(in[1]), .sel(sel[1]), .out0(a[2]), .out1(a[3]));"),
        "{text}"
    );
    let reparsed = parse_systemverilog(&text, &options(&[], &types)).unwrap();
    assert_eq!(connection_set(&reparsed), connection_set(&parsed));
    let ports: Vec<(&str, u32)> = reparsed.modules[0]
        .ports
        .iter()
        .map(|port| (port.name.as_str(), port.width))
        .collect();
    assert_eq!(ports, [("in", 2), ("out", 2), ("sel", 4)]);
    assert_eq!(write_systemverilog(&reparsed), text);
}

#[test]
fn bus_connections_are_grouped() {
    let text = "\
module cell (input [3:0] a, output y);
endmodule
module top (input [3:0] bus, input [1:0] b, output y);
  cell C1 (.a({b, bus[3:2]}), .y(y));
  cell C2 (.a(bus), .y());
endmodule
";
    let written = write_systemverilog(&parse(text));
    assert!(written.contains("  input [3:0] bus,\n"), "{written}");
    assert!(
        written.contains("  cell C1 (.a({b, bus[3:2]}), .y(y));"),
        "{written}"
    );
    assert!(written.contains("  cell C2 (.a(bus), .y());"), "{written}");
}

fn waveform_numbers(circuit: &Circuit, reference: &ModuleReference) -> BTreeMap<String, f64> {
    let stimulus = stimulus_of(circuit, reference).unwrap();
    let mut values = BTreeMap::new();
    for (name, value) in &stimulus.waveform.as_ref().unwrap().args {
        values.insert(
            name.clone(),
            to_float(value.prefixed_value.as_ref().unwrap()),
        );
    }
    values
}

#[test]
fn a_stimulus_is_an_annex_e_primitive_that_reads_back() {
    let circuit =
        parse_spice("* t\nV1 a 0 DC 0.9 PULSE(0 1.8 1n 10p 10p 1n 2n)\nR1 a 0 1k\n.end\n").unwrap();
    let text = write_systemverilog(&circuit);
    // no SI suffix
    assert!(
        text.contains("vpulse #(.dc(0.9), .val0(0), .val1(1.8), .td(1e-9)"),
        "{text}"
    );
    let back = parse(&text);
    let v1 = back
        .modules
        .iter()
        .flat_map(|module| &module.module_references)
        .find(|reference| reference.name == "V1")
        .unwrap();
    assert_eq!(v1.module_name, "vsource");
    let original = waveform_numbers(&circuit, &circuit.modules[0].module_references[0]);
    let read_back = waveform_numbers(&back, v1);
    assert_eq!(
        original.keys().collect::<Vec<_>>(),
        read_back.keys().collect::<Vec<_>>()
    );
    for (name, number) in &original {
        assert!(
            (number - read_back[name]).abs() <= 1e-12 * number.abs().max(1.0),
            "{name}"
        );
    }
}

// ── test_vams_parser.py: TestBuses (#35) ─────────────────────────────────

type ConnectionTuple = (String, String, String, String, String, String);

#[test]
fn vams_switch_simulation_view() {
    let circuit = parse_vams_with(
        SWITCH_VAMS,
        &options(&[("IS_SIM", "1")], &[("optical", WG)]),
    )
    .unwrap();
    let switch = &circuit.modules[0];
    let ports: Vec<(&str, u32, &str, SignalDomain)> = switch
        .ports
        .iter()
        .map(|port| {
            (
                port.name.as_str(),
                port.width,
                port.discipline.as_str(),
                port.domain,
            )
        })
        .collect();
    assert_eq!(
        ports,
        [
            ("in", 2, "optical", WG),
            ("out", 2, "optical", WG),
            ("sel", 4, "electrical", EL),
        ]
    );
    let mut connections: BTreeSet<ConnectionTuple> = BTreeSet::new();
    for connection in &switch.connections {
        let (Some(source), Some(target)) = (&connection.source, &connection.target) else {
            continue;
        };
        connections.insert((
            connection.name.clone(),
            source.instance_name.clone(),
            source.port_name.clone(),
            target.instance_name.clone(),
            target.port_name.clone(),
            name_of(connection.domain),
        ));
    }
    let expected = [
        ("in[1]", "", "in[1]", "I_I[1]", "in0", WG),
        ("in[0]", "", "in[0]", "I_I[0]", "in0", WG),
        ("out[1]", "", "out[1]", "I_O[1]", "out0", WG),
        ("out[0]", "", "out[0]", "I_O[0]", "out0", WG),
        ("sel[3]", "", "sel[3]", "I_O[1]", "sel", EL),
        ("sel[2]", "", "sel[2]", "I_O[0]", "sel", EL),
        ("sel[1]", "", "sel[1]", "I_I[1]", "sel", EL),
        ("sel[0]", "", "sel[0]", "I_I[0]", "sel", EL),
        ("a[0]", "I_I[0]", "out0", "I_O[0]", "in0", WG),
        ("a[1]", "I_I[0]", "out1", "I_O[1]", "in0", WG),
        ("a[2]", "I_I[1]", "out0", "I_O[0]", "in1", WG),
        ("a[3]", "I_I[1]", "out1", "I_O[1]", "in1", WG),
    ];
    let mut expected_set = BTreeSet::new();
    for (name, source, source_pin, target, target_pin, domain) in expected {
        expected_set.insert((
            name.to_string(),
            source.to_string(),
            source_pin.to_string(),
            target.to_string(),
            target_pin.to_string(),
            name_of(domain),
        ));
    }
    assert_eq!(connections, expected_set);
}

#[test]
fn vams_leaf_pin_domains() {
    let hdl_options = HdlOptions {
        pin_domains: Some(pin_domains(&[("MZM_1X2", &[("out0", WG), ("out1", WG)])])),
        ..Default::default()
    };
    let circuit = parse_vams_with(SWITCH_VAMS, &hdl_options).unwrap();
    let domains = domains_by_net(&circuit.modules[0]);
    for index in 0..4 {
        assert_eq!(domains[&format!("a[{index}]")], WG);
    }
    assert_eq!(domains["in[0]"], EL); // MZM_1X2.in0 not mapped
}

#[test]
fn vams_positional_ports_use_the_definition() {
    let text = "\
module cell (p, n);
  inout [1:0] p;
  inout n;
endmodule
module top (bus, g);
  inout [1:0] bus;
  inout g;
  cell X1 (bus, g);
endmodule
";
    let circuit = parse_vams(text).unwrap();
    let mut targets = BTreeSet::new();
    for connection in &circuit.modules[1].connections {
        if let Some(target) = &connection.target {
            targets.insert((target.instance_name.as_str(), target.port_name.as_str()));
        }
    }
    assert_eq!(
        targets,
        BTreeSet::from([("X1", "p[1]"), ("X1", "p[0]"), ("X1", "n")])
    );
}

#[test]
fn vams_width_mismatch_raises() {
    let text = "module top (a);\n  inout [2:0] a;\n  leaf L[1:0] (.x(a));\nendmodule\n";
    match parse_vams(text) {
        Err(Error::Validation(message)) => {
            assert!(message.contains("3 bits for 2 instance"), "{message}")
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// ── test_vams_parser.py: TestSimulationFixtures (650763b) ────────────────

#[test]
fn vams_override_values_are_expressions_and_references() {
    let circuit = parse_vams(
        "module t (a, b);\n  inout a, b;\n  nch #(.l(0.13 * 1e-6), .w(wmin)) M1 (.d(a), .g(b), .s(b), .b(b));\nendmodule\n",
    )
    .unwrap();
    let [length, width] = circuit.modules[0].module_references[0]
        .parameters
        .as_slice()
    else {
        panic!("two params");
    };
    let length = length
        .default_value
        .as_ref()
        .unwrap()
        .expression
        .as_ref()
        .unwrap();
    assert!(length.binary.is_some());
    let width = width
        .default_value
        .as_ref()
        .unwrap()
        .expression
        .as_ref()
        .unwrap();
    assert_eq!(width.reference.as_deref(), Some("wmin"));
}

#[test]
fn vams_a_module_without_ports_parses() {
    for header in ["module t;", "module t ();"] {
        let circuit = parse_vams(&format!(
            "{header}\n  resistor #(.r(1k)) R1 (.p(a), .n(b));\nendmodule\n"
        ))
        .unwrap();
        let names: Vec<&str> = circuit
            .modules
            .iter()
            .map(|module| module.name.as_str())
            .collect();
        assert_eq!(names, ["t"]);
    }
}

#[test]
fn vams_attributes_carry_spice_libraries_and_code() {
    let code = r#"[{"name": "rdiv", "language": "spice", "body": ".subckt rdiv a b\nR1 a b 1k\n.ends rdiv"}]"#;
    let escaped = code.replace('\\', "\\\\").replace('"', "\\\"");
    let circuit = parse_vams(&format!(
        "(* spice_lib = \"a.lib mos_ff\\nb.lib\", spice_code = \"{escaped}\" *)\nmodule t;\n  rdiv X1 (.a(x), .b(y));\nendmodule\n"
    ))
    .unwrap();
    let directives: Vec<(DirectiveKind, &str)> = circuit
        .directives
        .iter()
        .map(|directive| (directive.kind, directive.value.as_str()))
        .collect();
    assert_eq!(
        directives,
        [
            (DirectiveKind::Lib, "a.lib mos_ff"),
            (DirectiveKind::Lib, "b.lib")
        ]
    );
    let [macro_block] = circuit.macros.as_slice() else {
        panic!("one macro");
    };
    assert_eq!(
        (macro_block.name.as_str(), macro_block.language.as_str()),
        ("rdiv", "spice")
    );
    assert_eq!(macro_block.body.lines().nth(1), Some("R1 a b 1k"));
}

#[test]
fn vams_a_net_initializer_is_a_nodeset() {
    let circuit = parse_vams(
        "module t (a);\n  inout a;\n  electrical a;\n  electrical b = 5.0;\n  resistor #(.r(1k)) R1 (.p(a), .n(b));\nendmodule\n",
    )
    .unwrap();
    let node_sets = circuit.simulation.unwrap().node_sets.unwrap();
    let mut expected = IndexMap::new();
    expected.insert("V(b)".to_string(), "5".to_string());
    assert_eq!(node_sets.conditions, expected);
}

// ── test_vams_writer.py: TestNativeSimulationForms (650763b) ─────────────

#[test]
fn vams_node_sets_are_net_initializers() {
    let mut circuit = parse_vams(
        "module t (a);\n  inout a;\n  resistor #(.r(1k)) R1 (.p(a), .n(b));\nendmodule\n",
    )
    .unwrap();
    let mut conditions = IndexMap::new();
    conditions.insert("V(b)".to_string(), "5".to_string());
    circuit.simulation = Some(Simulation {
        node_sets: Some(InitialCondition {
            conditions: conditions.clone(),
        }),
        ..Default::default()
    });
    let text = write_vams(&circuit);
    assert!(text.contains("electrical b = 5;"), "{text}");
    let back = parse_vams(&text).unwrap();
    assert_eq!(
        back.simulation.unwrap().node_sets.unwrap().conditions,
        conditions
    );
}

#[test]
fn vams_a_model_instance_reads_back_as_its_primitive() {
    let circuit = parse_spice("* t\nM1 d g s b pch w=1u\n.end\n").unwrap();
    let back = parse_vams(&write_vams(&circuit)).unwrap();
    let [m1] = back.modules[0].module_references.as_slice() else {
        panic!("one instance");
    };
    assert_eq!(
        (
            m1.module_name.as_str(),
            m1.properties["model_name"].as_str()
        ),
        ("mosfet", "pch")
    );
}

// ── test_vams_preprocess.py: TestConditionals (#35) ──────────────────────

const CONDITIONALS: &str = "\
`ifdef SIM
module sim (a); endmodule
`elsif FAST
module fast (a); endmodule
`else
module plain (a); endmodule
`endif
`ifndef SIM
`define W 3
`endif
module m (a); input [`W:0] a; endmodule
";

fn budget(text: &str) -> ExpansionBudget {
    ExpansionBudget::for_text(text)
}

fn defines(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    let mut defined = IndexMap::new();
    for (name, value) in pairs {
        defined.insert(name.to_string(), value.to_string());
    }
    defined
}

#[test]
fn vams_else_branch_without_defines() {
    let text = preprocess_vams(CONDITIONALS, &defines(&[]), &mut budget(CONDITIONALS))
        .unwrap()
        .text;
    assert!(text.contains("module plain"), "{text}");
    assert!(
        !text.contains("module sim") && !text.contains("module fast"),
        "{text}"
    );
    assert!(text.contains("input [3:0] a;"), "{text}");
}

#[test]
fn vams_defines_select_branches() {
    let simulated = preprocess_vams(
        CONDITIONALS,
        &defines(&[("SIM", "1")]),
        &mut budget(CONDITIONALS),
    )
    .unwrap()
    .text;
    assert!(simulated.contains("module sim"), "{simulated}");
    let fast = preprocess_vams(
        CONDITIONALS,
        &defines(&[("FAST", "")]),
        &mut budget(CONDITIONALS),
    )
    .unwrap()
    .text;
    assert!(
        fast.contains("module fast") && !fast.contains("module plain"),
        "{fast}"
    );
}

#[test]
fn vams_nested_inactive_branch_stays_inactive() {
    let text = "`ifdef A\n`ifdef B\nx;\n`else\ny;\n`endif\n`endif\nz;\n";
    assert_eq!(
        preprocess_vams(text, &defines(&[]), &mut budget(text))
            .unwrap()
            .text,
        "z;\n"
    );
}

#[test]
fn vams_unbalanced_raises() {
    let unclosed = preprocess_vams(
        "`ifdef A\nmodule m (a); endmodule\n",
        &defines(&[]),
        &mut budget(""),
    );
    assert!(
        matches!(unclosed, Err(Error::Validation(message)) if message.contains("without `endif"))
    );
    let unopened = preprocess_vams("`else\n", &defines(&[]), &mut budget(""));
    assert!(
        matches!(unopened, Err(Error::Validation(message)) if message.contains("`else without"))
    );
}

// ── test_vams_roundtrip.py (#35) ─────────────────────────────────────────

fn top_connection_rows(circuit: &Circuit) -> BTreeSet<ConnectionTuple> {
    let mut rows = BTreeSet::new();
    for connection in &circuit.modules[0].connections {
        let source = connection.source.clone().unwrap_or_default();
        let target = connection.target.clone().unwrap_or_default();
        rows.insert((
            connection.name.clone(),
            source.instance_name,
            source.port_name,
            target.instance_name,
            target.port_name,
            name_of(connection.domain),
        ));
    }
    rows
}

#[test]
fn vams_buses_and_instance_arrays() {
    let types = [("optical", WG)];
    let first = parse_vams_with(SWITCH_VAMS, &options(&[("IS_SIM", "1")], &types)).unwrap();
    let output = write_vams(&first);
    assert!(output.contains("  input [1:0] in;\n"), "{output}");
    assert!(output.contains("  optical [1:0] in, out;\n"), "{output}");
    assert!(output.contains("  optical [3:0] a;\n"), "{output}");
    let second = parse_vams_with(&output, &options(&[], &types)).unwrap();
    assert_eq!(top_connection_rows(&first), top_connection_rows(&second));
    assert_eq!(write_vams(&second), output);
}
