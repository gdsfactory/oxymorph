//! Parser fixes ported from kfnetlist #34 (1deb1e0, non-SystemVerilog parts) and #36 (0a5184b).
//! from: kfnetlist tests/morphnet/test_port_domain.py (with its SystemVerilog parametrization), test_spectre_parser.py, test_spectre_spice_parser.py

use std::collections::{BTreeMap, BTreeSet};

use oxymorph_netlist::hdl::HdlOptions;
use oxymorph_netlist::helpers;
use oxymorph_netlist::{
    parse_hspice, parse_spectre, parse_spectre_spice, parse_spice, parse_systemverilog, parse_vams,
    parse_xyce, write_spectre, Error,
};
use oxymorph_schema::{Circuit, Module, SignalDomain};

const SPICE: &str = "\
.subckt cell o1 e1 vdd out
R1 e1 vdd 1k
.ends cell
.subckt top o1 e1 vdd out
X1 o1 e1 vdd out cell
.ends top
";
const SPECTRE: &str = "\
subckt cell (o1 e1 vdd out)
R1 (e1 vdd) resistor r=1k
ends cell
subckt top (o1 e1 vdd out)
X1 (o1 e1 vdd out) cell
ends top
";
const VAMS: &str = "\
module top (o1, e1, vdd, out);
  cell X1 (.o1(o1), .e1(e1), .vdd(vdd), .out(out));
endmodule
";
const SV: &str = "\
module top (inout o1, e1, vdd, out);
  cell X1 (.o1, .e1, .vdd, .out);
endmodule
";

type Parser = fn(&str) -> Result<Circuit, Error>;

fn module_named<'a>(circuit: &'a Circuit, name: &str) -> &'a Module {
    circuit
        .modules
        .iter()
        .find(|module| module.name == name)
        .unwrap_or_else(|| panic!("no module {name}"))
}

#[test]
fn domain_of() {
    assert_eq!(helpers::domain_of("o1"), SignalDomain::Waveguide);
    assert_eq!(helpers::domain_of("o12"), SignalDomain::Waveguide);
    assert_eq!(helpers::domain_of("e1"), SignalDomain::Electrical);
    assert_eq!(helpers::domain_of("out"), SignalDomain::Electrical);
    assert_eq!(helpers::domain_of("vdd"), SignalDomain::Electrical);
}

fn parse_sv(text: &str) -> Result<Circuit, Error> {
    parse_systemverilog(text, &HdlOptions::default())
}

#[test]
fn parsers_mark_optical_terminals() {
    let expected: BTreeMap<String, SignalDomain> = [
        ("o1", SignalDomain::Waveguide),
        ("e1", SignalDomain::Electrical),
        ("vdd", SignalDomain::Electrical),
        ("out", SignalDomain::Electrical),
    ]
    .into_iter()
    .map(|(name, domain)| (name.to_string(), domain))
    .collect();

    let cases: [(&str, Parser, &str); 6] = [
        ("spice", parse_spice, SPICE),
        ("xyce", parse_xyce, SPICE),
        ("hspice", parse_hspice, SPICE),
        ("spectre", parse_spectre, SPECTRE),
        ("vams", parse_vams, VAMS),
        ("systemverilog", parse_sv, SV),
    ];
    for (format, parse, text) in cases {
        let circuit = parse(text).unwrap();
        let top = module_named(&circuit, "top");
        let port_domains: BTreeMap<String, SignalDomain> = top
            .ports
            .iter()
            .map(|port| (port.name.clone(), port.domain))
            .collect();
        let connection_domains: BTreeMap<String, SignalDomain> = top
            .connections
            .iter()
            .map(|connection| (connection.name.clone(), connection.domain))
            .collect();
        assert_eq!(port_domains, expected, "{format} ports");
        assert_eq!(connection_domains, expected, "{format} connections");
    }
}

