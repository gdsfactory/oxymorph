//! Spectre parser + writer, ported from `spectre/transformer.py` and
//! `spectre/writer.py`.

use indexmap::IndexMap;
use oxymorph_schema::*;

use crate::ast::TreeTransform;
use crate::engine::Engine;
use crate::error::Error;
use crate::expression::{parameter_value, value_text, Dialect};
use crate::helpers::*;
use crate::preprocess::preprocess_spectre;
use crate::stimulus::{
    add_stimuli, from_spectre, name_stimulus, spectre_keys, spectre_params, StimulusIndex,
};

const GRAMMAR: &str = include_str!("../../grammars/spectre.lark");

thread_local! {
    static ENGINE: Engine = Engine::lalr(GRAMMAR).expect("spectre grammar builds");
}

fn with_engine<R>(f: impl FnOnce(&Engine) -> R) -> R {
    ENGINE.with(|e| f(e))
}

// ---------------------------------------------------------------------------
// Intermediate values (mirrors the `*Data` classes in the transformer)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct InstanceData {
    pub name: String,
    pub net_names: Vec<String>,
    pub cell_name: String,
    pub params: IndexMap<String, ParameterValue>,
    pub device_prefix: String,
}

#[derive(Clone, Debug)]
pub struct ModelData {
    pub name: String,
    pub type_name: String,
    pub params: IndexMap<String, ParameterValue>,
}

#[derive(Clone, Debug)]
pub struct SubcktData {
    pub module: Module,
    pub ext_modules: IndexMap<String, ExternalModule>,
}

#[derive(Clone, Debug)]
pub struct IncludeData {
    pub path: String,
    pub section: String,
    pub lang: String,
}

