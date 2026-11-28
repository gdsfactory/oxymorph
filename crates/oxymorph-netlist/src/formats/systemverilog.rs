//! SystemVerilog parser + writer, ported from `systemverilog/transformer.py` and
//! `systemverilog/writer.py`.

use std::sync::OnceLock;

use indexmap::IndexMap;
use oxymorph_schema::*;
use regex::Regex;

use crate::ast::{check_tree_depth, TreeTransform};
use crate::engine::Engine;
use crate::error::Error;
use crate::expression::{
    expression_or_raw, format_expression, to_float, trim_fraction_zeros, twelve_significant_digits,
    Dialect, Resolve,
};
use crate::hdl::*;
use crate::helpers::{
    connections_to_instance_nets, domain_of, is_si_number, macro_lines, parse_parameter_number,
    pins_by_instance, pins_of, sim_params, MacroLanguage, NetMap,
};
use crate::preprocess::systemverilog::preprocess_systemverilog;
use crate::stimulus::{
    add_stimuli, from_primitive, name_stimulus, primitive, primitive_file, stimulus_keys,
    StimulusIndex,
};

const GRAMMAR: &str = include_str!("../../grammars/systemverilog.lark");

thread_local! {
    static ENGINE: Engine =
        Engine::lalr(&ascii_word_boundaries(GRAMMAR)).expect("systemverilog grammar builds");
}

/// `lark_rs` builds its lexer as a DFA, which has no Unicode `\b`: the grammar's keyword
/// boundaries become ASCII ones (its identifiers are ASCII). The grammar file stays verbatim.
fn ascii_word_boundaries(grammar: &str) -> String {
    grammar.replace(r"\b", r"(?-u:\b)")
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// A type name (`wire`, `connection`, `""`) with its packed dims.
#[derive(Clone, Debug, Default)]
struct TypeSpec {
    name: String,
    dims: Vec<Dim>,
}

#[derive(Clone, Debug)]
struct PortSpec {
    name: String,
    direction: Option<String>,
    port_type: Option<TypeSpec>,
    /// Unpacked dims.
    dims: Vec<Dim>,
}

#[derive(Clone, Debug)]
struct ParamSpec {
    name: String,
    expression: String,
    param_type: String,
    local: bool,
}

#[derive(Clone, Debug)]
struct PortDecl {
    direction: String,
    ports: Vec<PortSpec>,
}

#[derive(Clone, Debug)]
struct DataDecl {
    data_type: TypeSpec,
    assigns: Vec<(String, Vec<Dim>, Option<NetExpression>)>,
}

/// A port connection's net: unconnected (`.a()`), implicit (`.a`), or an expression.
#[derive(Clone, Debug)]
enum ConnectionValue {
    Unconnected,
    Implicit,
    Expression(NetExpression),
}

#[derive(Clone, Debug)]
enum PortConnection {
    Named(String, ConnectionValue),
    Wildcard,
    Ordered(Option<NetExpression>),
}

#[derive(Clone, Debug, Default)]
struct HierInst {
    name: String,
    dims: Vec<Dim>,
    named: IndexMap<String, ConnectionValue>,
    ordered: Vec<Option<NetExpression>>,
    wildcard: bool,
}

#[derive(Clone, Debug, Default)]
struct InstanceStmt {
    module: String,
    named_params: IndexMap<String, String>,
    ordered_params: Vec<String>,
    instances: Vec<HierInst>,
}

#[derive(Clone, Debug)]
enum ModuleItem {
    PortDecl(PortDecl),
    DataDecl(DataDecl),
    Params(Vec<ParamSpec>),
    Instance(InstanceStmt),
    Merge(Vec<NetExpression>),
    Placeholder(usize),
}

/// A module before connectivity: instances need the other modules' ports.
#[derive(Clone, Debug, Default)]
struct ModuleData {
    name: String,
    ports: Vec<PortSpec>,
    params: Vec<ParamSpec>,
    items: Vec<ModuleItem>,
}

/// Port directions, net/port types and bus ranges declared in a module.
#[derive(Clone, Debug, Default)]
struct Declarations {
    directions: IndexMap<String, String>,
    types: IndexMap<String, String>,
    ranges: Buses,
}

impl ModuleData {
    fn port_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for port in &self.ports {
            names.push(port.name.clone());
        }
        names
    }

    /// The declarations; a bare ANSI header name inherits the previous port's direction
    /// and type.
    fn declarations(&mut self, budget: &mut ExpansionBudget) -> Result<Declarations, Error> {
        let mut declarations = Declarations::default();
        let ansi = self
            .ports
            .iter()
            .any(|port| port.direction.is_some() || port.port_type.is_some());
        let mut previous_direction = "inout".to_string();
        let mut previous_type: Option<TypeSpec> = None;
        for spec in &mut self.ports {
            if ansi && spec.direction.is_none() && spec.port_type.is_none() {
                spec.direction = Some(previous_direction.clone());
                spec.port_type = previous_type.clone();
            } else if ansi && spec.direction.is_none() {
                spec.direction = Some(previous_direction.clone());
            }
            if let Some(direction) = spec.direction.clone().filter(|text| !text.is_empty()) {
                declarations
                    .directions
                    .insert(spec.name.clone(), direction.clone());
                previous_direction = direction;
                previous_type = spec.port_type.clone();
            }
            declare(
                &mut declarations,
                &spec.name,
                spec.port_type.as_ref(),
                &spec.dims,
                budget,
            )?;
        }
        for item in &self.items {
            match item {
                ModuleItem::PortDecl(port_decl) => {
                    for spec in &port_decl.ports {
                        declarations
                            .directions
                            .insert(spec.name.clone(), port_decl.direction.clone());
                        declare(
                            &mut declarations,
                            &spec.name,
                            spec.port_type.as_ref(),
                            &spec.dims,
                            budget,
                        )?;
                    }
                }
                ModuleItem::DataDecl(data_decl) => {
                    for (name, dims, _) in &data_decl.assigns {
                        let data_type = Some(&data_decl.data_type);
                        declare(&mut declarations, name, data_type, dims, budget)?;
                    }
                }
                _ => {}
            }
        }
        Ok(declarations)
    }

    fn param_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        add_public_params(&mut names, &self.params);
        for item in &self.items {
            if let ModuleItem::Params(specs) = item {
                add_public_params(&mut names, specs);
            }
        }
        names
    }
}