#[test]
fn master_decides_device_not_instance_name() {
    let circuit = parse_spectre(
        "\
subckt inverter (a y)
R0 (a y) resistor r=1k
ends inverter
subckt top (a y)
I1 (a y) inverter
Xr (a y) capacitor c=1p
ends top
",
    )
    .unwrap();
    let top = &circuit.modules[1];
    let references: Vec<(&str, &str)> = top
        .module_references
        .iter()
        .map(|reference| (reference.name.as_str(), reference.module_name.as_str()))
        .collect();
    assert_eq!(references, [("I1", "inverter"), ("Xr", "capacitor")]);
    let pins: BTreeSet<(&str, &str)> = top
        .connections
        .iter()
        .filter_map(|connection| connection.target.as_ref())
        .map(|target| (target.instance_name.as_str(), target.port_name.as_str()))
        .collect();
    assert!(pins.contains(&("I1", "a")) && pins.contains(&("Xr", "p")));
}

#[test]
fn builtin_master_with_extra_nets_keeps_every_net() {
    let circuit = parse_spectre(
        "\
subckt mzi (e1 e2 arm)
Xheater (e1 e2 arm) resistor length=100u
ends mzi
",
    )
    .unwrap();
    let pins: BTreeSet<(&str, &str)> = circuit.modules[0]
        .connections
        .iter()
        .filter_map(|connection| {
            let target = connection.target.as_ref()?;
            (target.instance_name == "Xheater")
                .then_some((target.port_name.as_str(), connection.name.as_str()))
        })
        .collect();
    assert_eq!(
        pins,
        BTreeSet::from([("p0", "e1"), ("p1", "e2"), ("p2", "arm")])
    );
    assert!(write_spectre(&circuit).contains("Xheater (e1 e2 arm) resistor"));
}

#[test]
fn subckt_named_like_an_analysis_is_an_instance() {
    let circuit = parse_spectre(
        "\
subckt dc (o1 o2)
ends dc
subckt top (o1 o2)
Xdc1 (o1 o2) dc gap=200n
ends top
",
    )
    .unwrap();
    let top = &circuit.modules[1];
    let names: Vec<&str> = top
        .module_references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(names, ["Xdc1"]);
    assert_eq!(top.connections.len(), 2);
}

#[test]
fn spectre_instance_of_spice_subckt_uses_its_port_names() {
    let circuit = parse_spectre_spice(
        "\
simulator lang=spice
.subckt rdiv a b mid
R1 a mid 1k
R2 mid b 1k
.ends rdiv
simulator lang=spectre
subckt top (vin gnd)
X0 (vin gnd tap) rdiv
C0 (tap gnd) capacitor c=1p
ends top
",
    )
    .unwrap();
    let top = module_named(&circuit, "top");
    let mut pins: BTreeSet<&str> = BTreeSet::new();
    for connection in &top.connections {
        for reference in [&connection.source, &connection.target]
            .into_iter()
            .flatten()
        {
            if reference.instance_name == "X0" {
                pins.insert(reference.port_name.as_str());
            }
        }
    }
    assert_eq!(pins, BTreeSet::from(["a", "b", "mid"]));
}

// from: kfnetlist f020af6 tests/morphnet/test_spice_parser.py::test_numeric_subckt_ports
#[test]
fn numeric_subckt_ports() {
    let text = "\
.subckt cpl 1 2 5
R1 1 2 1k
.ends cpl
.subckt top a b
X1 a b 0 cpl
.ends top
.end
";
    type Parser = fn(&str) -> Result<Circuit, Error>;
    let parsers: [(&str, Parser); 3] = [
        ("spice", parse_spice),
        ("hspice", parse_hspice),
        ("xyce", parse_xyce),
    ];
    for (dialect, parse) in parsers {
        let circuit = parse(text).unwrap_or_else(|error| panic!("{dialect}: {error:?}"));
        let port_names: Vec<&str> = module_named(&circuit, "cpl")
            .ports
            .iter()
            .map(|port| port.name.as_str())
            .collect();
        assert_eq!(port_names, ["1", "2", "5"], "{dialect}");
    }
}