impl IncludeData {
    /// `include "p" section=s` is a library include; `lang=` goes in `name`.
    fn to_directive(&self) -> Directive {
        let name = if self.lang.is_empty() {
            String::new()
        } else {
            format!("lang={}", self.lang)
        };
        if self.section.is_empty() {
            Directive {
                kind: DirectiveKind::Include,
                name,
                value: self.path.clone(),
            }
        } else {
            Directive {
                kind: DirectiveKind::Lib,
                name,
                value: format!("{} {}", self.path, self.section),
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParamData {
    pub params: IndexMap<String, ParameterValue>,
}

#[derive(Clone, Debug)]
pub struct AnalysisData {
    pub kind: String,
    pub name: String,
    pub arguments: Vec<String>,
    pub options: IndexMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct OutputRequestData {
    pub kind: String,
    pub analysis_type: String,
    pub variables: Vec<String>,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Token(String, String),
    Tree(String, Vec<Item>),
    None,
    Circuit(Circuit),
    Str(String),
    Port(String, String),
    ParamKv(String, ParameterValue),
    IncludeOption(String, String),
    Pval(ParameterValue),
    Instance(InstanceData),
    Model(ModelData),
    Subckt(SubcktData),
    Include(IncludeData),
    Params(ParamData),
    Analysis(AnalysisData),
    OutputReq(OutputRequestData),
    List(Vec<Item>),
}

fn tok(it: &Item) -> Option<&str> {
    match it {
        Item::Token(_, v) => Some(v),
        _ => None,
    }
}

const SPECTRE_ANALYSIS_TYPES: &[&str] = &[
    "tran", "dc", "ac", "noise", "sp", "stb", "pss", "pnoise", "xf", "psp", "op",
];

// ---------------------------------------------------------------------------
// Transformer
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct SpectreTransformer {
    all_subckts: IndexMap<String, Module>,
    /// Stimuli of the sources, named and in build order.
    stimuli: Vec<Stimulus>,
}

impl TreeTransform for SpectreTransformer {
    type Item = Item;

    fn token(&mut self, t: &lark_rs::tree::Token) -> Item {
        Item::Token(t.type_.clone(), t.value.clone())
    }

    fn none(&mut self) -> Item {
        Item::None
    }

    fn default_rule(&mut self, name: &str, items: Vec<Item>) -> Item {
        Item::Tree(name.to_string(), items)
    }

    fn rule(&mut self, name: &str, items: Vec<Item>) -> Item {
        match name {
            "net_ref" => Item::Str(tok(&items[0]).unwrap_or("").to_string()),
            // parity: kf-expressions (a number, a name or a bare expression)
            "pvalue_value" => Item::Pval(parameter_value(tok(&items[0]).unwrap_or(""))),
            // parity: kf-stimuli (a quoted string, `type="pulse"`, kept as written)
            "pvalue_string" => Item::Pval(ParameterValue {
                expression: Some(Expression {
                    raw: Some(tok(&items[0]).unwrap_or("").to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            "param_assign" => {
                let name = tok(&items[0]).unwrap_or("").to_string();
                let value = match items.get(1) {
                    Some(Item::Pval(p)) => p.clone(),
                    _ => ParameterValue::default(),
                };
                Item::ParamKv(name, value)
            }
            "port_decl" => Item::Port(
                "__port__".to_string(),
                tok(&items[0]).unwrap_or("").to_string(),
            ),
            "instance_stmt" => self.instance_stmt(items),
            "model_stmt" => self.model_stmt(items),
            "parameters_stmt" => self.parameters_stmt(items),
            // parity: kf-spectre-writer-dialects (`include "p" section=s lang=l`)
            "include_opt" => Item::IncludeOption(
                tok(&items[0]).unwrap_or("").to_string(),
                tok(&items[1]).unwrap_or("").to_string(),
            ),
            "include_stmt" => {
                let path = tok(&items[0])
                    .unwrap_or("")
                    .trim_matches(|c| c == '"' || c == '\'');
                let mut include = IncludeData {
                    path: path.to_string(),
                    section: String::new(),
                    lang: String::new(),
                };
                for item in &items[1..] {
                    match item {
                        Item::IncludeOption(key, value) if key == "section" => {
                            include.section = value.clone();
                        }
                        Item::IncludeOption(key, value) if key == "lang" => {
                            include.lang = value.clone();
                        }
                        _ => {}
                    }
                }
                Item::Include(include)
            }
            "save_stmt" => {
                let variables = items
                    .iter()
                    .filter_map(|i| match i {
                        Item::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect();
                Item::OutputReq(OutputRequestData {
                    kind: "save".to_string(),
                    analysis_type: String::new(),
                    variables,
                })
            }
            "section_body" => Item::List(items.into_iter().filter(|i| !is_none(i)).collect()),
            "section_def" => items
                .into_iter()
                .find(|i| matches!(i, Item::List(_)))
                .unwrap_or(Item::List(vec![])),
            "subckt_body" => Item::List(items.into_iter().filter(|i| !is_none(i)).collect()),
            "subckt_def" => self.subckt_def(items),
            "start" => self.start(items),
            _ => Item::Tree(name.to_string(), items),
        }
    }
}

fn is_none(i: &Item) -> bool {
    matches!(i, Item::None)
}

impl SpectreTransformer {
    fn instance_stmt(&mut self, items: Vec<Item>) -> Item {
        let inst_name = tok(&items[0]).unwrap_or("").to_string();
        let device_prefix = inst_name
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_default();

        let mut net_names = Vec::new();
        let mut cell_name = String::new();
        let mut params = IndexMap::new();

        for item in &items[1..] {
            match item {
                Item::Token(_, v) => cell_name = v.clone(),
                Item::ParamKv(pname, pval) => {
                    params.insert(pname.clone(), pval.clone());
                }
                Item::Str(s) => net_names.push(s.clone()),
                _ => {}
            }
        }

        // A subckt defined in the deck named like an analysis (`dc`) is an instance
        if SPECTRE_ANALYSIS_TYPES.contains(&cell_name.as_str())
            && !self.all_subckts.contains_key(&cell_name)
        {
            let mut options = IndexMap::new();
            for (k, v) in &params {
                options.insert(k.clone(), value_text(v, Dialect::Spectre));
            }
            return Item::Analysis(AnalysisData {
                kind: cell_name,
                name: inst_name,
                arguments: vec![],
                options,
            });
        }

        Item::Instance(InstanceData {
            name: inst_name,
            net_names,
            cell_name,
            params,
            device_prefix,
        })
    }

    fn model_stmt(&mut self, items: Vec<Item>) -> Item {
        let name = tok(&items[0]).unwrap_or("").to_string();
        let type_name = tok(&items[1]).unwrap_or("").to_string();
        let mut params = IndexMap::new();
        for item in &items[2..] {
            if let Item::ParamKv(pname, pval) = item {
                params.insert(pname.clone(), pval.clone());
            }
        }
        Item::Model(ModelData {
            name,
            type_name,
            params,
        })
    }

    fn parameters_stmt(&mut self, items: Vec<Item>) -> Item {
        let mut params = IndexMap::new();
        for item in &items {
            if let Item::ParamKv(pname, pval) = item {
                params.insert(pname.clone(), pval.clone());
            }
        }
        Item::Params(ParamData { params })
    }

    fn subckt_def(&mut self, items: Vec<Item>) -> Item {
        let subckt_name = tok(&items[0]).unwrap_or("").to_string();
        let mut port_names = Vec::new();
        let mut subckt_params: IndexMap<String, ParameterValue> = IndexMap::new();
        let mut body_items: Vec<Item> = Vec::new();

        for item in &items[1..] {
            match item {
                Item::Port(_, name) => port_names.push(name.clone()),
                Item::ParamKv(pname, pval) => {
                    subckt_params.insert(pname.clone(), pval.clone());
                }
                Item::List(list) => body_items = list.clone(),
                _ => {}
            }
        }

        let result = self.build_subckt(subckt_name.clone(), port_names, subckt_params, body_items);
        self.all_subckts.insert(subckt_name, result.module.clone());
        Item::Subckt(result)
    }

    #[allow(clippy::unnecessary_unwrap)]
    fn build_subckt(
        &mut self,
        name: String,
        port_names: Vec<String>,
        subckt_params: IndexMap<String, ParameterValue>,
        body_items: Vec<Item>,
    ) -> SubcktData {
        let mut instances: Vec<InstanceData> = Vec::new();
        let mut nested_subckts: IndexMap<String, Module> = IndexMap::new();
        let mut ext_modules: IndexMap<String, ExternalModule> = IndexMap::new();
        let mut model_defs: IndexMap<String, ModelData> = IndexMap::new();
        let mut circuit_params: Vec<Parameter> = Vec::new();
        let mut directives: Vec<Directive> = Vec::new();

        for item in &body_items {
            match item {
                Item::Instance(i) => instances.push(i.clone()),
                Item::Subckt(s) => {
                    nested_subckts.insert(s.module.name.clone(), s.module.clone());
                    ext_modules.extend(s.ext_modules.clone());
                }
                Item::Model(m) => {
                    model_defs.insert(m.name.clone(), m.clone());
                }
                Item::Params(p) => {
                    for (pname, pval) in &p.params {
                        circuit_params.push(Parameter {
                            name: pname.clone(),
                            default_value: Some(pval.clone()),
                            ..Default::default()
                        });
                    }
                }
                Item::Include(inc) => directives.push(inc.to_directive()),
                Item::List(list) => {
                    for sub in list {
                        if let Item::Instance(i) = sub {
                            instances.push(i.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        for (pname, pval) in &subckt_params {
            circuit_params.push(Parameter {
                name: pname.clone(),
                default_value: Some(pval.clone()),
                ..Default::default()
            });
        }

        let ports: Vec<Port> = port_names
            .iter()
            .map(|pn| Port {
                name: pn.clone(),
                direction: PortDirection::Inout,
                // parity: kf-signal-domains
                domain: domain_of(pn),
                ..Default::default()
            })
            .collect();

        let mut net_map = NetMap::new();
        let mut module_references: Vec<ModuleReference> = Vec::new();

        for port in &ports {
            net_map.add(&port.name, "", &port.name);
        }

        for inst in &instances {
            // Spectre picks the device from the master, not the instance name: a defined
            // subckt or a built-in master (`capacitor`) wins over the SPICE-style name
            // prefix, which is only the fallback.
            let subckt_mod = nested_subckts
                .get(&inst.cell_name)
                .or_else(|| self.all_subckts.get(&inst.cell_name));
            let device_prefix =
                module_to_prefix(&inst.cell_name).unwrap_or(inst.device_prefix.as_str());
            let port_template = device_port_template(device_prefix);

            // A built-in master with more or fewer nets than its pin template (a
            // 3-terminal `resistor`) keeps every net positionally.
            if subckt_mod.is_some()
                || device_prefix == "X"
                || port_template.is_none_or(|template| template.len() != inst.net_names.len())
            {
                let inst_port_names: Vec<String> = match subckt_mod {
                    Some(m) => m.ports.iter().map(|p| p.name.clone()).collect(),
                    None => (0..inst.net_names.len()).map(|i| format!("p{i}")).collect(),
                };
                for (net_name, port_name) in inst.net_names.iter().zip(inst_port_names.iter()) {
                    net_map.add(net_name, &inst.name, port_name);
                }
                module_references.push(ModuleReference {
                    name: inst.name.clone(),
                    module_name: inst.cell_name.clone(),
                    parameters: inst
                        .params
                        .iter()
                        .map(|(n, v)| Parameter {
                            name: n.clone(),
                            default_value: Some(v.clone()),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                });
            } else {
                let template = port_template.unwrap();
                let module_name = device_prefix_to_module(device_prefix)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| device_prefix.to_lowercase());

                for (net_name, port_name) in inst.net_names.iter().zip(template.iter()) {
                    net_map.add(net_name, &inst.name, port_name);
                }

                if !ext_modules.contains_key(&module_name) {
                    let ext_ports = template
                        .iter()
                        .map(|pn| Port {
                            name: pn.to_string(),
                            direction: PortDirection::Inout,
                            // parity: kf-signal-domains
                            domain: domain_of(pn),
                            ..Default::default()
                        })
                        .collect();
                    let mut props = Properties::new();
                    props.insert("spice_prefix".to_string(), device_prefix.to_string());
                    ext_modules.insert(
                        module_name.clone(),
                        ExternalModule {
                            name: module_name.clone(),
                            ports: ext_ports,
                            properties: props,
                            ..Default::default()
                        },
                    );
                }

                let mut given = inst.params.clone();
                let mut stimulus = None;
                if device_prefix == "V" || device_prefix == "I" {
                    // parity: kf-stimuli
                    stimulus = from_spectre(&given, Dialect::Spectre);
                    for key in spectre_keys(stimulus.as_ref()) {
                        given.shift_remove(key);
                    }
                }
                let mut props = Properties::new();
                if !inst.cell_name.is_empty() && inst.cell_name != module_name {
                    props.insert("model_name".to_string(), inst.cell_name.clone());
                }
                let mut reference = ModuleReference {
                    name: inst.name.clone(),
                    module_name,
                    parameters: given
                        .iter()
                        .map(|(n, v)| Parameter {
                            name: n.clone(),
                            default_value: Some(v.clone()),
                            ..Default::default()
                        })
                        .collect(),
                    properties: props,
                    ..Default::default()
                };
                if let Some(stimulus) = stimulus {
                    let named = name_stimulus(&name, &mut reference, stimulus);
                    self.stimuli.push(named);
                }
                module_references.push(reference);
            }
        }

        for mdata in model_defs.values() {
            if !ext_modules.contains_key(&mdata.name) {
                let mut props = Properties::new();
                props.insert("model_type".to_string(), mdata.type_name.clone());
                ext_modules.insert(
                    mdata.name.clone(),
                    ExternalModule {
                        name: mdata.name.clone(),
                        parameters: mdata
                            .params
                            .iter()
                            .map(|(n, v)| Parameter {
                                name: n.clone(),
                                default_value: Some(v.clone()),
                                ..Default::default()
                            })
                            .collect(),
                        properties: props,
                        ..Default::default()
                    },
                );
            }
        }

        let connections = net_map_to_connections(&net_map);

        let module = Module {
            name,
            ports,
            parameters: circuit_params,
            module_references,
            connections,
            directives,
            ..Default::default()
        };
        SubcktData {
            module,
            ext_modules,
        }
    }

    fn start(&mut self, items: Vec<Item>) -> Item {
        let mut subckt_datas: Vec<SubcktData> = Vec::new();
        let mut model_defs: IndexMap<String, ModelData> = IndexMap::new();
        let mut top_instances: Vec<InstanceData> = Vec::new();
        let mut directives: Vec<Directive> = Vec::new();
        let mut analyses: Vec<AnalysisData> = Vec::new();
        let mut output_requests: Vec<OutputRequestData> = Vec::new();

        for item in &items {
            match item {
                Item::None => {}
                Item::Subckt(s) => subckt_datas.push(s.clone()),
                Item::Model(m) => {
                    model_defs.insert(m.name.clone(), m.clone());
                }
                Item::Instance(i) => top_instances.push(i.clone()),
                Item::Analysis(a) => analyses.push(a.clone()),
                Item::OutputReq(o) => output_requests.push(o.clone()),
                Item::Include(inc) => directives.push(inc.to_directive()),
                Item::Params(p) => {
                    for (pname, pval) in &p.params {
                        directives.push(Directive {
                            kind: DirectiveKind::Param,
                            name: pname.clone(),
                            value: value_text(pval, Dialect::Spectre),
                        });
                    }
                }
                Item::List(list) => {
                    for sub in list {
                        match sub {
                            Item::Subckt(s) => subckt_datas.push(s.clone()),
                            Item::Analysis(a) => analyses.push(a.clone()),
                            Item::OutputReq(o) => output_requests.push(o.clone()),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let mut modules: Vec<Module> = Vec::new();
        let mut all_ext_modules: IndexMap<String, ExternalModule> = IndexMap::new();

        for sd in &subckt_datas {
            modules.push(sd.module.clone());
            all_ext_modules.extend(sd.ext_modules.clone());
        }

        if !top_instances.is_empty() {
            let top_data = self.build_subckt(
                "__top__".to_string(),
                vec![],
                IndexMap::new(),
                top_instances
                    .iter()
                    .map(|i| Item::Instance(i.clone()))
                    .collect(),
            );
            modules.insert(0, top_data.module.clone());
            all_ext_modules.extend(top_data.ext_modules.clone());
        }

        for mdata in model_defs.values() {
            if !all_ext_modules.contains_key(&mdata.name) {
                let mut props = Properties::new();
                props.insert("model_type".to_string(), mdata.type_name.clone());
                all_ext_modules.insert(
                    mdata.name.clone(),
                    ExternalModule {
                        name: mdata.name.clone(),
                        parameters: mdata
                            .params
                            .iter()
                            .map(|(n, v)| Parameter {
                                name: n.clone(),
                                default_value: Some(v.clone()),
                                ..Default::default()
                            })
                            .collect(),
                        properties: props,
                        ..Default::default()
                    },
                );
            }
        }

        // parity: kf-top-elements
        let top_module = if top_instances.is_empty() {
            modules.last().map(|m| m.name.clone()).unwrap_or_default()
        } else {
            "__top__".to_string()
        };

        let simulation = if !analyses.is_empty() || !output_requests.is_empty() {
            Some(Simulation {
                analyses: analyses
                    .iter()
                    .map(|a| Analysis {
                        kind: analysis_kind(&a.kind).unwrap_or_default(),
                        name: a.name.clone(),
                        arguments: a.arguments.clone(),
                        options: a
                            .options
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    })
                    .collect(),
                output_requests: output_requests
                    .iter()
                    .map(|o| OutputRequest {
                        kind: output_request_kind(&o.kind).unwrap_or_default(),
                        analysis_type: o.analysis_type.clone(),
                        variables: o.variables.clone(),
                    })
                    .collect(),
                ..Default::default()
            })
        } else {
            None
        };

        let mut circuit = Circuit {
            name: String::new(),
            top_module,
            modules,
            ext_modules: all_ext_modules.into_values().collect(),
            directives,
            simulation,
            ..Default::default()
        };
        add_stimuli(&mut circuit, std::mem::take(&mut self.stimuli));
        Item::Circuit(circuit)
    }
}

/// Spectre netlist text; `simulator lang=spice` sections (a SPICE subcircuit, inline
/// model code) are read as SPICE when the `spectre_spice` feature is built.
pub fn parse_spectre(text: &str) -> Result<Circuit, Error> {
    #[cfg(feature = "spectre_spice")]
    if text
        .lines()
        .any(|line| crate::preprocess::simulator_lang_re().is_match(line))
    {
        // parity: kf-top-elements
        return crate::formats::spectre_spice::parse_sections(text, "spectre");
    }
    parse_spectre_section(text)
}

/// Spectre netlist text with no `simulator lang=` switch (one language section).
pub(crate) fn parse_spectre_section(text: &str) -> Result<Circuit, Error> {
    let cleaned = preprocess_spectre(text);
    let tree = with_engine(|e| e.parse(&cleaned))?;
    let mut t = SpectreTransformer::default();
    match t.transform(&tree) {
        Item::Circuit(c) => Ok(c),
        _ => Err(Error::Validation(
            "spectre transform did not produce a circuit".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

fn module_to_prefix(module: &str) -> Option<&'static str> {
    Some(match module {
        "resistor" => "R",
        "capacitor" => "C",
        "inductor" => "L",
        "diode" => "D",
        "bjt" => "Q",
        "mosfet" => "M",
        "vsource" => "V",
        "isource" => "I",
        _ => return None,
    })
}

fn analysis_kind_to_spectre(kind: AnalysisKind) -> &'static str {
    match kind {
        AnalysisKind::Op => "op",
        AnalysisKind::Dc => "dc",
        AnalysisKind::Ac => "ac",
        AnalysisKind::Tran => "tran",
        AnalysisKind::Noise => "noise",
        _ => "dc",
    }
}

/// The lines around SPICE macro text in a Spectre deck.
const SPICE_BLOCK: (&str, &str) = ("simulator lang=spice", "simulator lang=spectre");

/// The netlist-wide settings of a Spectre write: the VACASK dialect
/// (`properties["dialect"] == "vacask"`) and the net written as node `0`.
struct SpectreStyle<'a> {
    vacask: bool,
    ground: &'a str,
    /// The stimuli of the circuit being written, which its sources point to.
    stimuli: StimulusIndex<'a>,
}

impl SpectreStyle<'_> {
    /// The body lines of `macros`: Spectre takes its own language and SPICE inside
    /// `simulator lang=spice`; VACASK has no `simulator lang` switch, so it takes none.
    fn macro_lines(&self, macros: &[Macro]) -> Vec<String> {
        if self.vacask {
            return macro_lines(macros, &[], "//");
        }
        let mut accept: Vec<MacroLanguage> = vec![("spectre", None)];
        for language in SPICE_LANGUAGES {
            accept.push((language, Some(SPICE_BLOCK)));
        }
        macro_lines(macros, &accept, "//")
    }

    fn dialect(&self) -> Dialect {
        if self.vacask {
            Dialect::Vacask
        } else {
            Dialect::Spectre
        }
    }
}

pub fn write_spectre(circuit: &Circuit) -> String {
    let mut lines: Vec<String> = Vec::new();
    // parity: kf-spectre-writer-dialects
    let style = SpectreStyle {
        vacask: circuit.properties.get("dialect").map(String::as_str) == Some("vacask"),
        ground: circuit
            .properties
            .get("ground_net")
            .map(String::as_str)
            .unwrap_or(""),
        stimuli: StimulusIndex::new(circuit),
    };
    if style.vacask && !style.ground.is_empty() {
        lines.push("ground 0".to_string());
    }

    let lookups = Lookups::of(circuit);

    write_directives(&circuit.directives, &mut lines);
    lines.extend(style.macro_lines(&circuit.macros));

    for ext in &circuit.ext_modules {
        if let Some(model_type) = ext.properties.get("model_type") {
            lines.push(format!(
                "model {} {} ({})",
                ext.name,
                model_type,
                named_params(&ext.parameters, style.dialect()).join(" ")
            ));
        }
    }

    for module in &circuit.modules {
        if module.name == "__top__" {
            lines.extend(style.macro_lines(&module.macros));
            lines.extend(instance_lines(module, &lookups, &style));
        } else {
            write_subckt(module, &lookups, &style, &mut lines);
        }
    }

    if let Some(sim) = &circuit.simulation {
        for analysis in &sim.analyses {
            lines.push(format_analysis(analysis));
        }
        for req in &sim.output_requests {
            lines.push(format!("save {}", req.variables.join(" ")));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

fn write_directives(directives: &[Directive], lines: &mut Vec<String>) {
    let mut params: IndexMap<String, String> = IndexMap::new();
    for d in directives {
        match d.kind {
            // parity: kf-spectre-writer-dialects (LIB packs "path section"; name may
            // carry "lang=<dialect>")
            DirectiveKind::Include | DirectiveKind::Lib => {
                let (path, section) = d.value.rsplit_once(' ').unwrap_or(("", &d.value));
                let mut parts = if d.kind == DirectiveKind::Lib && !path.is_empty() {
                    let path = path.trim_matches(|c| c == '"' || c == '\'');
                    vec![format!("include \"{path}\""), format!("section={section}")]
                } else {
                    vec![format!("include \"{}\"", d.value)]
                };
                if d.name.starts_with("lang=") {
                    parts.push(d.name.clone());
                }
                lines.push(parts.join(" "));
            }
            DirectiveKind::Param => {
                params.insert(d.name.clone(), d.value.clone());
            }
            _ => {}
        }
    }
    if !params.is_empty() {
        let assigns = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!("parameters {assigns}"));
    }
}

fn write_subckt(module: &Module, lookups: &Lookups, style: &SpectreStyle, lines: &mut Vec<String>) {
    let port_names = module
        .ports
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>()
        .join(" ");

    let param_strs = named_params(&module.parameters, style.dialect());
    let params_suffix = if param_strs.is_empty() {
        String::new()
    } else {
        format!(" {}", param_strs.join(" "))
    };

    if style.vacask {
        // VACASK: params go on a `parameters` line in the body
        lines.push(format!("subckt {} ({port_names})", module.name));
        if !param_strs.is_empty() {
            lines.push(format!("parameters{params_suffix}"));
        }
    } else {
        lines.push(format!(
            "subckt {module_name} ({port_names}){params_suffix}",
            module_name = module.name
        ));
    }
    lines.extend(style.macro_lines(&module.macros));
    lines.extend(instance_lines(module, lookups, style));
    lines.push(format!("ends {}", module.name));
}

fn instance_lines(module: &Module, lookups: &Lookups, style: &SpectreStyle) -> Vec<String> {
    let inst_nets = connections_to_instance_nets(&module.connections, style.ground);
    let instance_pins = pins_by_instance(&inst_nets);
    let mut lines = Vec::new();
    for reference in &module.module_references {
        if !is_skipped(reference) {
            let pins = pins_of(&instance_pins, &reference.name);
            lines.push(write_instance(reference, &inst_nets, pins, lookups, style));
        }
    }
    lines
}

#[allow(clippy::unnecessary_unwrap)]
fn write_instance(
    r: &ModuleReference,
    inst_nets: &IndexMap<(String, String), String>,
    pins: &[&str],
    lookups: &Lookups,
    style: &SpectreStyle,
) -> String {
    let ext_mod = lookups.ext_by_name.get(r.module_name.as_str());
    let prefix = module_to_prefix(&r.module_name);

    // parity: kf-device-pin-template
    let complete_template = prefix
        .and_then(device_port_template)
        .filter(|template| has_every_pin(r, template, inst_nets));
    let port_order: Vec<String> = if prefix.is_some() && ext_mod.is_some() {
        ext_mod
            .unwrap()
            .ports
            .iter()
            .map(|p| p.name.clone())
            .collect()
    } else if let Some(rm) = lookups.mod_by_name.get(r.module_name.as_str()) {
        rm.ports.iter().map(|p| p.name.clone()).collect()
    } else if let Some(em) = ext_mod {
        em.ports.iter().map(|p| p.name.clone()).collect()
    } else if let Some(template) = complete_template {
        template.iter().map(|pin| pin.to_string()).collect()
    } else {
        pin_names(pins)
    };

    let mut nets = Vec::new();
    for pin in &port_order {
        match inst_nets.get(&(r.name.clone(), pin.clone())) {
            Some(net) if !net.is_empty() => nets.push(net.clone()),
            // parity: kf-one-pin-nets
            _ => nets.push(open_node(&r.name, pin)),
        }
    }
    let nets = nets.join(" ");

    let cell_name = r
        .properties
        .get("model_name")
        .cloned()
        .unwrap_or_else(|| r.module_name.clone());

    // parity: kf-spectre-writer-dialects (VACASK names the primary value of R/C/L
    // after the device)
    let value_name = match prefix {
        Some(letter @ ("R" | "C" | "L")) if style.vacask => letter.to_lowercase(),
        _ => "value".to_string(),
    };
    let (params, sources) = sim_params(r);
    let resolve = |name: &str| sources.resolve(name);
    let mut param_strs: Vec<String> = Vec::new();
    for param in params {
        if let Some(text) = param_text(param, style.dialect(), true, Some(&resolve)) {
            let name = if param.name == "value" {
                &value_name
            } else {
                &param.name
            };
            param_strs.push(format!("{name}={text}"));
        }
    }
    if let Some(stimulus) = style.stimuli.of(r) {
        // parity: kf-stimuli
        for (name, text) in spectre_params(stimulus, style.dialect()) {
            param_strs.push(format!("{name}={text}"));
        }
    }
    let params_text = if param_strs.is_empty() {
        String::new()
    } else {
        format!(" {}", param_strs.join(" "))
    };

    let instance_name = if style.vacask {
        r.name.to_lowercase()
    } else {
        r.name.clone()
    };
    format!("{instance_name} ({nets}) {cell_name}{params_text}")
}

fn has_every_pin(
    reference: &ModuleReference,
    template: &[&str],
    instance_nets: &IndexMap<(String, String), String>,
) -> bool {
    for pin in template {
        if !instance_nets.contains_key(&(reference.name.clone(), pin.to_string())) {
            return false;
        }
    }
    true
}

fn format_analysis(analysis: &Analysis) -> String {
    let keyword = analysis_kind_to_spectre(analysis.kind);
    let mut parts = vec![
        if analysis.name.is_empty() {
            keyword.to_string()
        } else {
            analysis.name.clone()
        },
        keyword.to_string(),
    ];
    for (k, v) in &analysis.options {
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}
