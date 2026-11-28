//! Spectre-SPICE: split on `simulator lang=` and delegate to the spectre or
//! spice parser, then merge (ported from `spectre_spice/parser.py`).

use indexmap::IndexMap;
use oxymorph_schema::*;

use crate::error::Error;
use crate::formats::parse_spice;
use crate::formats::spectre::parse_spectre_section;
use crate::formats::spice::write_spice;
use crate::preprocess::preprocess_spectre_spice;

pub fn parse_spectre_spice(text: &str) -> Result<Circuit, Error> {
    parse_sections(text, "spice")
}

/// A file of `simulator lang=` sections that starts in `language`: each section is read
/// by its own parser, and the top-level elements of every section are one deck.
pub(crate) fn parse_sections(text: &str, language: &str) -> Result<Circuit, Error> {
    let sections = preprocess_spectre_spice(text, language);

    let mut all_modules: Vec<Module> = Vec::new();
    let mut all_ext_modules: IndexMap<String, ExternalModule> = IndexMap::new();
    let mut all_directives: Vec<Directive> = Vec::new();
    let mut all_analyses: Vec<Analysis> = Vec::new();
    let mut all_output_requests: Vec<OutputRequest> = Vec::new();
    let mut all_measurements: Vec<Measurement> = Vec::new();
    let mut all_temperatures: Vec<f64> = Vec::new();
    let mut ic_conditions: Properties = Properties::new();
    let mut ns_conditions: Properties = Properties::new();
    let mut all_stimuli: Vec<Stimulus> = Vec::new();

    let mut section_of: IndexMap<String, usize> = IndexMap::new();
    for (index, section) in sections.iter().enumerate() {
        if section.text.trim().is_empty() {
            continue;
        }
        let circuit = if section.language == "spectre" {
            parse_spectre_section(&section.text)?
        } else {
            parse_spice(&section.text)?
        };

        for module in &circuit.modules {
            section_of.insert(module.name.clone(), index);
        }
        for module in circuit.modules {
            // parity: kf-top-elements (one deck: the top-level elements of every section)
            let existing_top = all_modules
                .iter_mut()
                .find(|existing| existing.name == module.name);
            match existing_top {
                Some(top) if module.name == "__top__" => {
                    top.module_references.extend(module.module_references);
                    top.connections.extend(module.connections);
                }
                _ => all_modules.push(module),
            }
        }
        for ext in circuit.ext_modules {
            all_ext_modules.insert(ext.name.clone(), ext);
        }
        all_directives.extend(circuit.directives);

        if let Some(sim) = circuit.simulation {
            all_analyses.extend(sim.analyses);
            all_output_requests.extend(sim.output_requests);
            all_measurements.extend(sim.measurements);
            all_temperatures.extend(sim.temperatures);
            all_stimuli.extend(sim.stimuli);
            if let Some(ic) = sim.initial_conditions {
                ic_conditions.extend(ic.conditions);
            }
            if let Some(ns) = sim.node_sets {
                ns_conditions.extend(ns.conditions);
            }
        }
    }

    resolve_cross_section_pins(&mut all_modules, &section_of);
    let has_top = all_modules.iter().any(|module| module.name == "__top__");
    let top_module = if has_top {
        "__top__".to_string()
    } else {
        all_modules
            .last()
            .map(|m| m.name.clone())
            .unwrap_or_default()
    };

    let has_sim = !all_analyses.is_empty()
        || !all_output_requests.is_empty()
        || !all_measurements.is_empty()
        || !ic_conditions.is_empty()
        || !ns_conditions.is_empty()
        || !all_temperatures.is_empty()
        || !all_stimuli.is_empty();

    let simulation = if has_sim {
        Some(Simulation {
            analyses: all_analyses,
            output_requests: all_output_requests,
            measurements: all_measurements,
            initial_conditions: if ic_conditions.is_empty() {
                None
            } else {
                Some(InitialCondition {
                    conditions: ic_conditions,
                })
            },
            node_sets: if ns_conditions.is_empty() {
                None
            } else {
                Some(InitialCondition {
                    conditions: ns_conditions,
                })
            },
            temperatures: all_temperatures,
            stimuli: all_stimuli,
            ..Default::default()
        })
    } else {
        None
    };

    Ok(Circuit {
        name: String::new(),
        top_module,
        modules: all_modules,
        ext_modules: all_ext_modules.into_values().collect(),
        directives: all_directives,
        simulation,
        ..Default::default()
    })
}

/// Name the pins of instances whose subcircuit is defined in another section.
///
/// Each section is parsed on its own, so such instances get positional `p0`, `p1`, ...
/// pins; map them to the definition's port names.
fn resolve_cross_section_pins(modules: &mut [Module], section_of: &IndexMap<String, usize>) {
    let mut ports_of: IndexMap<String, Vec<String>> = IndexMap::new();
    for module in modules.iter() {
        let port_names = module.ports.iter().map(|port| port.name.clone()).collect();
        ports_of.insert(module.name.clone(), port_names);
    }
    for module in modules.iter_mut() {
        let mut instance_ports: IndexMap<String, &Vec<String>> = IndexMap::new();
        for reference in &module.module_references {
            let Some(port_names) = ports_of.get(&reference.module_name) else {
                continue;
            };
            if section_of[&reference.module_name] != section_of[&module.name] {
                instance_ports.insert(reference.name.clone(), port_names);
            }
        }
        for connection in &mut module.connections {
            for port_reference in [&mut connection.source, &mut connection.target]
                .into_iter()
                .flatten()
            {
                let Some(port_names) = instance_ports.get(&port_reference.instance_name) else {
                    continue;
                };
                if let Some(port_name) = positional_pin_name(&port_reference.port_name, port_names)
                {
                    port_reference.port_name = port_name.clone();
                }
            }
        }
    }
}

/// The definition's port name for a positional pin `p<N>`, if it has one.
fn positional_pin_name<'a>(pin: &str, port_names: &'a [String]) -> Option<&'a String> {
    let digits = pin.strip_prefix('p')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    port_names.get(digits.parse::<usize>().ok()?)
}

pub fn write_spectre_spice(circuit: &Circuit) -> String {
    write_spice(circuit)
}
