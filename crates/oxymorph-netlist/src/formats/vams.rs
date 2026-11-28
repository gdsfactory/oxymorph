//! VAMS parser + writer, ported from `vams/transformer.py` and `vams/writer.py`.

use indexmap::IndexMap;
use oxymorph_schema::*;

use crate::ast::{check_tree_depth, TreeTransform};
use crate::engine::Engine;
use crate::error::Error;
use crate::expression::{format_expression, parameter_value, value_text, Dialect, Resolve};
use crate::hdl::*;
use crate::helpers::*;
use crate::preprocess::preprocess_vams;
use crate::stimulus::{
    add_stimuli, from_primitive, name_stimulus, primitive, primitive_file, stimulus_keys,
    StimulusIndex,
};

/// The built-in device modules (`DEVICE_PREFIX_TO_CIRCUIT` values).
const PRIMITIVES: [&str; 8] = [
    "resistor",
    "capacitor",
    "inductor",
    "diode",
    "bjt",
    "mosfet",
    "vsource",
    "isource",
];

/// Macro languages a Verilog-A writer keeps as its own (the rest ride in attributes).
const MACRO_LANGUAGES: [&str; 3] = ["verilog-ams", "verilog-a", "vams"];

const GRAMMAR: &str = include_str!("../../grammars/vams.lark");

thread_local! {
    static ENGINE: Engine = Engine::lalr(GRAMMAR).expect("vams grammar builds");
}

#[derive(Clone, Debug)]
pub struct InstanceData {
    pub module_type: String,
    pub name: String,
    pub named_ports: IndexMap<String, Option<NetExpression>>,
    pub positional_ports: Vec<NetExpression>,
    pub named_params: IndexMap<String, ParameterValue>,
    pub positional_params: Vec<ParameterValue>,
    pub dims: Vec<Dim>,
    pub attributes: IndexMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct NatureData {
    pub name: String,
    pub attrs: IndexMap<String, ParameterValue>,
}

#[derive(Clone, Debug)]
pub struct DisciplineData {
    pub name: String,
    pub natures: IndexMap<String, String>,
    pub domain: String,
}

#[derive(Clone, Debug)]
pub struct ParamsetData {
    pub name: String,
    pub base_type: String,
    pub params: IndexMap<String, ParameterValue>,
}

/// `input [3:0] a, b;` or `electrical [3:0] bus;`: the declared names and their ranges.
#[derive(Clone, Debug)]
pub struct DeclarationData {
    pub keyword: String,
    pub names: Vec<String>,
    pub ranges: Buses,
}

/// A module before connectivity: instances need the other modules' ports.
#[derive(Clone, Debug)]
pub struct VamsModule {
    pub name: String,
    pub ports: Vec<Port>,
    pub parameters: Vec<Parameter>,
    pub instances: Vec<InstanceData>,
    pub properties: Properties,
    pub buses: Buses,
    pub types: IndexMap<String, String>,
}

impl VamsModule {
    fn port_ranges(&self) -> IndexMap<String, Option<Range>> {
        let mut ranges = IndexMap::new();
        for port in &self.ports {
            ranges.insert(port.name.clone(), self.buses.get(&port.name).copied());
        }
        ranges
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Token(String, String),
    Tree(String, Vec<Item>),
    None,
    Circuit(Circuit),
    Pval(ParameterValue),
    ParamKv(String, ParameterValue),
    MapPval(IndexMap<String, ParameterValue>),
    DisciplineBody(IndexMap<String, String>, String),
    StrTuple(String, String),
    Str(String),
    Names(Vec<String>),
    DeclNames(Vec<(String, Vec<Dim>)>),
    DeclName(String, Vec<Dim>),
    Dim(Dim),
    Dims(Vec<Dim>),
    Expression(NetExpression),
    NamedConn(String, Option<NetExpression>),
    Attribute(IndexMap<String, String>),
    AttributeSpec(String, String),
    AssignList(Vec<(String, ParameterValue)>),
    TypedAssigns(Vec<(String, ParameterValue, String)>),
    Override(IndexMap<String, ParameterValue>, Vec<ParameterValue>),
    Conn(IndexMap<String, Option<NetExpression>>, Vec<NetExpression>),
    PortDir(PortDirection, DeclarationData),
    DisciplineDecl(DeclarationData),
    Ground(String),
    Analog(i64),
    Nature(NatureData),
    Discipline(DisciplineData),
    Paramset(ParamsetData),
    Instance(InstanceData),
    Module(VamsModule),
    List(Vec<Item>),
}

fn tok(it: &Item) -> Option<&str> {
    match it {
        Item::Token(_, v) => Some(v),
        _ => None,
    }
}

/// An identifier without its escape (`\I[3]` → `I[3]`).
fn unescaped(item: &Item) -> String {
    let text = tok(item).unwrap_or("");
    text.strip_prefix('\\').unwrap_or(text).to_string()
}

fn expression_of(item: Item) -> NetExpression {
    match item {
        Item::Expression(expression) => expression,
        other => NetExpression::Text(tok(&other).unwrap_or("").to_string()),
    }
}

fn dir_map(s: &str) -> PortDirection {
    match s {
        "input" => PortDirection::Input,
        "output" => PortDirection::Output,
        _ => PortDirection::Inout,
    }
}

pub struct VamsTransformer<'a> {
    analog_blocks: Vec<String>,
    options: &'a HdlOptions,
    /// Module → `(* name = "value" *)` attributes.
    attributes: IndexMap<String, IndexMap<String, String>>,
    /// Net initializers of the module being read.
    initial: Vec<(String, ParameterValue)>,
    /// Module → net → nodeset value.
    nodesets: IndexMap<String, IndexMap<String, ParameterValue>>,
    /// Stimuli of the source primitives, named and in build order.
    stimuli: Vec<Stimulus>,
    /// What the parse may still expand to.
    budget: ExpansionBudget,
    error: Option<Error>,
}

impl<'a> VamsTransformer<'a> {
    pub fn new(
        analog_blocks: Vec<String>,
        options: &'a HdlOptions,
        budget: ExpansionBudget,
    ) -> Self {
        Self {
            analog_blocks,
            options,
            budget,
            attributes: IndexMap::new(),
            initial: Vec::new(),
            nodesets: IndexMap::new(),
            stimuli: Vec::new(),
            error: None,
        }
    }