/// Names of the parameters an instance can override (not `localparam`).
fn add_public_params(names: &mut Vec<String>, specs: &[ParamSpec]) {
    for spec in specs {
        if !spec.local {
            names.push(spec.name.clone());
        }
    }
}

/// Record one declared name; its bits are charged to `budget`.
fn declare(
    declarations: &mut Declarations,
    name: &str,
    port_type: Option<&TypeSpec>,
    dims: &[Dim],
    budget: &mut ExpansionBudget,
) -> Result<(), Error> {
    let mut all_dims = Vec::new();
    if let Some(port_type) = port_type {
        if !port_type.name.is_empty() {
            declarations
                .types
                .entry(name.to_string())
                .or_insert_with(|| port_type.name.clone());
        }
        all_dims.extend(port_type.dims.iter().cloned());
    }
    all_dims.extend(dims.iter().cloned());
    let range = decl_range(name, &all_dims)?;
    budget.charge(range.map_or(1, range_width))?;
    if let Some(range) = range {
        declarations.ranges.entry(name.to_string()).or_insert(range);
    }
    Ok(())
}

fn port_ranges(
    module: &ModuleData,
    declarations: &Declarations,
) -> IndexMap<String, Option<Range>> {
    let mut ranges = IndexMap::new();
    for name in module.port_names() {
        let range = declarations.ranges.get(&name).copied();
        ranges.insert(name, range);
    }
    ranges
}

#[derive(Clone, Debug)]
enum Item {
    Token(String, String),
    None,
    Tree,
    Expression(NetExpression),
    Dim(Dim),
    Dims(Vec<Dim>),
    Type(TypeSpec),
    ParamAssign(String, String),
    ParamSpec(ParamSpec),
    ParamSpecs(Vec<ParamSpec>),
    HeaderParams(Vec<ParamSpec>),
    PortSpec(PortSpec),
    PortList(Vec<PortSpec>),
    PortDecl(PortDecl),
    DeclAssign(String, Vec<Dim>, Option<NetExpression>),
    DataDecl(DataDecl),
    NamedParam(String, String),
    OrderedParam(String),
    ParamValues(Vec<Item>),
    Connection(PortConnection),
    HierInst(HierInst),
    InstanceStmt(InstanceStmt),
    Merges(Vec<Vec<NetExpression>>),
    Merge(Vec<NetExpression>),
    Placeholder(usize),
    Module(ModuleData),
    Circuit(Box<Circuit>),
}

fn identifier(item: &Item) -> String {
    match item {
        Item::Token(_, value) => value.strip_prefix('\\').unwrap_or(value).to_string(),
        _ => String::new(),
    }
}

fn token_text(item: &Item) -> String {
    match item {
        Item::Token(_, value) => value.clone(),
        Item::Expression(expression) => expression.text(),
        _ => String::new(),
    }
}

fn expression_of(item: Item) -> NetExpression {
    match item {
        Item::Expression(expression) => expression,
        other => NetExpression::Text(token_text(&other)),
    }
}

fn dims_of(item: Option<&Item>) -> Vec<Dim> {
    match item {
        Some(Item::Dims(dims)) => dims.clone(),
        _ => Vec::new(),
    }
}

fn dim_text(dim: &Dim) -> String {
    format!("[{}]", dim.join(":"))
}

fn sized_number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:\d[\d_]*\s*)?'[sS]?([bBoOdDhH])\s*([0-9a-fA-F_]+)$").unwrap()
    })
}

/// Numeric value when `text` is a literal (`200e-9`, `4'b1010`), else its expression tree.
fn parameter_value(text: &str) -> Result<ParameterValue, Error> {
    if is_si_number(text) {
        return parse_parameter_number(&text.replace('_', "")).map_err(Error::Validation);
    }
    if let Some(captures) = sized_number_re().captures(text) {
        let radix = match captures[1].to_ascii_lowercase().as_str() {
            "b" => 2,
            "o" => 8,
            "h" => 16,
            _ => 10,
        };
        let digits = captures[2].replace('_', "");
        let value = u128::from_str_radix(&digits, radix).map_err(|_| {
            Error::Validation(format!("invalid literal for base {radix}: '{digits}'"))
        })?;
        return parse_parameter_number(&value.to_string()).map_err(Error::Validation);
    }
    Ok(ParameterValue {
        expression: Some(expression_or_raw(text)),
        ..Default::default()
    })
}

fn direction_of(text: &str) -> PortDirection {
    match text {
        "input" => PortDirection::Input,
        "output" => PortDirection::Output,
        _ => PortDirection::Inout,
    }
}

struct SystemVerilogTransformer<'a> {
    blocks: Vec<String>,
    options: &'a HdlOptions,
    budget: ExpansionBudget,
    stimuli: Vec<Stimulus>,
    error: Option<Error>,
}

impl SystemVerilogTransformer<'_> {
    fn fail(&mut self, error: Error) -> Item {
        self.error.get_or_insert(error);
        Item::None
    }

    fn param_specs(items: Vec<Item>, local_default: bool) -> Vec<ParamSpec> {
        let mut local = local_default;
        let mut param_type = String::new();
        let mut specs = Vec::new();
        for item in items {
            match item {
                Item::Token(kind, value) if kind == "PARAM_KW" => local = value == "localparam",
                Item::ParamAssign(name, expression) => specs.push(ParamSpec {
                    name,
                    expression,
                    param_type: param_type.clone(),
                    local,
                }),
                Item::Type(type_spec) => param_type = type_spec.name,
                _ => {}
            }
        }
        specs
    }

    fn hier_inst(items: Vec<Item>) -> Item {
        let mut items = items.into_iter();
        let name = items
            .next()
            .map(|item| identifier(&item))
            .unwrap_or_default();
        let dims = dims_of(items.next().as_ref());
        let mut instance = HierInst {
            name,
            dims,
            ..Default::default()
        };
        for item in items {
            match item {
                Item::Connection(PortConnection::Wildcard) => instance.wildcard = true,
                Item::Connection(PortConnection::Named(port, value)) => {
                    instance.named.insert(port, value);
                }
                Item::Connection(PortConnection::Ordered(expression)) => {
                    instance.ordered.push(expression)
                }
                _ => {}
            }
        }
        if instance.ordered.len() == 1 && instance.ordered[0].is_none() {
            instance.ordered.clear(); // `X1 ()`
        }
        Item::HierInst(instance)
    }

    fn instance_stmt(items: Vec<Item>) -> Item {
        let mut statement = InstanceStmt {
            module: identifier(&items[0]),
            ..Default::default()
        };
        for item in items.into_iter().skip(1) {
            match item {
                Item::HierInst(instance) => statement.instances.push(instance),
                Item::ParamValues(arguments) => {
                    for argument in arguments {
                        match argument {
                            Item::NamedParam(name, value) => {
                                statement.named_params.insert(name, value);
                            }
                            Item::OrderedParam(value) => statement.ordered_params.push(value),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Item::InstanceStmt(statement)
    }

    fn module_def(items: Vec<Item>) -> Item {
        let mut module = ModuleData {
            name: identifier(&items[0]),
            ..Default::default()
        };
        for item in items.into_iter().skip(1) {
            match item {
                Item::HeaderParams(specs) => module.params = specs,
                Item::ParamSpecs(specs) => module.items.push(ModuleItem::Params(specs)),
                Item::PortList(ports) => module.ports = ports,
                Item::Merges(merges) => {
                    for merge in merges {
                        module.items.push(ModuleItem::Merge(merge));
                    }
                }
                Item::Merge(merge) => module.items.push(ModuleItem::Merge(merge)),
                Item::PortDecl(port_decl) => module.items.push(ModuleItem::PortDecl(port_decl)),
                Item::DataDecl(data_decl) => module.items.push(ModuleItem::DataDecl(data_decl)),
                Item::InstanceStmt(statement) => module.items.push(ModuleItem::Instance(statement)),
                Item::Placeholder(index) => module.items.push(ModuleItem::Placeholder(index)),
                _ => {}
            }
        }
        Item::Module(module)
    }

    fn start(&mut self, items: Vec<Item>) -> Result<Circuit, Error> {
        let mut modules: IndexMap<String, ModuleData> = IndexMap::new();
        let mut properties = Properties::new();
        for item in items {
            match item {
                Item::Module(module) => {
                    modules.insert(module.name.clone(), module);
                }
                Item::Placeholder(index) if index < self.blocks.len() => {
                    properties.insert(
                        format!("behavioral_block_{index}"),
                        self.blocks[index].clone(),
                    );
                }
                _ => {}
            }
        }
        let mut declarations: IndexMap<String, Declarations> = IndexMap::new();
        for (name, module) in &mut modules {
            check_unique_ports(name, &module.port_names())?;
            declarations.insert(name.clone(), module.declarations(&mut self.budget)?);
        }
        let mut built = Vec::new();
        for module in modules.values() {
            built.push(self.build_module(module, &modules, &declarations)?);
        }
        let mut used = std::collections::HashSet::new();
        for module in &built {
            for reference in &module.module_references {
                used.insert(reference.module_name.clone());
            }
        }
        let mut tops: Vec<&str> = Vec::new();
        for module in &built {
            if !used.contains(&module.name) {
                tops.push(&module.name);
            }
        }
        if tops.is_empty() {
            tops = built.iter().map(|module| module.name.as_str()).collect();
        }
        let top_module = tops.last().map(|name| name.to_string()).unwrap_or_default();
        let mut circuit = Circuit {
            top_module,
            modules: built,
            properties,
            ..Default::default()
        };
        add_stimuli(&mut circuit, std::mem::take(&mut self.stimuli));
        propagate_domains(
            &mut circuit,
            &self.options.disciplines,
            self.options.pin_domains.as_ref(),
        );
        Ok(circuit)
    }

    fn build_module(
        &mut self,
        module: &ModuleData,
        modules: &IndexMap<String, ModuleData>,
        declarations: &IndexMap<String, Declarations>,
    ) -> Result<Module, Error> {
        let declared = &declarations[&module.name];
        let buses = &declared.ranges;

        let mut parameters = Vec::new();
        for spec in &module.params {
            parameters.push(parameter(spec)?);
        }
        let mut properties = Properties::new();
        let mut merges: Vec<Vec<NetExpression>> = Vec::new();
        let mut instances: Vec<&InstanceStmt> = Vec::new();
        for item in &module.items {
            match item {
                ModuleItem::DataDecl(data_decl) => {
                    for (name, _, right_side) in &data_decl.assigns {
                        if let Some(
                            right_side @ (NetExpression::Net { .. } | NetExpression::Concat(_)),
                        ) = right_side
                        {
                            merges.push(vec![NetExpression::net(name), right_side.clone()]);
                        }
                    }
                }
                ModuleItem::Params(specs) => {
                    for spec in specs {
                        parameters.push(parameter(spec)?);
                    }
                }
                ModuleItem::Merge(nets) => merges.push(nets.clone()),
                ModuleItem::Instance(statement) => instances.push(statement),
                ModuleItem::Placeholder(index) if *index < self.blocks.len() => {
                    properties.insert(
                        format!("behavioral_block_{index}"),
                        self.blocks[*index].clone(),
                    );
                }
                _ => {}
            }
        }

        let mut ports = Vec::new();
        let mut port_bits: IndexMap<String, Vec<String>> = IndexMap::new();
        for name in module.port_names() {
            let range = buses.get(&name).copied();
            let pins = match range {
                Some(range) => bits(&name, range)?,
                None => vec![name.clone()],
            };
            port_bits.insert(name.clone(), pins);
            let mut port_properties = Properties::new();
            if let Some((msb, lsb)) = range {
                port_properties.insert("range".to_string(), format!("{msb}:{lsb}"));
            }
            let direction = declared
                .directions
                .get(&name)
                .map_or("inout", String::as_str);
            ports.push(Port {
                direction: direction_of(direction),
                domain: domain_of(&name),
                discipline: declared.types.get(&name).cloned().unwrap_or_default(),
                width: range.map_or(0, |range| range_width(range) as u32),
                properties: port_properties,
                name,
            });
        }
        let mut pins = std::collections::HashSet::new();
        for bits_of_port in port_bits.values() {
            for pin in bits_of_port {
                pins.insert(pin.clone());
            }
        }

        // nets (bit level): assign/alias merge nets; a module port names its net
        let mut parent: IndexMap<String, String> = IndexMap::new();
        for group in &merges {
            let mut sides = Vec::new();
            for expression in group {
                sides.push(resolve(expression, buses, 1, &mut self.budget)?);
            }
            if sides.iter().any(|side| side.len() != sides[0].len()) {
                let texts: Vec<String> = group.iter().map(NetExpression::text).collect();
                return Err(Error::Validation(format!(
                    "{}: width mismatch in {}",
                    module.name,
                    texts.join(" = ")
                )));
            }
            for side in &sides[1..] {
                for (left, right) in sides[0].iter().zip(side) {
                    let left_root = find(&parent, left);
                    let right_root = find(&parent, right);
                    if left_root != right_root {
                        let (keep, drop) =
                            if pins.contains(&right_root) && !pins.contains(&left_root) {
                                (right_root, left_root)
                            } else {
                                (left_root, right_root)
                            };
                        parent.insert(drop, keep);
                    }
                }
            }
        }

        let mut net_map = NetMap::new();
        for bits_of_port in port_bits.values() {
            for pin in bits_of_port {
                net_map.add(&find(&parent, pin), "", pin);
            }
        }

        let mut module_references = Vec::new();
        for statement in instances {
            let target = modules.get(&statement.module);
            let param_names = target.map(ModuleData::param_names).unwrap_or_default();
            let target_ranges =
                target.map(|target| port_ranges(target, &declarations[&target.name]));
            let mut params = Vec::new();
            for (name, text) in &statement.named_params {
                params.push(Parameter {
                    name: name.clone(),
                    default_value: Some(parameter_value(text)?),
                    ..Default::default()
                });
            }
            for (index, text) in statement.ordered_params.iter().enumerate() {
                let name = param_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("p{index}"));
                params.push(Parameter {
                    name,
                    default_value: Some(parameter_value(text)?),
                    ..Default::default()
                });
            }
            for instance in &statement.instances {
                let range = decl_range(&instance.name, &instance.dims)?;
                if !instance.dims.is_empty() && range.is_none() {
                    return Err(Error::Validation(format!(
                        "{}{}: instance array range must be constant",
                        instance.name,
                        dim_text(&instance.dims[0])
                    )));
                }
                let count = range.map_or(1, range_width);
                let budget = &mut self.budget;
                charge_instances(count, target_ranges.as_ref(), params.len(), budget)?;
                let names = instance_names(&instance.name, range)?;
                for (port, expression) in connections(instance, &statement.module, target)? {
                    let ranges = target_ranges.as_ref();
                    let triples = connect(&names, &port, &expression, buses, ranges, budget)?;
                    for (name, pin, net) in triples {
                        net_map.add(&find(&parent, &net), &name, &pin);
                    }
                }
                let mut given = IndexMap::new();
                for param in &params {
                    if let Some(value) = &param.default_value {
                        given.insert(param.name.clone(), value.clone());
                    }
                }
                let mut file = None;
                let master = statement.module.to_lowercase();
                if master == "vpwl" || master == "ipwl" {
                    // see the preprocessor's `pwl_file` attribute
                    if let Some(value) = given.shift_remove("pwl_file") {
                        if let Some(expression) = value.expression {
                            let raw = expression.raw.unwrap_or_default();
                            file = Some(raw.trim_matches('"').to_string());
                        }
                    }
                    params.retain(|param| param.name != "pwl_file");
                }
                // a Verilog-AMS LRM Annex E source primitive
                if let Some(stimulus) = from_primitive(&statement.module, &given, file.as_deref()) {
                    let keys = stimulus_keys(Some(&stimulus));
                    let current = master.starts_with('i');
                    let mut rest = Vec::new();
                    for param in &params {
                        if !keys.contains(&param.name.as_str()) {
                            rest.push(param.clone());
                        }
                    }
                    for name in &names {
                        let mut reference = ModuleReference {
                            name: name.clone(),
                            module_name: if current { "isource" } else { "vsource" }.to_string(),
                            parameters: rest.clone(),
                            ..Default::default()
                        };
                        let named = name_stimulus(&module.name, &mut reference, stimulus.clone());
                        self.stimuli.push(named);
                        module_references.push(reference);
                    }
                    continue;
                }
                for name in &names {
                    module_references.push(ModuleReference {
                        name: name.clone(),
                        module_name: statement.module.clone(),
                        parameters: params.clone(),
                        ..Default::default()
                    });
                }
            }
        }

        Ok(Module {
            name: module.name.clone(),
            ports,
            parameters,
            module_references,
            connections: typed_connections(&net_map, &declared.types),
            properties,
            ..Default::default()
        })
    }
}

fn find(parent: &IndexMap<String, String>, net: &str) -> String {
    let mut current = net;
    while let Some(next) = parent.get(current) {
        if next == current {
            break;
        }
        current = next;
    }
    current.to_string()
}

fn parameter(spec: &ParamSpec) -> Result<Parameter, Error> {
    let mut properties = Properties::new();
    if spec.local {
        properties.insert("localparam".to_string(), "true".to_string());
    }
    if !spec.param_type.is_empty() {
        properties.insert("type".to_string(), spec.param_type.clone());
    }
    Ok(Parameter {
        name: spec.name.clone(),
        default_value: Some(parameter_value(&spec.expression)?),
        properties,
        ..Default::default()
    })
}

/// `(instance port, net expression)` pairs for one instance.
fn connections(
    instance: &HierInst,
    module: &str,
    target: Option<&ModuleData>,
) -> Result<Vec<(String, NetExpression)>, Error> {
    let target_ports = target.map(ModuleData::port_names).unwrap_or_default();
    let mut pending: Vec<(String, ConnectionValue)> = Vec::new();
    for (port, value) in &instance.named {
        pending.push((port.clone(), value.clone()));
    }
    for (index, expression) in instance.ordered.iter().enumerate() {
        let port = target_ports
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("p{index}"));
        let value = match expression {
            Some(expression) => ConnectionValue::Expression(expression.clone()),
            None => ConnectionValue::Unconnected,
        };
        pending.push((port, value));
    }
    if instance.wildcard {
        if target.is_none() {
            return Err(Error::Validation(format!(
                "{}: `.*` needs the definition of module '{module}'",
                instance.name
            )));
        }
        for port in &target_ports {
            if !instance.named.contains_key(port) {
                pending.push((port.clone(), ConnectionValue::Implicit));
            }
        }
    }
    let mut pairs = Vec::new();
    for (port, value) in pending {
        let expression = match value {
            ConnectionValue::Unconnected => continue,
            ConnectionValue::Implicit => NetExpression::net(&port),
            ConnectionValue::Expression(NetExpression::Text(text)) => {
                return Err(Error::Validation(format!(
                    "{}.{port}: unsupported port expression '{text}'",
                    instance.name
                )))
            }
            ConnectionValue::Expression(expression) => expression,
        };
        pairs.push((port, expression));
    }
    Ok(pairs)
}

impl TreeTransform for SystemVerilogTransformer<'_> {
    type Item = Item;

    fn token(&mut self, token: &lark_rs::tree::Token) -> Item {
        Item::Token(token.type_.clone(), token.value.clone())
    }

    fn none(&mut self) -> Item {
        Item::None
    }

    fn default_rule(&mut self, _name: &str, _items: Vec<Item>) -> Item {
        Item::Tree
    }

    fn rule(&mut self, name: &str, items: Vec<Item>) -> Item {
        if self.error.is_some() {
            return Item::None;
        }
        match name {
            // ── expressions
            "number" => Item::Expression(NetExpression::Literal(token_text(&items[0]))),
            "sized_number" => {
                let text: String = token_text(&items[0])
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect();
                Item::Expression(NetExpression::Literal(text))
            }
            "string" => Item::Expression(NetExpression::Text(token_text(&items[0]))),
            "binop" => {
                let mut text = String::new();
                for item in &items {
                    text.push_str(&token_text(item));
                }
                Item::Expression(NetExpression::Text(text))
            }
            "unop" => Item::Expression(NetExpression::Text(format!(
                "{}{}",
                token_text(&items[0]),
                token_text(&items[1])
            ))),
            "paren" => {
                Item::Expression(NetExpression::Text(format!("({})", token_text(&items[0]))))
            }
            "assign_pattern" => {
                let texts: Vec<String> = items.iter().map(token_text).collect();
                Item::Expression(NetExpression::Text(format!("'{{{}}}", texts.join(", "))))
            }
            "concat" => Item::Expression(NetExpression::Concat(
                items.into_iter().map(expression_of).collect(),
            )),
            "replicate" => {
                let mut items = items.into_iter();
                let count = items
                    .next()
                    .map(|item| token_text(&item))
                    .unwrap_or_default();
                match replicate(&count, items.map(expression_of).collect(), &mut self.budget) {
                    Ok(expression) => Item::Expression(expression),
                    Err(error) => self.fail(error),
                }
            }
            "range" => Item::Dim(items.iter().map(token_text).collect()),
            "dims" => {
                let mut dims = Vec::new();
                for item in items {
                    if let Item::Dim(dim) = item {
                        dims.push(dim);
                    }
                }
                Item::Dims(dims)
            }
            "net_ref" => {
                let name = identifier(&items[0]);
                let mut selects = Vec::new();
                for item in items.into_iter().skip(1) {
                    if let Item::Dim(dim) = item {
                        selects.push(dim);
                    }
                }
                if selects.len() > 1 {
                    let texts: Vec<String> = selects.iter().map(dim_text).collect();
                    return self.fail(Error::Validation(format!(
                        "{name}{}: multi-dimensional selects are not supported",
                        texts.concat()
                    )));
                }
                Item::Expression(NetExpression::Net {
                    name,
                    select: selects.pop().unwrap_or_default(),
                })
            }
            // ── types
            "builtin_type" | "implicit_type" => {
                let type_name = if name == "builtin_type" {
                    token_text(&items[0])
                } else {
                    String::new()
                };
                let mut dims = Vec::new();
                for item in items {
                    if let Item::Dim(dim) = item {
                        dims.push(dim);
                    }
                }
                Item::Type(TypeSpec {
                    name: type_name,
                    dims,
                })
            }
            "user_type" => Item::Type(TypeSpec {
                name: identifier(&items[0]),
                dims: Vec::new(),
            }),
            // ── parameters
            "param_assign" => {
                Item::ParamAssign(identifier(&items[0]), token_text(&items[items.len() - 1]))
            }
            "param_port" => match Self::param_specs(items, false).into_iter().next() {
                Some(spec) => Item::ParamSpec(spec),
                None => Item::None,
            },
            "param_port_list" => {
                let mut specs = Vec::new();
                for item in items {
                    if let Item::ParamSpec(spec) = item {
                        specs.push(spec);
                    }
                }
                // `#(parameter real a = 1, b = 2)`: b inherits `real` from a
                for index in 1..specs.len() {
                    if specs[index].param_type.is_empty() {
                        specs[index].param_type = specs[index - 1].param_type.clone();
                    }
                }
                Item::HeaderParams(specs)
            }
            "param_decl" => Item::ParamSpecs(Self::param_specs(items, false)),
            // ── ports
            "port_name" => {
                if items.len() == 4 {
                    // user type first: `connection [3:0] a [1:0]`
                    let port_type = TypeSpec {
                        name: identifier(&items[0]),
                        dims: dims_of(items.get(1)),
                    };
                    Item::PortSpec(PortSpec {
                        name: identifier(&items[2]),
                        direction: None,
                        port_type: Some(port_type),
                        dims: dims_of(items.get(3)),
                    })
                } else {
                    Item::PortSpec(PortSpec {
                        name: identifier(&items[0]),
                        direction: None,
                        port_type: None,
                        dims: dims_of(items.get(1)),
                    })
                }
            }
            "ansi_port" => {
                let mut direction = None;
                let mut port_type = None;
                let mut spec = None;
                for item in items {
                    match item {
                        Item::Token(kind, value) if kind == "PORT_DIR" => direction = Some(value),
                        Item::Type(type_spec) => port_type = Some(type_spec),
                        Item::PortSpec(port_spec) => spec = Some(port_spec),
                        _ => {}
                    }
                }
                let Some(mut spec) = spec else {
                    return Item::None;
                };
                if direction.is_some() {
                    spec.direction = direction;
                }
                if port_type.is_some() {
                    spec.port_type = port_type;
                }
                Item::PortSpec(spec)
            }
            "port_list" => {
                let mut specs = Vec::new();
                for item in items {
                    if let Item::PortSpec(spec) = item {
                        specs.push(spec);
                    }
                }
                Item::PortList(specs)
            }
            "port_decl" => {
                let direction = token_text(&items[0]);
                // `input [3:0] a, b;` / `input connection a, b;`: names share the first type
                let mut port_type: Option<TypeSpec> = None;
                for item in &items[1..] {
                    if let Item::Type(type_spec) = item {
                        port_type = Some(type_spec.clone());
                        break;
                    }
                }
                let mut ports = Vec::new();
                for item in items.into_iter().skip(1) {
                    if let Item::PortSpec(mut spec) = item {
                        if spec.port_type.is_none() {
                            spec.port_type = port_type.clone();
                        }
                        port_type = spec.port_type.clone();
                        ports.push(spec);
                    }
                }
                Item::PortDecl(PortDecl { direction, ports })
            }
            // ── net / variable declarations
            "decl_assign" => {
                let mut items = items.into_iter();
                let name = items
                    .next()
                    .map(|item| identifier(&item))
                    .unwrap_or_default();
                let dims = dims_of(items.next().as_ref());
                Item::DeclAssign(name, dims, items.next().map(expression_of))
            }
            "data_decl" | "user_decl" => {
                let mut data_type = TypeSpec::default();
                let mut assigns = Vec::new();
                for (index, item) in items.into_iter().enumerate() {
                    match item {
                        Item::Type(type_spec) if index == 0 => data_type = type_spec,
                        Item::Token(_, _) if index == 0 => data_type.name = identifier(&item),
                        Item::Dim(dim) => data_type.dims.push(dim),
                        Item::DeclAssign(name, dims, value) => assigns.push((name, dims, value)),
                        _ => {}
                    }
                }
                Item::DataDecl(DataDecl { data_type, assigns })
            }
            // ── instances
            "named_param" => {
                let value = items.get(1).map(token_text).unwrap_or_default();
                Item::NamedParam(identifier(&items[0]), value)
            }
            "ordered_param" => Item::OrderedParam(token_text(&items[0])),
            "param_values" => Item::ParamValues(items),
            "named_conn" => {
                let port = identifier(&items[0]);
                let value = match items.into_iter().nth(1) {
                    Some(item) => ConnectionValue::Expression(expression_of(item)),
                    None => ConnectionValue::Unconnected,
                };
                Item::Connection(PortConnection::Named(port, value))
            }
            "implicit_conn" => Item::Connection(PortConnection::Named(
                identifier(&items[0]),
                ConnectionValue::Implicit,
            )),
            "wildcard_conn" => Item::Connection(PortConnection::Wildcard),
            "ordered_conn" => Item::Connection(PortConnection::Ordered(
                items.into_iter().next().map(expression_of),
            )),
            "hier_inst" => Self::hier_inst(items),
            "instance_stmt" => Self::instance_stmt(items),
            // ── assign / alias / placeholders
            "assign_stmt" => {
                let expressions: Vec<NetExpression> =
                    items.into_iter().map(expression_of).collect();
                let mut merges = Vec::new();
                for pair in expressions.as_chunks::<2>().0 {
                    merges.push(pair.to_vec());
                }
                Item::Merges(merges)
            }
            "alias_stmt" => Item::Merge(items.into_iter().map(expression_of).collect()),
            "placeholder" => match token_text(&items[0]).parse() {
                Ok(index) => Item::Placeholder(index),
                Err(_) => Item::None,
            },
            "end_label" => Item::None,
            "module_def" => Self::module_def(items),
            "start" => match self.start(items) {
                Ok(circuit) => Item::Circuit(Box::new(circuit)),
                Err(error) => self.fail(error),
            },
            _ => Item::Tree,
        }
    }
}

/// Parse structural SystemVerilog text into a circuit.
///
/// Nets are bit level: a bus port `input [3:0] in` is one `Port` with `width=4` whose
/// bits `in[3]` … `in[0]` are the net members, so a bit-select `.a(in[0])` meets the
/// port. Instance arrays `X I[3:0] (...)` expand to instances `I[3]` … `I[0]`. Width
/// mismatches and selects of undeclared buses are `Error::Validation`.
pub fn parse_systemverilog(text: &str, options: &HdlOptions) -> Result<Circuit, Error> {
    let mut budget = ExpansionBudget::for_text(text);
    let preprocessed = preprocess_systemverilog(text, &options.defines, &mut budget)?;
    let directives = directives_of(&preprocessed.directives);
    let tree = ENGINE.with(|engine| engine.parse(&preprocessed.text))?;
    check_tree_depth(&tree)?;
    let mut transformer = SystemVerilogTransformer {
        blocks: preprocessed.blocks,
        options,
        budget,
        stimuli: Vec::new(),
        error: None,
    };
    let result = transformer.transform(&tree);
    if let Some(error) = transformer.error {
        return Err(error);
    }
    match result {
        Item::Circuit(circuit) => Ok(Circuit {
            directives,
            ..*circuit
        }),
        _ => Err(Error::Validation(
            "systemverilog transform did not produce a circuit".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Python's exponent form: `2.5e-07`, `1e+16`.
fn python_exponent(mantissa: &str, exponent: i32) -> String {
    let sign = if exponent < 0 { '-' } else { '+' };
    format!(
        "{}e{sign}{:02}",
        trim_fraction_zeros(mantissa),
        exponent.abs()
    )
}

/// Plain number (SI suffixes are not SystemVerilog), else the expression text.
fn format_param_value(parameter: &Parameter, resolve: Option<Resolve>) -> String {
    let Some(value) = &parameter.default_value else {
        return "0".to_string();
    };
    if let Some(prefixed_value) = &value.prefixed_value {
        return twelve_significant_digits(to_float(prefixed_value), python_exponent);
    }
    if let Some(expression) = &value.expression {
        let text = format_expression(expression, Dialect::SystemVerilog, resolve, false);
        if !text.is_empty() {
            return text;
        }
    }
    "0".to_string()
}

fn port_range(port: &Port) -> Option<Range> {
    declared_range(port.width, port.properties.get("range").map(String::as_str))
}

/// `wire [3:0] a` for builtin types, `connection a [3:0]` for user types.
fn declaration(type_name: &str, name: &str, range: Option<Range>) -> String {
    let escaped = sv_name(name);
    let Some((msb, lsb)) = range else {
        return join_present(&[type_name, &escaped]);
    };
    let dim = format!("[{msb}:{lsb}]");
    if !type_name.is_empty() && !is_builtin_type(type_name) {
        return format!("{type_name} {escaped} {dim}");
    }
    join_present(&[type_name, &dim, &escaped])
}

fn join_present(parts: &[&str]) -> String {
    let present: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !part.is_empty())
        .collect();
    present.join(" ")
}

fn param_decl(parameter: &Parameter, keyword: &str) -> String {
    let type_text = match parameter.properties.get("type") {
        Some(param_type) if !param_type.is_empty() => format!(" {param_type}"),
        _ => String::new(),
    };
    format!(
        "{keyword}{type_text} {} = {}",
        sv_name(&parameter.name),
        format_param_value(parameter, None)
    )
}

fn is_local(parameter: &Parameter) -> bool {
    parameter.properties.get("localparam").map(String::as_str) == Some("true")
}

fn behavioral_blocks(properties: &Properties) -> Vec<&String> {
    let mut blocks = Vec::new();
    for (key, value) in properties {
        if key.starts_with("behavioral_block_") {
            blocks.push(value);
        }
    }
    blocks
}

/// The macro languages SystemVerilog takes as body text.
const MACRO_LANGUAGES: [MacroLanguage; 2] = [("systemverilog", None), ("verilog", None)];

/// Convert a circuit to structural SystemVerilog text.
pub fn write_systemverilog(circuit: &Circuit) -> String {
    let masters = master_ports(circuit);
    let stimuli = StimulusIndex::new(circuit);
    let mut lines: Vec<String> = Vec::new();
    write_directives(&circuit.directives, &mut lines);
    lines.extend(macro_lines(&circuit.macros, &MACRO_LANGUAGES, "//"));
    for block in behavioral_blocks(&circuit.properties) {
        lines.push(block.clone());
    }
    for module in &circuit.modules {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        write_module(module, &masters, &stimuli, &mut lines);
    }
    lines.push(String::new());
    lines.join("\n")
}

fn write_module(
    module: &Module,
    masters: &MasterPorts,
    stimuli: &StimulusIndex,
    lines: &mut Vec<String>,
) {
    let mut header_params = Vec::new();
    for parameter in &module.parameters {
        if !is_local(parameter) {
            header_params.push(param_decl(parameter, "parameter"));
        }
    }
    let params = if header_params.is_empty() {
        String::new()
    } else {
        format!(" #({})", header_params.join(", "))
    };
    let mut port_lines = Vec::new();
    for port in &module.ports {
        let direction = match port.direction {
            PortDirection::Input => "input",
            PortDirection::Output => "output",
            PortDirection::Inout => "inout",
        };
        port_lines.push(format!(
            "{direction} {}",
            declaration(&port.discipline, &port.name, port_range(port))
        ));
    }
    let ports = if port_lines.is_empty() {
        "()".to_string()
    } else {
        format!("(\n  {}\n)", port_lines.join(",\n  "))
    };
    lines.push(format!("module {}{params} {ports};", sv_name(&module.name)));
    lines.extend(macro_lines(&module.macros, &MACRO_LANGUAGES, "//"));

    for parameter in &module.parameters {
        if is_local(parameter) {
            lines.push(format!("  {};", param_decl(parameter, "localparam")));
        }
    }

    // buses: declared port ranges, then internal `name[i]` nets (declared here)
    let mut port_buses = Buses::new();
    let mut port_names = std::collections::HashSet::new();
    for port in &module.ports {
        port_names.insert(port.name.as_str());
        if let Some(range) = port_range(port) {
            port_buses.insert(port.name.clone(), range);
        }
    }
    let mut net_types: IndexMap<&str, &str> = IndexMap::new();
    let mut internal_names = Vec::new();
    for connection in &module.connections {
        let declared = connection
            .properties
            .get("discipline")
            .map_or("", String::as_str);
        net_types.insert(bus_base(&connection.name), declared);
        // a scalar port literally named `in[3]` (from kfnetlist) is not a bus bit
        if !port_names.contains(connection.name.as_str()) {
            internal_names.push(connection.name.as_str());
        }
    }
    let internal = net_buses(&internal_names);
    let mut buses = Buses::new();
    for (base, range) in &internal {
        if !port_names.contains(base.as_str()) {
            buses.insert(base.clone(), *range);
        }
    }
    for (name, range) in port_buses {
        buses.insert(name, range);
    }
    for (net, net_type) in &net_types {
        if port_names.contains(net) || (!internal.contains_key(*net) && net_type.is_empty()) {
            continue;
        }
        let type_name = if net_type.is_empty() {
            "wire"
        } else {
            net_type
        };
        lines.push(format!(
            "  {};",
            declaration(type_name, net, internal.get(*net).copied())
        ));
    }

    let instance_nets = connections_to_instance_nets(&module.connections, "");
    let instance_pins = pins_by_instance(&instance_nets);
    for reference in &module.module_references {
        let pins = pins_of(&instance_pins, &reference.name);
        lines.push(write_instance(
            reference,
            stimuli,
            masters,
            pins,
            &instance_nets,
            &buses,
        ));
    }
    for block in behavioral_blocks(&module.properties) {
        lines.push(format!("  {block}"));
    }
    lines.push("endmodule".to_string());
}

fn write_instance(
    reference: &ModuleReference,
    stimuli: &StimulusIndex,
    masters: &MasterPorts,
    pins: &[&str],
    instance_nets: &IndexMap<(String, String), String>,
    buses: &Buses,
) -> String {
    let (shown, sources) = sim_params(reference);
    let resolve = |name: &str| sources.resolve(name);
    let mut parts = Vec::new();
    for parameter in shown {
        parts.push(format!(
            ".{}({})",
            sv_name(&parameter.name),
            format_param_value(parameter, Some(&resolve))
        ));
    }
    let mut master = reference.module_name.clone();
    let stimulus = stimuli.of(reference);
    if let Some(stimulus) = stimulus {
        // a Verilog-AMS LRM Annex E source primitive
        let current = reference.module_name == "isource";
        let (primitive_master, stimulus_params) =
            primitive(stimulus, current, Dialect::SystemVerilog);
        master = primitive_master;
        for (name, text) in stimulus_params {
            parts.push(format!(".{name}({text})"));
        }
    }
    let params = if parts.is_empty() {
        String::new()
    } else {
        format!(" #({})", parts.join(", "))
    };
    let attribute = match stimulus.and_then(primitive_file) {
        Some(file) => format!("(* pwl_file = \"{file}\" *) "),
        None => String::new(),
    };
    let connections = port_connections(reference, masters, pins, instance_nets, buses, sv_name);
    format!(
        "  {attribute}{}{params} {} ({connections});",
        sv_name(&master),
        sv_name(&reference.name)
    )
}