    fn fail(&mut self, error: Error) -> Item {
        self.error.get_or_insert(error);
        Item::None
    }
}

impl TreeTransform for VamsTransformer<'_> {
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
        if self.error.is_some() {
            return Item::None;
        }
        match name {
            // parity: kf-vams-sources (a Verilog-A number: `1M` is mega)
            "pvalue_number" => {
                let v = tok(&items[0]).unwrap_or("");
                let value = parse_verilog_number(v)
                    .unwrap_or_else(|| parse_parameter_number(v).unwrap_or_default());
                Item::Pval(value)
            }
            // parity: kf-vams-sources
            "pvalue_ident" => Item::Pval(parameter_value(tok(&items[0]).unwrap_or(""))),
            "pvalue_string" => Item::Pval(ParameterValue {
                expression: Some(Expression {
                    raw: Some(tok(&items[0]).unwrap_or("").to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            "attr_spec" => match attribute_value(items.get(1)) {
                Ok(value) => Item::AttributeSpec(tok(&items[0]).unwrap_or("").to_string(), value),
                Err(error) => self.fail(error),
            },
            "attribute" => {
                let mut attributes = IndexMap::new();
                for item in items {
                    if let Item::AttributeSpec(name, value) = item {
                        attributes.insert(name, value);
                    }
                }
                Item::Attribute(attributes)
            }
            "param_assign" => {
                let n = tok(&items[0]).unwrap_or("").to_string();
                let v = match items.get(1) {
                    Some(Item::Pval(p)) => p.clone(),
                    _ => ParameterValue::default(),
                };
                Item::ParamKv(n, v)
            }
            "param_assign_list" => Item::AssignList(
                items
                    .into_iter()
                    .filter_map(|i| match i {
                        Item::ParamKv(n, v) => Some((n, v)),
                        _ => None,
                    })
                    .collect(),
            ),
            "param_type" => items.into_iter().next().unwrap_or(Item::None),
            "port_list" => Item::Names(
                items
                    .into_iter()
                    .filter_map(|i| match i {
                        Item::Token(_, v) => Some(v),
                        _ => None,
                    })
                    .collect(),
            ),
            "port_item" => items.into_iter().next().unwrap_or(Item::None),
            "decl_name" => {
                let name = unescaped(&items[0]);
                if let Some(Item::Pval(value)) = items.get(2) {
                    self.initial.push((name.clone(), value.clone())); // `electrical b = 5.0;`
                }
                let dims = match items.get(1) {
                    Some(Item::Dims(dims)) => dims.clone(),
                    _ => Vec::new(),
                };
                Item::DeclName(name, dims)
            }
            "port_name_list" => {
                let mut names = Vec::new();
                for item in items {
                    if let Item::DeclName(name, dims) = item {
                        names.push((name, dims));
                    }
                }
                Item::DeclNames(names)
            }
            "range" => Item::Dim(
                items
                    .iter()
                    .map(|item| tok(item).unwrap_or("").trim().to_string())
                    .collect(),
            ),
            "dims" => {
                let mut dims = Vec::new();
                for item in items {
                    if let Item::Dim(dim) = item {
                        dims.push(dim);
                    }
                }
                Item::Dims(dims)
            }
            "net_ref" => Item::Expression(NetExpression::Net {
                name: unescaped(&items[0]),
                select: match items.get(1) {
                    Some(Item::Dim(dim)) => dim.clone(),
                    _ => Vec::new(),
                },
            }),
            "literal" => Item::Expression(NetExpression::Literal(
                tok(&items[0]).unwrap_or("").to_string(),
            )),
            "concat" => Item::Expression(NetExpression::Concat(
                items.into_iter().map(expression_of).collect(),
            )),
            "replicate" => {
                let mut items = items.into_iter();
                let count = items
                    .next()
                    .map(|item| tok(&item).unwrap_or("").to_string())
                    .unwrap_or_default();
                match replicate(&count, items.map(expression_of).collect(), &mut self.budget) {
                    Ok(expression) => Item::Expression(expression),
                    Err(error) => self.fail(error),
                }
            }
            "port_dir_decl" | "discipline_decl" => {
                let keyword = tok(&items[0]).unwrap_or("").to_string();
                let packed = match items.get(1) {
                    Some(Item::Dim(dim)) => Some(dim.clone()),
                    _ => None,
                };
                let names = match items.last() {
                    Some(Item::DeclNames(names)) => names.clone(),
                    _ => Vec::new(),
                };
                match declaration_data(keyword, packed, &names, &mut self.budget) {
                    Ok(data) if name == "port_dir_decl" => {
                        Item::PortDir(dir_map(&data.keyword), data)
                    }
                    Ok(data) => Item::DisciplineDecl(data),
                    Err(error) => self.fail(error),
                }
            }
            "typed_parameter_decl" => {
                let ptype = tok(&items[0]).unwrap_or("").to_string();
                Item::TypedAssigns(typed_assigns(&items, ptype, false))
            }
            "untyped_parameter_decl" => {
                Item::TypedAssigns(typed_assigns(&items, String::new(), false))
            }
            "typed_localparam_decl" => {
                let ptype = tok(&items[0]).unwrap_or("").to_string();
                Item::TypedAssigns(typed_assigns(&items, ptype, true))
            }
            "untyped_localparam_decl" => {
                Item::TypedAssigns(typed_assigns(&items, String::new(), true))
            }
            "ground_stmt" => Item::Ground(tok(&items[0]).unwrap_or("").to_string()),
            "named_override" => {
                let name = tok(&items[0]).unwrap_or("").to_string();
                let text = tok(&items[1]).unwrap_or("()");
                let text = text[1..text.len() - 1].trim(); // `(0.13 * 1e-6)`
                                                           // parity: kf-vams-sources (an override is any expression)
                let value = parse_verilog_number(text)
                    .or_else(|| (!text.is_empty()).then(|| parameter_value(text)));
                Item::ParamKv(name, value.unwrap_or_default())
            }
            "positional_override" => match items.into_iter().next() {
                Some(Item::Pval(p)) => Item::Pval(p),
                _ => Item::Pval(ParameterValue::default()),
            },
            "param_override" => {
                let mut named = IndexMap::new();
                let mut positional = Vec::new();
                for i in items {
                    match i {
                        Item::ParamKv(n, v) => {
                            named.insert(n, v);
                        }
                        Item::Pval(p) => positional.push(p),
                        _ => {}
                    }
                }
                Item::Override(named, positional)
            }
            "named_conn" => {
                let mut items = items.into_iter();
                let port = items
                    .next()
                    .map(|item| tok(&item).unwrap_or("").to_string())
                    .unwrap_or_default();
                Item::NamedConn(port, items.next().map(expression_of))
            }
            "positional_conn" => match items.into_iter().next() {
                Some(item) => Item::Expression(expression_of(item)),
                None => Item::None,
            },
            "port_conn" => {
                let mut named = IndexMap::new();
                let mut positional = Vec::new();
                for item in items {
                    match item {
                        Item::NamedConn(port, expression) => {
                            named.insert(port, expression);
                        }
                        Item::Expression(expression) => positional.push(expression),
                        _ => {}
                    }
                }
                Item::Conn(named, positional)
            }
            "instance_stmt" => instance_stmt(items),
            "analog_placeholder" => {
                let v = tok(&items[0]).unwrap_or("0");
                Item::Analog(v.parse::<f64>().unwrap_or(0.0) as i64)
            }
            "nature_body" => {
                let mut attrs = IndexMap::new();
                for i in items {
                    if let Item::ParamKv(n, v) = i {
                        attrs.insert(n, v);
                    }
                }
                Item::MapPval(attrs)
            }
            "nature_attr" => {
                let n = tok(&items[0]).unwrap_or("").to_string();
                let v = match items.get(1) {
                    Some(Item::Pval(p)) => p.clone(),
                    _ => ParameterValue::default(),
                };
                Item::ParamKv(n, v)
            }
            "nature_def" => {
                let name = tok(&items[0]).unwrap_or("").to_string();
                let attrs = match items.get(1) {
                    Some(Item::MapPval(m)) => m.clone(),
                    _ => IndexMap::new(),
                };
                Item::Nature(NatureData { name, attrs })
            }
            "discipline_body" => {
                let mut natures = IndexMap::new();
                let mut domain = String::new();
                for i in items {
                    match i {
                        Item::StrTuple(a, b) => {
                            natures.insert(a, b);
                        }
                        Item::Str(s) => domain = s,
                        _ => {}
                    }
                }
                Item::DisciplineBody(natures, domain)
            }
            "discipline_nature_attr" => Item::StrTuple(
                tok(&items[0]).unwrap_or("").to_string(),
                tok(&items[1]).unwrap_or("").to_string(),
            ),
            "discipline_domain_attr" => Item::Str(tok(&items[0]).unwrap_or("").to_string()),
            "discipline_def" => {
                let name = tok(&items[0]).unwrap_or("").to_string();
                let (natures, domain) = match items.get(1) {
                    Some(Item::DisciplineBody(natures, domain)) => {
                        (natures.clone(), domain.clone())
                    }
                    _ => (IndexMap::new(), String::new()),
                };
                Item::Discipline(DisciplineData {
                    name,
                    natures,
                    domain,
                })
            }
            "paramset_body" => {
                let mut params = IndexMap::new();
                for i in items {
                    if let Item::ParamKv(n, v) = i {
                        params.insert(n, v);
                    }
                }
                Item::MapPval(params)
            }
            "paramset_param" => {
                for i in items {
                    if let Item::ParamKv(n, v) = i {
                        return Item::ParamKv(n, v);
                    }
                }
                Item::None
            }
            "paramset_constraint" => Item::None,
            "paramset_def" => {
                let name = tok(&items[0]).unwrap_or("").to_string();
                let base_type = tok(&items[1]).unwrap_or("").to_string();
                let params = match items.get(2) {
                    Some(Item::MapPval(m)) => m.clone(),
                    _ => IndexMap::new(),
                };
                Item::Paramset(ParamsetData {
                    name,
                    base_type,
                    params,
                })
            }
            "module_body" => Item::List(
                items
                    .into_iter()
                    .filter(|i| !matches!(i, Item::None))
                    .collect(),
            ),
            "module_def" => match self.module_def(items) {
                Ok(module) => Item::Module(module),
                Err(error) => self.fail(error),
            },
            "start" => match self.start(items) {
                Ok(circuit) => Item::Circuit(circuit),
                Err(error) => self.fail(error),
            },
            _ => Item::Tree(name.to_string(), items),
        }
    }
}

/// The text of an attribute value: a string literal unescaped (Verilog's `\"`, `\\`,
/// `\n` are JSON's), else the value as Verilog-A text; a bare flag is `1`.
fn attribute_value(value: Option<&Item>) -> Result<String, Error> {
    let Some(Item::Pval(value)) = value else {
        return Ok("1".to_string());
    };
    let raw = value
        .expression
        .as_ref()
        .and_then(|expression| expression.raw.as_deref());
    if let Some(raw) = raw.filter(|raw| raw.starts_with('"')) {
        return serde_json::from_str::<String>(raw)
            .map_err(|error| Error::Validation(format!("attribute string {raw}: {error}")));
    }
    Ok(value_text(value, Dialect::Vams))
}

/// The declared names and ranges; every declared bit is charged to `budget`.
fn declaration_data(
    keyword: String,
    packed: Option<Dim>,
    names: &[(String, Vec<Dim>)],
    budget: &mut ExpansionBudget,
) -> Result<DeclarationData, Error> {
    let mut ranges = Buses::new();
    let mut declared = Vec::new();
    for (name, dims) in names {
        declared.push(name.clone());
        let mut all_dims = Vec::new();
        all_dims.extend(packed.clone());
        all_dims.extend(dims.iter().cloned());
        let range = decl_range(name, &all_dims)?;
        budget.charge(range.map_or(1, range_width))?;
        if let Some(range) = range {
            ranges.insert(name.clone(), range);
        }
    }
    Ok(DeclarationData {
        keyword,
        names: declared,
        ranges,
    })
}

fn instance_stmt(items: Vec<Item>) -> Item {
    let mut attributes = IndexMap::new();
    let mut rest = Vec::new();
    for item in items {
        match item {
            Item::Attribute(found) => attributes.extend(found),
            other => rest.push(other),
        }
    }
    let mut rest = rest.into_iter();
    let module_type = rest.next().map(|item| unescaped(&item)).unwrap_or_default();
    let mut next = rest.next();
    let (named_params, positional_params) = match next {
        Some(Item::Override(named, positional)) => {
            next = rest.next();
            (named, positional)
        }
        _ => (IndexMap::new(), Vec::new()),
    };
    let name = next.map(|item| unescaped(&item)).unwrap_or_default();
    let dims = match rest.next() {
        Some(Item::Dims(dims)) => dims,
        _ => Vec::new(),
    };
    let (named_ports, positional_ports) = match rest.next() {
        Some(Item::Conn(named, positional)) => (named, positional),
        _ => (IndexMap::new(), Vec::new()),
    };
    Item::Instance(InstanceData {
        module_type,
        name,
        named_ports,
        positional_ports,
        named_params,
        positional_params,
        dims,
        attributes,
    })
}

fn typed_assigns(
    items: &[Item],
    ptype: String,
    localparam: bool,
) -> Vec<(String, ParameterValue, String)> {
    let mut assigns = Vec::new();
    for item in items {
        if let Item::AssignList(list) = item {
            for (n, v) in list {
                let tag = if localparam {
                    if ptype.is_empty() {
                        "localparam".to_string()
                    } else {
                        format!("localparam:{ptype}")
                    }
                } else {
                    ptype.clone()
                };
                assigns.push((n.clone(), v.clone(), tag));
            }
        }
    }
    assigns
}

/// The macros a `spice_code` / `spice_module_code` attribute carries (a JSON list).
fn macros_of(code: Option<&String>) -> Result<Vec<Macro>, Error> {
    let code = code.map_or("", String::as_str);
    let code = if code.is_empty() { "[]" } else { code };
    serde_json::from_str(code).map_err(|error| Error::Validation(format!("inline code: {error}")))
}

impl VamsTransformer<'_> {
    fn module_def(&mut self, items: Vec<Item>) -> Result<VamsModule, Error> {
        let mut attributes = IndexMap::new();
        let mut rest = Vec::new();
        for item in items {
            match item {
                Item::Attribute(found) => attributes.extend(found),
                other => rest.push(other),
            }
        }
        let module_name = unescaped(&rest[0]);
        self.attributes.insert(module_name.clone(), attributes);
        if !self.initial.is_empty() {
            let initial: IndexMap<String, ParameterValue> =
                std::mem::take(&mut self.initial).into_iter().collect();
            self.nodesets.insert(module_name.clone(), initial);
        }

        let mut port_names: Vec<String> = Vec::new();
        let mut body_items: Vec<Item> = Vec::new();
        for item in rest.into_iter().skip(1) {
            match item {
                Item::Names(names) => port_names = names,
                Item::List(list) => body_items = list,
                _ => {}
            }
        }
        check_unique_ports(&module_name, &port_names)?;

        let mut port_dirs: IndexMap<String, PortDirection> = IndexMap::new();
        let mut types: IndexMap<String, String> = IndexMap::new();
        let mut buses = Buses::new();
        let mut parameters: Vec<Parameter> = Vec::new();
        let mut instances: Vec<InstanceData> = Vec::new();
        let mut ground_net = String::new();
        let mut analog_block = String::new();
        let mut properties = Properties::new();

        for item in body_items {
            match item {
                Item::PortDir(direction, data) => {
                    for name in &data.names {
                        port_dirs.insert(name.clone(), direction);
                    }
                    for (name, range) in data.ranges {
                        buses.entry(name).or_insert(range);
                    }
                }
                Item::DisciplineDecl(data) => {
                    for name in &data.names {
                        types.insert(name.clone(), data.keyword.clone());
                    }
                    for (name, range) in data.ranges {
                        buses.entry(name).or_insert(range);
                    }
                }
                Item::TypedAssigns(list) => {
                    for (pname, pval, ptype_tag) in list {
                        let mut props = Properties::new();
                        let ptype;
                        if let Some(rest) = ptype_tag.strip_prefix("localparam") {
                            props.insert("localparam".to_string(), "true".to_string());
                            ptype = rest.strip_prefix(':').unwrap_or("").to_string();
                        } else {
                            ptype = ptype_tag;
                        }
                        if !ptype.is_empty() {
                            props.insert("type".to_string(), ptype);
                        }
                        parameters.push(Parameter {
                            name: pname,
                            default_value: Some(pval),
                            properties: props,
                            ..Default::default()
                        });
                    }
                }
                Item::Instance(inst) => instances.push(inst),
                Item::Ground(net) => ground_net = net,
                Item::Analog(index)
                    if index >= 0 && (index as usize) < self.analog_blocks.len() =>
                {
                    analog_block = self.analog_blocks[index as usize].clone();
                }
                _ => {}
            }
        }

        let mut ports = Vec::new();
        for name in port_names {
            let range = buses.get(&name).copied();
            let mut port_properties = Properties::new();
            if let Some((msb, lsb)) = range {
                port_properties.insert("range".to_string(), format!("{msb}:{lsb}"));
            }
            ports.push(Port {
                direction: port_dirs
                    .get(&name)
                    .copied()
                    .unwrap_or(PortDirection::Inout),
                // parity: kf-signal-domains
                domain: domain_of(&name),
                discipline: types.get(&name).cloned().unwrap_or_default(),
                width: range.map_or(0, |range| range_width(range) as u32),
                properties: port_properties,
                name,
            });
        }

        if !ground_net.is_empty() {
            properties.insert("ground_net".to_string(), ground_net);
        }
        if !analog_block.is_empty() {
            properties.insert("analog_block".to_string(), analog_block);
        }

        Ok(VamsModule {
            name: module_name,
            ports,
            parameters,
            instances,
            properties,
            buses,
            types,
        })
    }

    fn build_module(
        &mut self,
        module: &VamsModule,
        modules: &IndexMap<String, VamsModule>,
        paramsets: &IndexMap<String, String>,
    ) -> Result<Module, Error> {
        let mut net_map = NetMap::new();
        for port in &module.ports {
            let pins = match module.buses.get(&port.name) {
                Some(range) => bits(&port.name, *range)?,
                None => vec![port.name.clone()],
            };
            for pin in pins {
                net_map.add(&pin, "", &pin);
            }
        }

        let mut module_references: Vec<ModuleReference> = Vec::new();
        for inst in &module.instances {
            let params: Vec<Parameter> = if !inst.named_params.is_empty() {
                inst.named_params
                    .iter()
                    .map(|(n, v)| Parameter {
                        name: n.clone(),
                        default_value: Some(v.clone()),
                        ..Default::default()
                    })
                    .collect()
            } else {
                inst.positional_params
                    .iter()
                    .enumerate()
                    .map(|(i, v)| Parameter {
                        name: format!("p{i}"),
                        default_value: Some(v.clone()),
                        ..Default::default()
                    })
                    .collect()
            };

            let target = modules.get(&inst.module_type);
            let range = decl_range(&inst.name, &inst.dims)?;
            if !inst.dims.is_empty() && range.is_none() {
                return Err(Error::Validation(format!(
                    "{}[{}]: instance array range must be constant",
                    inst.name,
                    inst.dims[0].join(":")
                )));
            }
            let target_ranges = target.map(VamsModule::port_ranges);
            let count = range.map_or(1, range_width);
            let ranges = target_ranges.as_ref();
            charge_instances(count, ranges, params.len(), &mut self.budget)?;
            let names = instance_names(&inst.name, range)?;
            let mut connections: Vec<(String, Option<&NetExpression>)> = Vec::new();
            for (port, expression) in &inst.named_ports {
                connections.push((port.clone(), expression.as_ref()));
            }
            // parity: kf-hdl-nets (bit-level nets; a positional pin takes the port's name)
            for (index, expression) in inst.positional_ports.iter().enumerate() {
                let port = target
                    .and_then(|target| target.ports.get(index))
                    .map_or_else(|| format!("p{index}"), |port| port.name.clone());
                connections.push((port, Some(expression)));
            }
            for (port, expression) in connections {
                let Some(expression) = expression else {
                    continue; // unconnected
                };
                let triples = connect(
                    &names,
                    &port,
                    expression,
                    &module.buses,
                    target_ranges.as_ref(),
                    &mut self.budget,
                )?;
                for (name, pin, net) in triples {
                    net_map.add(&net, &name, &pin);
                }
            }

            // parity: kf-vams-sources (an LRM Annex E source: vpulse, isine, ...)
            let file = inst.attributes.get("pwl_file").map(String::as_str);
            if let Some(stimulus) = from_primitive(&inst.module_type, &inst.named_params, file) {
                let keys = stimulus_keys(Some(&stimulus));
                let device_params: Vec<Parameter> = params
                    .into_iter()
                    .filter(|parameter| !keys.contains(&parameter.name.as_str()))
                    .collect();
                let current = inst.module_type.to_lowercase().starts_with('i');
                for name in &names {
                    let mut reference = ModuleReference {
                        name: name.clone(),
                        module_name: if current { "isource" } else { "vsource" }.to_string(),
                        parameters: device_params.clone(),
                        ..Default::default()
                    };
                    let named = name_stimulus(&module.name, &mut reference, stimulus.clone());
                    self.stimuli.push(named);
                    module_references.push(reference);
                }
                continue;
            }
            // parity: kf-vams-sources (`pch M1 (...)` with `paramset pch mosfet`: a mosfet)
            let base = paramsets
                .get(&inst.module_type)
                .filter(|base| PRIMITIVES.contains(&base.as_str()));
            for name in &names {
                let mut reference = ModuleReference {
                    name: name.clone(),
                    module_name: inst.module_type.clone(),
                    parameters: params.clone(),
                    ..Default::default()
                };
                if let Some(base) = base {
                    reference.module_name = base.clone();
                    reference
                        .properties
                        .insert("model_name".to_string(), inst.module_type.clone());
                }
                module_references.push(reference);
            }
        }

        Ok(Module {
            name: module.name.clone(),
            ports: module.ports.clone(),
            parameters: module.parameters.clone(),
            module_references,
            connections: typed_connections(&net_map, &module.types),
            properties: module.properties.clone(),
            ..Default::default()
        })
    }

    fn start(&mut self, items: Vec<Item>) -> Result<Circuit, Error> {
        let mut modules: IndexMap<String, VamsModule> = IndexMap::new();
        let mut paramsets: IndexMap<String, String> = IndexMap::new();
        for item in &items {
            match item {
                Item::Module(module) => {
                    modules.insert(module.name.clone(), module.clone());
                }
                Item::Paramset(paramset) => {
                    paramsets.insert(paramset.name.clone(), paramset.base_type.clone());
                }
                _ => {}
            }
        }
        let mut built = Vec::new();
        for module in modules.values() {
            built.push(self.build_module(module, &modules, &paramsets)?);
        }

        let mut ext_modules: IndexMap<String, ExternalModule> = IndexMap::new();
        for item in &items {
            match item {
                Item::Nature(n) => {
                    let mut props = Properties::new();
                    props.insert("ext_kind".to_string(), "nature".to_string());
                    for (k, v) in &n.attrs {
                        let val = match &v.prefixed_value {
                            Some(pv) => format_si_value(pv),
                            None => String::new(),
                        };
                        props.insert(format!("nature_{k}"), val);
                    }
                    ext_modules.insert(
                        n.name.clone(),
                        ExternalModule {
                            name: n.name.clone(),
                            kind: ExternalModuleKind::Nature,
                            properties: props,
                            ..Default::default()
                        },
                    );
                }
                Item::Discipline(d) => {
                    let mut props = Properties::new();
                    props.insert("ext_kind".to_string(), "discipline".to_string());
                    for (role, nature_name) in &d.natures {
                        props.insert(format!("discipline_{role}"), nature_name.clone());
                    }
                    if !d.domain.is_empty() {
                        props.insert("discipline_domain".to_string(), d.domain.clone());
                    }
                    ext_modules.insert(
                        d.name.clone(),
                        ExternalModule {
                            name: d.name.clone(),
                            kind: ExternalModuleKind::Discipline,
                            properties: props,
                            ..Default::default()
                        },
                    );
                }
                Item::Paramset(p) => {
                    let params: Vec<Parameter> = p
                        .params
                        .iter()
                        .map(|(n, v)| Parameter {
                            name: n.clone(),
                            default_value: Some(v.clone()),
                            ..Default::default()
                        })
                        .collect();
                    let mut props = Properties::new();
                    props.insert("ext_kind".to_string(), "model".to_string());
                    props.insert("model_type".to_string(), p.base_type.clone());
                    ext_modules.insert(
                        p.name.clone(),
                        ExternalModule {
                            name: p.name.clone(),
                            kind: ExternalModuleKind::Model,
                            parameters: params,
                            properties: props,
                            ..Default::default()
                        },
                    );
                }
                _ => {}
            }
        }

        // parity: kf-top-elements
        let has_top = built.iter().any(|module| module.name == "__top__");
        let top_module = if has_top {
            "__top__".to_string()
        } else {
            built.last().map(|m| m.name.clone()).unwrap_or_default()
        };

        // Verilog-A cannot say these: they ride in attributes (see the writer)
        let top_attributes = self
            .attributes
            .get(&top_module)
            .cloned()
            .unwrap_or_default();
        let mut directives = Vec::new();
        let libraries = top_attributes.get("spice_lib").map_or("", String::as_str);
        for line in libraries.lines() {
            if !line.trim().is_empty() {
                directives.push(Directive {
                    kind: DirectiveKind::Lib,
                    value: line.trim().to_string(),
                    ..Default::default()
                });
            }
        }
        let macros = macros_of(top_attributes.get("spice_code"))?;
        for module in &mut built {
            let attributes = self.attributes.get(&module.name);
            module.macros = macros_of(attributes.and_then(|found| found.get("spice_module_code")))?;
        }
        let mut simulation = None;
        let nodesets = self
            .nodesets
            .get(&top_module)
            .filter(|found| !found.is_empty());
        if let Some(nodesets) = nodesets {
            let mut conditions = Properties::new();
            for (net, value) in nodesets {
                conditions.insert(format!("V({net})"), value_text(value, Dialect::Ngspice));
            }
            simulation = Some(Simulation {
                node_sets: Some(InitialCondition { conditions }),
                ..Default::default()
            });
        }

        let mut circuit = Circuit {
            name: String::new(),
            top_module,
            modules: built,
            ext_modules: ext_modules.into_values().collect(),
            directives,
            macros,
            simulation,
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
}

/// Parse Verilog-AMS netlist text with the default [`HdlOptions`].
pub fn parse_vams(text: &str) -> Result<Circuit, Error> {
    parse_vams_with(text, &HdlOptions::default())
}

/// Parse Verilog-AMS netlist text into a circuit.
///
/// Nets are bit level, as in `parse_systemverilog`: a bus `input [3:0] in;` is one `Port`
/// with `width=4` whose bits `in[3]` … `in[0]` are the net members, and instance arrays
/// `X I[3:0] (...)` expand to `I[3]` … `I[0]`. Width mismatches and selects of undeclared
/// buses are `Error::Validation`.
pub fn parse_vams_with(text: &str, options: &HdlOptions) -> Result<Circuit, Error> {
    let mut budget = ExpansionBudget::for_text(text);
    let pre = preprocess_vams(text, &options.defines, &mut budget)?;
    let tree = ENGINE.with(|engine| engine.parse(&pre.text))?;
    check_tree_depth(&tree)?;
    let mut transformer = VamsTransformer::new(pre.analog_blocks, options, budget);
    let result = transformer.transform(&tree);
    if let Some(error) = transformer.error {
        return Err(error);
    }
    let Item::Circuit(mut circuit) = result else {
        return Err(Error::Validation(
            "vams transform did not produce a circuit".into(),
        ));
    };
    let mut directives = directives_of(&pre.directives);
    if !directives.is_empty() {
        directives.append(&mut circuit.directives);
        circuit.directives = directives;
    }
    Ok(circuit)
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

fn format_param_value(pval: &ParameterValue, resolve: Option<Resolve>) -> String {
    if let Some(pv) = &pval.prefixed_value {
        // parity: kf-vams-sources (mega is `M`)
        return format_verilog_value(pv);
    }
    let text = match &pval.expression {
        // parity: kf-expressions
        Some(expression) => format_expression(expression, Dialect::Vams, resolve, false),
        None => String::new(),
    };
    if text.is_empty() {
        "0".to_string()
    } else {
        text
    }
}

/// The Verilog-A macros as body text; the rest travel in attributes (`foreign`).
fn verilog_macro_lines(macros: &[Macro]) -> Vec<String> {
    let mut verilog = Vec::new();
    for code in macros {
        if is_verilog(code) {
            verilog.push(code.clone());
        }
    }
    let mut accept: Vec<MacroLanguage> = Vec::new();
    for language in MACRO_LANGUAGES {
        accept.push((language, None));
    }
    macro_lines(&verilog, &accept, "//")
}

fn is_verilog(code: &Macro) -> bool {
    MACRO_LANGUAGES.contains(&code.language.to_lowercase().as_str())
}

/// JSON of the macros in a language other than Verilog's ("" when none).
fn foreign(macros: &[Macro]) -> String {
    let mut entries = Vec::new();
    for code in macros {
        if !is_verilog(code) {
            entries.push(format!(
                "{{\"name\": {}, \"language\": {}, \"body\": {}}}",
                json_string(&code.name),
                json_string(&code.language),
                json_string(&code.body)
            ));
        }
    }
    if entries.is_empty() {
        String::new()
    } else {
        format!("[{}]", entries.join(", "))
    }
}

/// What Verilog-A cannot say, as attributes of its module: on the top one the circuit's
/// SPICE libraries (`.lib path section`) and inline code in other languages; on any module
/// its own such code.
fn attributes(module: &Module, circuit: &Circuit) -> String {
    let mut found = Vec::new();
    if module.name == circuit.top_module {
        let mut libraries = Vec::new();
        for directive in &circuit.directives {
            if directive.kind == DirectiveKind::Lib {
                libraries.push(directive.value.as_str());
            }
        }
        if !libraries.is_empty() {
            found.push(format!(
                "spice_lib = {}",
                json_string(&libraries.join("\n"))
            ));
        }
        let code = foreign(&circuit.macros);
        if !code.is_empty() {
            found.push(format!("spice_code = {}", json_string(&code)));
        }
    }
    let code = foreign(&module.macros);
    if !code.is_empty() {
        found.push(format!("spice_module_code = {}", json_string(&code)));
    }
    if found.is_empty() {
        String::new()
    } else {
        format!("(* {} *)", found.join(", "))
    }
}

/// `input a, b` / `optical [3:0] bus`.
fn declaration(keyword: &str, names: &[String], range: Option<Range>) -> String {
    let dim = match range {
        Some((msb, lsb)) => format!(" [{msb}:{lsb}]"),
        None => String::new(),
    };
    format!("{keyword}{dim} {}", names.join(", "))
}

pub fn write_vams(circuit: &Circuit) -> String {
    let masters = master_ports(circuit);
    let stimuli = StimulusIndex::new(circuit);
    let mut lines: Vec<String> = Vec::new();

    write_directives(&circuit.directives, &mut lines);
    lines.extend(verilog_macro_lines(&circuit.macros));

    for ext in &circuit.ext_modules {
        if ext.kind == ExternalModuleKind::Nature {
            lines.push(format!("nature {};", ext.name));
            for (key, val) in &ext.properties {
                if let Some(attr) = key.strip_prefix("nature_") {
                    lines.push(format!("  {attr} = {val};"));
                }
            }
            lines.push("endnature".to_string());
        } else if ext.kind == ExternalModuleKind::Discipline {
            lines.push(format!("discipline {};", ext.name));
            if let Some(domain) = ext.properties.get("discipline_domain") {
                lines.push(format!("  domain {domain};"));
            }
            for (key, val) in &ext.properties {
                if key.starts_with("discipline_") && key != "discipline_domain" {
                    let role = key.strip_prefix("discipline_").unwrap();
                    lines.push(format!("  {role} {val};"));
                }
            }
            lines.push("enddiscipline".to_string());
        }
    }

    // parity: kf-vams-sources (LRM 7.5: the primitive a device model is of)
    for (model, base) in paramsets(circuit) {
        lines.push(format!("paramset {} {base};", sv_name(model)));
        lines.push("endparamset".to_string());
    }

    for module in &circuit.modules {
        write_module(module, circuit, &masters, &stimuli, &mut lines);
    }

    if lines.last().map(|s| !s.is_empty()).unwrap_or(false) {
        lines.push(String::new());
    }
    lines.join("\n")
}

/// `model -> primitive` of the device models instances are called by, so a reader
/// knows `pch M1 (...)` is a `mosfet` (its pins, its kind).
fn paramsets(circuit: &Circuit) -> IndexMap<&str, &str> {
    let mut defined_names = std::collections::HashSet::new();
    for module in &circuit.modules {
        defined_names.insert(module.name.as_str());
    }
    let mut bases = IndexMap::new();
    for module in &circuit.modules {
        for reference in &module.module_references {
            let Some(model) = reference.properties.get("model_name") else {
                continue;
            };
            let defined = defined_names.contains(model.as_str());
            if !model.is_empty() && !defined && PRIMITIVES.contains(&reference.module_name.as_str())
            {
                bases
                    .entry(model.as_str())
                    .or_insert(reference.module_name.as_str());
            }
        }
    }
    bases
}

fn direction_keyword(direction: PortDirection) -> &'static str {
    match direction {
        PortDirection::Input => "input",
        PortDirection::Output => "output",
        PortDirection::Inout => "inout",
    }
}

fn write_module(
    module: &Module,
    circuit: &Circuit,
    masters: &MasterPorts,
    stimuli: &StimulusIndex,
    lines: &mut Vec<String>,
) {
    let port_names: Vec<String> = module.ports.iter().map(|p| p.name.clone()).collect();
    let attribute_line = attributes(module, circuit);
    if !attribute_line.is_empty() {
        lines.push(attribute_line);
    }
    lines.push(format!(
        "module {} ({});",
        sv_name(&module.name),
        port_names.join(", ")
    ));
    lines.extend(verilog_macro_lines(&module.macros));

    let mut ranges: IndexMap<&str, Option<Range>> = IndexMap::new();
    for port in &module.ports {
        let range_text = port.properties.get("range").map(String::as_str);
        ranges.insert(&port.name, declared_range(port.width, range_text));
    }
    let mut dirs_by_dir: IndexMap<(&str, Option<Range>), Vec<String>> = IndexMap::new();
    for port in &module.ports {
        let key = (
            direction_keyword(port.direction),
            ranges[port.name.as_str()],
        );
        dirs_by_dir.entry(key).or_default().push(port.name.clone());
    }
    for ((direction, range), names) in &dirs_by_dir {
        lines.push(format!("  {};", declaration(direction, names, *range)));
    }

    // disciplines of ports, then of internal nets (`name[i]` nets are buses)
    let mut internal_names = Vec::new();
    let mut net_types: IndexMap<&str, &str> = IndexMap::new();
    for connection in &module.connections {
        if !ranges.contains_key(connection.name.as_str()) {
            internal_names.push(connection.name.as_str());
        }
        let declared = connection
            .properties
            .get("discipline")
            .map_or("", String::as_str);
        net_types.insert(bus_base(&connection.name), declared);
    }
    let internal = net_buses(&internal_names);
    let mut disc_by_disc: IndexMap<(&str, Option<Range>), Vec<String>> = IndexMap::new();
    for port in &module.ports {
        if !port.discipline.is_empty() {
            let key = (port.discipline.as_str(), ranges[port.name.as_str()]);
            disc_by_disc.entry(key).or_default().push(port.name.clone());
        }
    }
    for (net, net_type) in &net_types {
        if !ranges.contains_key(net) && (internal.contains_key(*net) || !net_type.is_empty()) {
            let keyword = if net_type.is_empty() {
                "wire"
            } else {
                net_type
            };
            disc_by_disc
                .entry((keyword, internal.get(*net).copied()))
                .or_default()
                .push(net.to_string());
        }
    }
    for ((discipline, range), names) in &disc_by_disc {
        lines.push(format!("  {};", declaration(discipline, names, *range)));
    }
    if module.name == circuit.top_module {
        let node_sets = circuit
            .simulation
            .as_ref()
            .and_then(|simulation| simulation.node_sets.as_ref());
        if let Some(node_sets) = node_sets {
            for (node, value) in &node_sets.conditions {
                // LRM §3.6.3.2
                let net = node.strip_prefix("V(").unwrap_or(node);
                let net = net.strip_suffix(')').unwrap_or(net);
                lines.push(format!("  electrical {} = {value};", sv_name(net)));
            }
        }
    }
    let mut buses = Buses::new();
    for (base, range) in &internal {
        if !ranges.contains_key(base.as_str()) {
            buses.insert(base.clone(), *range);
        }
    }
    for (name, range) in &ranges {
        if let Some(range) = range {
            buses.insert(name.to_string(), *range);
        }
    }

    for param in &module.parameters {
        let is_local = param
            .properties
            .get("localparam")
            .map(|v| v == "true")
            .unwrap_or(false);
        let kw = if is_local { "localparam" } else { "parameter" };
        let ptype = param.properties.get("type").cloned().unwrap_or_default();
        let val = match &param.default_value {
            Some(v) => format_param_value(v, None),
            None => "0".to_string(),
        };
        let type_str = if ptype.is_empty() {
            String::new()
        } else {
            format!(" {ptype}")
        };
        lines.push(format!("  {kw}{type_str} {} = {val};", param.name));
    }

    // parity: kf-vams-sources (the circuit's ground net, where this module uses it)
    let circuit_ground = circuit.properties.get("ground_net").filter(|ground| {
        !ground.is_empty()
            && module
                .connections
                .iter()
                .any(|connection| &connection.name == *ground)
    });
    let ground_net = module
        .properties
        .get("ground_net")
        .filter(|ground| !ground.is_empty())
        .or(circuit_ground);
    if let Some(ground_net) = ground_net {
        lines.push(format!("  ground {ground_net};"));
    }

    let inst_nets = connections_to_instance_nets(&module.connections, "");
    let instance_pins = pins_by_instance(&inst_nets);
    for reference in &module.module_references {
        let pins = pins_of(&instance_pins, &reference.name);
        lines.push(write_instance(
            reference, stimuli, masters, pins, &inst_nets, &buses,
        ));
    }

    if let Some(analog_block) = module.properties.get("analog_block") {
        lines.push(format!("  {analog_block}"));
    }

    lines.push("endmodule".to_string());
}

/// VAMS writes port names in connections as they are (SystemVerilog escapes them).
fn plain_name(name: &str) -> String {
    name.to_string()
}

fn write_instance(
    reference: &ModuleReference,
    stimuli: &StimulusIndex,
    masters: &MasterPorts,
    pins: &[&str],
    inst_nets: &IndexMap<(String, String), String>,
    buses: &Buses,
) -> String {
    let (shown, sources) = sim_params(reference);
    let resolve = |name: &str| sources.resolve(name);
    let mut parts: Vec<String> = Vec::new();
    for parameter in shown {
        if let Some(value) = &parameter.default_value {
            let text = format_param_value(value, Some(&resolve));
            parts.push(format!(".{}({text})", parameter.name));
        }
    }
    // parity: kf-vams-sources (a `.model` name is the module, LRM E.2.2.1; a source an
    // Annex E primitive)
    let mut master = reference
        .properties
        .get("model_name")
        .filter(|model| !model.is_empty())
        .unwrap_or(&reference.module_name)
        .clone();
    let stimulus = stimuli.of(reference);
    if let Some(stimulus) = stimulus {
        let (primitive_master, stimulus_params) =
            primitive(stimulus, reference.module_name == "isource", Dialect::Vams);
        master = primitive_master;
        for (name, text) in stimulus_params {
            parts.push(format!(".{name}({text})"));
        }
    }
    let params_str = if parts.is_empty() {
        String::new()
    } else {
        format!(" #({})", parts.join(", "))
    };
    // parity: kf-hdl-nets (`bus[m:l]` / `{…}` connections, `.p()` open, `\0 ` escapes)
    let connections = port_connections(reference, masters, pins, inst_nets, buses, plain_name);
    let attribute = match stimulus.and_then(primitive_file) {
        Some(file) => format!("(* pwl_file = {} *) ", json_string(&file)),
        None => String::new(),
    };
    format!(
        "  {attribute}{}{params_str} {} ({connections});",
        sv_name(&master),
        sv_name(&reference.name)
    )
}
