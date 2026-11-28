//! Parse a gdsfactory+ `.gsch` schematic into the IR, ported from kfnetlist
//! `netlist/gsch/parser.py`.
//!
//! Everything the IR has no field for (positions, wires, polylines, port markers) is
//! stashed as raw JSON so [`super::write_gsch`] can restore it.

use std::collections::{HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};
use oxymorph_schema::{
    Circuit, Directive, ExternalModule, Macro, Module, ModuleReference, Parameter, ParameterValue,
    Port, Properties, SignalDomain, Stimulus,
};
use serde_json::Value;

use super::sim::{
    check_sim, code_macro, element, library, model_definition, port_names, select_entry, Element,
    GROUND,
};
use super::{
    first_truthy, object, python_eq, python_json, python_str, truthy, GschOptions, JsonObject,
};
use crate::error::Error;
use crate::expression::expression_or_raw;
use crate::hdl::propagate_domains;
use crate::helpers::{
    domain_of, is_si_number, net_map_to_connections, parse_parameter_number, NetMap, PinDomains,
};
use crate::stimulus::{add_stimuli, name_stimulus, StimulusIndex};

/// Instance stash: `properties["__info__"]` = `{"__gsch__": {"key", "entry"}}`. The `__`
/// prefix keeps it out of kfnetlist instance equality.
pub const INFO: &str = "__info__";
/// `{"key", "entry"[, "circuit": the parsed module name if not `model`][, "params": param ->
/// the model expression it is derived from][, "computed": param -> the value the simulation
/// view computed from props (source waveforms, a model param named like a prop it reads,
/// …)][, "defaults": model prop defaults the entry does not set][, "pins": IR pin -> entry
/// pin where they differ beyond case][, "tied": IR pins tied to ground, not in the entry]}`
pub const STASH: &str = "__gsch__";
/// The device's nyanlib model (FQN).
pub const MODEL: &str = "__model__";
/// Writers write a module with this name flat.
pub const TOP: &str = "__top__";
/// Deepest `.gsch` hierarchy `read_gsch` follows (the top is level 0).
pub const MAX_GSCH_DEPTH: usize = 64;

/// The Mosaic prop holding the positional `value` of a built-in.
pub fn value_prop_of(kind: &str) -> Option<&'static str> {
    match kind {
        "resistor" => Some("resistance"),
        "capacitor" => Some("capacitance"),
        "inductor" => Some("inductance"),
        _ => None,
    }
}

/// Entry `type` -> the module-level stash list (any other type is a component).
pub fn bucket_of(kind: &str) -> Option<&'static str> {
    match kind {
        "port" => Some("gsch_ports"),
        "wire" | "text" | "group" => Some("gsch_wires"),
        "polyline" => Some("gsch_routes"),
        "net" | "via" | "taper" => Some("gsch_other"),
        _ => None,
    }
}

/// The stash lists, in the order the module stash writes them.
pub const BUCKETS: [&str; 4] = ["gsch_ports", "gsch_wires", "gsch_routes", "gsch_other"];

/// Ground, supply and text markers only name nets; these variants are the circuit's ports.
pub fn is_exposed(entry: &JsonObject) -> bool {
    match entry.get("variant") {
        None | Some(Value::Null) => true,
        Some(Value::String(variant)) => variant == "emitter" || variant == "detector",
        Some(_) => false,
    }
}

fn port_type_domain(kind: Option<&Value>) -> Option<SignalDomain> {
    match kind.and_then(Value::as_str)? {
        "optical" | "photonic" => Some(SignalDomain::Waveguide), // nyanlib: photonic
        "electrical" | "electric" => Some(SignalDomain::Electrical),
        _ => None,
    }
}

/// `{"key": key, "entry": entry}` items per stash list.
pub type Buckets = IndexMap<&'static str, Vec<Value>>;

pub fn empty_buckets() -> Buckets {
    let mut buckets = Buckets::new();
    for name in BUCKETS {
        buckets.insert(name, Vec::new());
    }
    buckets
}

pub fn stash_item(key: &str, entry: &Value) -> Value {
    let mut item = JsonObject::new();
    item.insert("key".to_string(), Value::String(key.to_string()));
    item.insert("entry".to_string(), entry.clone());
    Value::Object(item)
}

pub fn item_key(item: &Value) -> String {
    python_str(item.get("key").unwrap_or(&Value::Null))
}

pub fn item_entry(item: &Value) -> &Value {
    item.get("entry").unwrap_or(&Value::Null)
}

/// The `type` of an entry (None for anything but an object with a string type).
pub fn entry_type(entry: &Value) -> Option<&str> {
    entry.as_object()?.get("type")?.as_str()
}

/// The module an instance entry instantiates: its `model`, else its `type`.
pub fn master_of(entry: &JsonObject) -> String {
    if truthy(entry.get("model")) {
        return python_str(&entry["model"]);
    }
    if truthy(entry.get("type")) {
        return python_str(&entry["type"]);
    }
    "ckt".to_string()
}

/// `(component key, port)` of a PortRef in any of its three forms.
pub fn terminal_ref(terminal: Option<&Value>) -> Option<(String, String)> {
    match terminal? {
        Value::Array(pair) if pair.len() == 2 => Some((python_str(&pair[0]), python_str(&pair[1]))),
        Value::Object(fields) => {
            let key = first_truthy(&[
                fields.get("component_id"),
                fields.get("device"),
                fields.get("instance"),
            ]);
            let pin = first_truthy(&[fields.get("port_name"), fields.get("port")]);
            match (key, pin) {
                (Some(key), Some(pin)) if !key.is_null() && !pin.is_null() => {
                    Some((python_str(key), python_str(pin)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The `terminals` (polyline) or `ports` (net entry) of `entry`.
pub fn terminal_refs(entry: &JsonObject) -> Vec<Option<(String, String)>> {
    let terminals = match entry.get("terminals") {
        Some(found) => Some(found),
        None => entry.get("ports"),
    };
    let mut refs = Vec::new();
    match terminals {
        Some(Value::Object(fields)) => {
            for terminal in fields.values() {
                refs.push(terminal_ref(Some(terminal)));
            }
        }
        Some(Value::Array(items)) => {
            for terminal in items {
                refs.push(terminal_ref(Some(terminal)));
            }
        }
        _ => {}
    }
    refs
}

/// The JSON object items of an entry list field (`ports`).
fn object_items(value: Option<&Value>) -> Vec<&JsonObject> {
    let mut items = Vec::new();
    if let Some(Value::Array(list)) = value {
        for item in list {
            if let Value::Object(fields) = item {
                items.push(fields);
            }
        }
    }
    items
}

/// The `name` of a port object (`p["name"]`).
fn named_port(port: &JsonObject) -> Option<String> {
    port.get("name").map(python_str)
}

/// A param from a JSON prop: an SI-prefixed number becomes a value; anything else an
/// expression (raw if it does not parse).
pub fn parameter(name: &str, raw: &Value) -> Parameter {
    let (text, scalar) = match raw {
        Value::Number(_) => (python_str(raw), true),
        Value::String(text) => (text.clone(), true),
        _ => (python_json(raw, None), false),
    };
    let default_value = if scalar && is_si_number(&text) {
        parse_parameter_number(&text).unwrap_or_default()
    } else {
        ParameterValue {
            expression: Some(expression_or_raw(&text)),
            ..Default::default()
        }
    };
    Parameter {
        name: name.to_string(),
        default_value: Some(default_value),
        ..Default::default()
    }
}

/// The IR params of a component, recording in `stash` which are not its props.
///
/// A model with `params` makes each an expression over the instance's source props
/// (`w = width * 1e-6`), which become `role=source` params; a bare rename of a prop
/// (`m = m`) is that prop. Writers resolve the references (nyancad substitutes them
/// textually). A model param named like a prop it reads (`w = w * 1e-6`) keeps its
/// substituted value: two params of one name would not survive the kfnetlist bridge. So does
/// any expression reading such a prop (`ad = w * 0.5`), since `w` then names the device
/// param.
fn parameters(
    entry: &JsonObject,
    device: Option<&Element>,
    stash: &mut JsonObject,
) -> Vec<Parameter> {
    let empty = JsonObject::new();
    let given = object(entry.get("props")).unwrap_or(&empty);
    let Some(device) = device else {
        // not simulated: the params are the props
        let mut out = Vec::new();
        for (name, value) in given {
            out.push(parameter(name, value));
        }
        return out;
    };
    let mut collide = HashSet::new();
    for (name, expression) in &device.expressions {
        if device.sources.contains_key(name) && expression.trim() != name {
            collide.insert(name.as_str());
        }
    }
    let is_value_symbol = entry
        .get("type")
        .and_then(Value::as_str)
        .and_then(value_prop_of)
        .is_some();
    let mut out = Vec::new();
    let mut expressions = JsonObject::new();
    let mut computed = JsonObject::new();
    for (name, value) in &device.params {
        match device.expressions.get(name) {
            Some(expression) if expression.trim() != name => {
                expressions.insert(name.clone(), Value::String(expression.clone()));
                if !device.sources.contains_key(name) && !reads_any(expression, &collide) {
                    out.push(parameter(name, &Value::String(expression.clone())));
                    continue;
                }
                // `w = w * 1e-6`: its value; the prop `w` has the name
                computed.insert(name.clone(), value.clone());
            }
            _ => {
                let is_prop = given.get(name).is_some_and(|prop| python_eq(prop, value));
                let is_default = device
                    .defaults
                    .get(name)
                    .is_some_and(|default| python_eq(default, value));
                let is_value = name == "value" && is_value_symbol; // resistance/…
                if !(is_prop || is_default || is_value) {
                    computed.insert(name.clone(), value.clone()); // waveforms, $mfactor, …
                }
            }
        }
        out.push(parameter(name, value));
    }
    for (name, value) in &device.sources {
        if !device.params.contains_key(name) {
            let mut source = parameter(name, value); // read by model expressions
            source
                .properties
                .insert("role".to_string(), "source".to_string());
            out.push(source);
        }
    }
    let mut names = HashSet::new();
    for parameter in &out {
        names.insert(parameter.name.clone());
    }
    let mut defaults = JsonObject::new();
    for (name, value) in &device.defaults {
        if names.contains(name) {
            defaults.insert(name.clone(), value.clone());
        }
    }
    for (field, value) in [
        ("params", expressions),
        ("computed", computed),
        ("defaults", defaults),
    ] {
        if !value.is_empty() {
            stash.insert(field.to_string(), Value::Object(value));
        }
    }
    out
}

/// Whether `expression` reads any of `names` (nyancad's word substitution).
fn reads_any(expression: &str, names: &HashSet<&str>) -> bool {
    for word in super::sim::identifier_re().find_iter(expression) {
        if names.contains(word.as_str()) {
            return true;
        }
    }
    false
}

/// `(entry key, pin)`; key None is a named net, `Some("")` a top pin.
pub type Node = (Option<String>, String);

/// Union-find over nodes, in the order they are first seen (Python's `parent` dict).
#[derive(Default)]
struct UnionFind {
    nodes: IndexSet<Node>,
    parent: Vec<usize>,
}

impl UnionFind {
    /// `parent.setdefault(node, node)`.
    fn index(&mut self, node: &Node) -> usize {
        if let Some(index) = self.nodes.get_index_of(node) {
            return index;
        }
        self.nodes.insert(node.clone());
        self.parent.push(self.parent.len());
        self.parent.len() - 1
    }

    fn root(&mut self, mut index: usize) -> usize {
        while self.parent[index] != index {
            self.parent[index] = self.parent[self.parent[index]];
            index = self.parent[index];
        }
        index
    }

    fn find(&mut self, node: &Node) -> usize {
        let index = self.index(node);
        self.root(index)
    }

    /// `parent[find(a)] = find(b)`: Python evaluates `find(b)` first.
    fn union(&mut self, first: &Node, second: &Node) {
        let second_root = self.find(second);
        let first_root = self.find(first);
        self.parent[first_root] = second_root;
    }
}

fn entry_node(key: &str, pin: &str) -> Node {
    (Some(key.to_string()), pin.to_string())
}

fn net_node(net: &str) -> Node {
    (None, net.to_string())
}

/// The net names: Mosaic `nets` names and net entries bind on their own; polyline `net`
/// fields and marker `nets` beside an `attached_port` only name a net nothing else names.
struct NetBuilder {
    union_find: UnionFind,
    hints: Vec<(Node, String)>,
}

impl NetBuilder {
    /// Merge `refs`; `net` names them (`strong`: like a Mosaic net name).
    fn join(&mut self, refs: &[Option<(String, String)>], net: Option<&Value>, strong: bool) {
        let mut nodes = Vec::new();
        for (key, pin) in refs.iter().flatten() {
            nodes.push(entry_node(key, pin));
        }
        for pair in nodes.windows(2) {
            self.union_find.union(&pair[0], &pair[1]);
        }
        let Some(first) = nodes.first() else {
            return;
        };
        self.union_find.find(first);
        if truthy(net) {
            let name = python_str(net.unwrap_or_default());
            if strong {
                self.union_find.union(first, &net_node(&name));
            } else {
                self.hints.push((first.clone(), name));
            }
        }
    }
}

/// Nets (name -> `(key, pin)` members) and exposed top pins (marker name -> entry).
///
/// Nets come from Mosaic `nets` maps, Livewire polylines, `net` entries and port markers;
/// `via`/`taper` entries join everything that ends on them. A net is named by its Mosaic
/// net name, else a polyline `net`, else `net_<n>`. A single pin is a net only under a
/// Mosaic or net-entry name (nyancad writes it).
pub fn find_nets(
    comps: &IndexMap<String, &JsonObject>,
    buckets: &Buckets,
    names: &IndexMap<String, String>,
) -> (IndexMap<String, Vec<Node>>, IndexMap<String, JsonObject>) {
    let mut builder = NetBuilder {
        union_find: UnionFind::default(),
        hints: Vec::new(),
    };
    for (key, entry) in comps {
        if let Some(nets) = object(entry.get("nets")) {
            for (pin, net) in nets {
                if truthy(Some(net)) {
                    builder
                        .union_find
                        .union(&entry_node(key, pin), &net_node(&python_str(net)));
                }
            }
        }
    }
    let mut top_pins: IndexMap<String, JsonObject> = IndexMap::new();
    for item in &buckets["gsch_ports"] {
        let Some(entry) = item_entry(item).as_object() else {
            continue;
        };
        let net = object(entry.get("nets")).and_then(|nets| nets.get("P"));
        if !is_exposed(entry) {
            if truthy(net) {
                builder
                    .union_find
                    .find(&net_node(&python_str(net.unwrap_or_default())));
            }
            continue;
        }
        let pin_name = match first_truthy(&[entry.get("name"), None]) {
            Some(name) => python_str(name),
            None => item_key(item),
        };
        let pin: Node = (Some(String::new()), pin_name.clone());
        top_pins
            .entry(pin_name.clone())
            .or_insert_with(|| entry.clone());
        let attached = terminal_ref(entry.get("attached_port"));
        if let Some(attached) = attached {
            // attached_port wins; `nets` only names the net
            builder.join(
                &[Some((String::new(), pin_name)), Some(attached)],
                net,
                false,
            );
        } else if truthy(net) {
            builder
                .union_find
                .union(&pin, &net_node(&python_str(net.unwrap_or_default())));
        } else {
            builder.union_find.find(&pin);
        }
    }
    for item in &buckets["gsch_routes"] {
        if let Some(entry) = item_entry(item).as_object() {
            builder.join(&terminal_refs(entry), entry.get("net"), false);
        }
    }
    let mut junctions = Vec::new();
    for item in &buckets["gsch_other"] {
        let entry = item_entry(item);
        match (entry_type(entry), entry.as_object()) {
            (Some("net"), Some(fields)) => {
                // the entry's key (or id) is the net name
                let key = Value::String(item_key(item));
                let name = first_truthy(&[fields.get("id"), fields.get("_id"), Some(&key)]);
                builder.join(&terminal_refs(fields), name, true);
            }
            (Some("via") | Some("taper"), _) => junctions.push(item_key(item)),
            _ => {}
        }
    }
    // everything ending on a via/taper is one net
    let mut touching: HashMap<String, Vec<Node>> = HashMap::new();
    for node in &builder.union_find.nodes {
        if let Some(key) = &node.0 {
            touching.entry(key.clone()).or_default().push(node.clone());
        }
    }
    let mut seen_junctions = HashSet::new();
    for key in junctions {
        if !seen_junctions.insert(key.clone()) {
            continue;
        }
        if let Some(nodes) = touching.get(&key) {
            for pair in nodes.windows(2) {
                builder.union_find.union(&pair[0], &pair[1]);
            }
        }
    }

    let mut members: IndexMap<usize, Vec<Node>> = IndexMap::new();
    let mut named: HashMap<usize, String> = HashMap::new();
    let all_nodes: Vec<Node> = builder.union_find.nodes.iter().cloned().collect();
    for node in all_nodes {
        let root = builder.union_find.find(&node);
        match &node.0 {
            None => {
                named.entry(root).or_insert_with(|| node.1.clone());
            }
            Some(key) if key.is_empty() || names.contains_key(key) => {
                members.entry(root).or_default().push(node);
            }
            Some(_) => {}
        }
    }
    let mut taken: HashSet<String> = named.values().cloned().collect();
    let strong: HashSet<usize> = named.keys().copied().collect();
    let hints = std::mem::take(&mut builder.hints);
    for (node, net) in hints {
        let root = builder.union_find.find(&node);
        if !named.contains_key(&root) && !taken.contains(&net) {
            named.insert(root, net.clone());
            taken.insert(net);
        }
    }

    let mut nets = IndexMap::new();
    let mut counter = 0;
    for (root, nodes) in members {
        if nodes.len() < 2 && !strong.contains(&root) {
            continue; // a lone pin keeps its label only
        }
        let name = named.entry(root).or_insert_with(|| {
            while taken.contains(&format!("net_{}", counter + 1)) {
                counter += 1;
            }
            counter += 1;
            let fresh = format!("net_{counter}");
            taken.insert(fresh.clone());
            fresh
        });
        nets.insert(name.clone(), nodes);
    }
    (nets, top_pins)
}

/// The nyanlib entry of a model: `models:<model>`, else `<model>`.
fn nyanlib_entry<'a>(models: &'a JsonObject, model: &str) -> Option<&'a JsonObject> {
    let found = first_truthy(&[models.get(&format!("models:{model}")), models.get(model)]);
    object(found)
}

/// Pin order from nyanlib: a model's `port-order`, else the `ports` order.
fn model_pins(models: &JsonObject, model: &str) -> Vec<String> {
    let Some(entry) = nyanlib_entry(models, model) else {
        return Vec::new();
    };
    for variant in object_items(entry.get("models")) {
        if let Some(Value::Array(order)) = variant.get("port-order") {
            if !order.is_empty() {
                return order.iter().map(python_str).collect();
            }
        }
    }
    port_names(entry.get("ports"))
}

fn model_domains(models: &JsonObject, model: &str) -> IndexMap<String, SignalDomain> {
    let mut found = IndexMap::new();
    if let Some(entry) = nyanlib_entry(models, model) {
        for port in object_items(entry.get("ports")) {
            if let (Some(name), Some(domain)) =
                (named_port(port), port_type_domain(port.get("type")))
            {
                found.insert(name, domain);
            }
        }
    }
    found
}

/// The simulation view of each component, by entry key.
struct Devices<'a> {
    comps: &'a IndexMap<String, &'a JsonObject>,
    devices: IndexMap<String, Option<Element>>,
}

impl Devices<'_> {
    fn device(&self, key: &str) -> Option<&Element> {
        self.devices.get(key).and_then(Option::as_ref)
    }

    /// The module the component `key` instantiates.
    fn master(&self, key: &str) -> String {
        match self.device(key) {
            Some(device) => device.module_name.clone(),
            None => master_of(self.comps[key]),
        }
    }

    /// The IR pin of the component's `pin`.
    fn ir_pin(&self, key: &str, pin: &str) -> String {
        match self.device(key) {
            Some(device) => device.pin(pin),
            None => pin.to_string(),
        }
    }
}

/// A JSON document as an object, or the reason it is not one.
fn document(text: &str) -> Result<JsonObject, Error> {
    match serde_json::from_str(text).map_err(json_error)? {
        Value::Object(fields) => Ok(fields),
        _ => Err(Error::Validation(".gsch must be a JSON object".to_string())),
    }
}

fn json_error(error: serde_json::Error) -> Error {
    Error::Parse {
        message: error.to_string(),
        line: error.line(),
        column: error.column(),
    }
}

fn models_object(models: &Value) -> Result<JsonObject, Error> {
    match models {
        Value::Object(fields) => Ok(fields.clone()),
        _ => Err(Error::Validation(
            "models must be a JSON object of nyanlib models".to_string(),
        )),
    }
}

/// Parse `.gsch` JSON text into a circuit with one module (the schematic).
///
/// Exposed port markers (no `variant`, `emitter`, `detector`) are the module's ports.
/// Devices are resolved for `options.sim` the way nyancad's netlister does: model entry,
/// pins, params, and the model libraries as LIB/INCLUDE directives; devices nyancad would
/// not netlist get `sim_skip`. The net `GND` is ground. The key prefix (`demo` for
/// `demo:R1`) is kept as the module's `gsch_schema` info.
pub fn parse_gsch(text: &str, options: &GschOptions) -> Result<Circuit, Error> {
    let sim = check_sim(&options.sim)?;
    let models = match &options.models {
        Some(models) => models_object(models)?,
        None => JsonObject::new(),
    };
    parse_document(
        text,
        &models,
        &sim,
        &options.corners,
        options.name.as_deref(),
    )
}

/// Instance name = `name` else key, made unique with `_<n>`; the key stays the reference id.
fn instance_names(comps: &IndexMap<String, &JsonObject>) -> IndexMap<String, String> {
    let mut names: IndexMap<String, String> = IndexMap::new();
    let mut used: HashSet<String> = HashSet::new();
    for (key, entry) in comps {
        let mut candidate = match first_truthy(&[entry.get("name"), None]) {
            Some(name) => python_str(name),
            None => key.clone(),
        };
        if used.contains(&candidate) {
            candidate = key.clone();
        }
        let mut suffix = 1;
        while used.contains(&candidate) {
            candidate = format!("{key}_{suffix}");
            suffix += 1;
        }
        used.insert(candidate.clone());
        names.insert(key.clone(), candidate);
    }
    names
}

/// The external modules instances use (pins in order of first use), and the pin domains
/// `propagate_domains` starts from.
fn external_modules(
    comps: &IndexMap<String, &JsonObject>,
    simulated: &Devices,
    models: &JsonObject,
    pins_by_key: &HashMap<&str, Vec<&str>>,
) -> (Vec<ExternalModule>, PinDomains) {
    let mut pins_seen: IndexMap<String, IndexSet<String>> = IndexMap::new(); // first-use order
    let mut device_domains: HashMap<String, IndexMap<String, SignalDomain>> = HashMap::new();
    let mut fqn: HashMap<String, String> = HashMap::new(); // master -> nyanlib model of its pins
    let mut prefix: HashMap<String, String> = HashMap::new(); // master -> SPICE letter
    for device in simulated.devices.values().flatten() {
        pins_seen
            .entry(device.module_name.clone())
            .or_insert_with(|| device.ports.iter().cloned().collect());
        prefix
            .entry(device.module_name.clone())
            .or_insert_with(|| device.prefix.clone());
    }
    let fixed: HashSet<String> = pins_seen.keys().cloned().collect();
    for (key, entry) in comps {
        let cell = simulated.master(key);
        pins_seen.entry(cell.clone()).or_default();
        let model = if truthy(entry.get("model")) {
            python_str(&entry["model"])
        } else {
            cell.clone()
        };
        fqn.entry(cell.clone()).or_insert(model);
        let is_fixed = fixed.contains(&cell);
        let mut open_pins = if is_fixed {
            Vec::new()
        } else {
            model_pins(models, &cell)
        };
        for port in object_items(entry.get("ports")) {
            let Some(port_name) = named_port(port) else {
                continue;
            };
            if let Some(domain) = port_type_domain(port.get("port_type")) {
                device_domains
                    .entry(cell.clone())
                    .or_default()
                    .entry(simulated.ir_pin(key, &port_name))
                    .or_insert(domain);
            }
            open_pins.push(port_name);
        }
        for pin in pins_by_key.get(key.as_str()).into_iter().flatten() {
            open_pins.push(pin.to_string());
        }
        if !is_fixed {
            pins_seen[&cell].extend(open_pins);
        }
    }

    let mut pin_domains = PinDomains::new();
    let mut ext_modules = Vec::new();
    for (cell, seen) in &pins_seen {
        let nyanlib = model_domains(models, &fqn[cell]);
        let mut domains = IndexMap::new();
        let mut ports = Vec::new();
        for pin in seen {
            let domain = device_domains
                .get(cell)
                .and_then(|found| found.get(pin))
                .or_else(|| nyanlib.get(pin))
                .copied()
                .unwrap_or_else(|| domain_of(pin));
            domains.insert(pin.clone(), domain);
            ports.push(Port {
                name: pin.clone(),
                domain,
                ..Default::default()
            });
        }
        pin_domains.insert(cell.clone(), domains);
        let mut properties = Properties::new();
        if let Some(letter) = prefix.get(cell).filter(|letter| !letter.is_empty()) {
            properties.insert("spice_prefix".to_string(), letter.clone());
        }
        ext_modules.push(ExternalModule {
            name: cell.clone(),
            ports,
            properties,
            ..Default::default()
        });
    }
    (ext_modules, pin_domains)
}

/// The instance of the component `key`, with its stash, and its named stimulus.
fn instance_reference(
    key: &str,
    entry: &JsonObject,
    simulated: &Devices,
    name: &str,
    tied: Option<&Vec<String>>,
    module_name: &str,
) -> (ModuleReference, Option<Stimulus>) {
    let device = simulated.device(key);
    let mut stash = JsonObject::new();
    stash.insert("key".to_string(), Value::String(key.to_string()));
    stash.insert("entry".to_string(), Value::Object(entry.clone()));
    let mut properties = Properties::new();
    match device {
        None => {
            properties.insert("sim_skip".to_string(), "1".to_string());
        }
        Some(device) if !device.model_name.is_empty() => {
            properties.insert("model_name".to_string(), device.model_name.clone());
        }
        Some(_) => {}
    }
    let cell = simulated.master(key);
    if cell != master_of(entry) {
        stash.insert("circuit".to_string(), Value::String(cell.clone()));
    }
    let mut entry_pins: Vec<String> = Vec::new();
    if let Some(nets) = object(entry.get("nets")) {
        entry_pins.extend(nets.keys().cloned());
    }
    for port in object_items(entry.get("ports")) {
        entry_pins.extend(named_port(port));
    }
    let mut renamed = JsonObject::new();
    for pin in &entry_pins {
        renamed.insert(simulated.ir_pin(key, pin), Value::String(pin.clone()));
    }
    renamed.retain(|ir, pin| ir.to_lowercase() != python_str(pin).to_lowercase());
    if !renamed.is_empty() {
        // SPICE pins (`p`/`n`) of a device with its own (`A`/`B`)
        stash.insert("pins".to_string(), Value::Object(renamed));
    }
    if let Some(pins) = tied {
        let tied_pins = pins.iter().cloned().map(Value::String).collect();
        stash.insert("tied".to_string(), Value::Array(tied_pins));
    }
    let parameters = parameters(entry, device, &mut stash);
    let mut info = JsonObject::new();
    info.insert(STASH.to_string(), Value::Object(stash));
    if truthy(entry.get("model")) {
        info.insert(MODEL.to_string(), entry["model"].clone());
    }
    let mut all_properties = Properties::new();
    all_properties.insert(INFO.to_string(), python_json(&Value::Object(info), None));
    all_properties.extend(properties);
    let mut reference = ModuleReference {
        name: name.to_string(),
        module_name: cell,
        parameters,
        properties: all_properties,
        ..Default::default()
    };
    let stimulus = device.and_then(|found| found.stimulus.clone());
    let named = stimulus.map(|found| name_stimulus(module_name, &mut reference, found));
    (reference, named)
}

fn parse_document(
    text: &str,
    given_models: &JsonObject,
    sim: &str,
    corners: &[String],
    name: Option<&str>,
) -> Result<Circuit, Error> {
    let mut doc = document(text)?;
    let inline = doc.shift_remove("models");
    let mut models = match &inline {
        Some(Value::Object(fields)) => fields.clone(),
        _ => JsonObject::new(),
    };
    for (key, value) in given_models {
        models.insert(key.clone(), value.clone());
    }

    let mut buckets = empty_buckets();
    let mut comps: IndexMap<String, &JsonObject> = IndexMap::new();
    for (key, entry) in &doc {
        let bucket = entry_type(entry).and_then(bucket_of);
        match (bucket, entry) {
            (Some(bucket), _) => buckets[bucket].push(stash_item(key, entry)),
            (None, Value::Object(fields)) => {
                comps.insert(key.clone(), fields);
            }
            (None, _) => buckets["gsch_other"].push(stash_item(key, entry)),
        }
    }

    let names = instance_names(&comps);
    let mut simulated = Devices {
        comps: &comps,
        devices: IndexMap::new(),
    };
    for (key, entry) in &comps {
        let device = element(entry, &models, sim, key)?;
        simulated.devices.insert(key.clone(), device);
    }

    let (nets, top_pins) = find_nets(&comps, &buckets, &names);
    let mut net_map = NetMap::new();
    let mut pins_by_key: HashMap<&str, Vec<&str>> = HashMap::new();
    for (net, nodes) in &nets {
        for (key, pin) in nodes {
            let key = key.as_deref().unwrap_or("");
            let instance = names.get(key).map(String::as_str).unwrap_or("");
            net_map.add(net, instance, &simulated.ir_pin(key, pin));
            pins_by_key.entry(key).or_default().push(pin);
        }
    }
    // an open MOS bulk: nyancad ties it to ground
    let mut tied: HashMap<String, Vec<String>> = HashMap::new();
    for (key, device) in &simulated.devices {
        let Some(device) = device else {
            continue;
        };
        let mut connected = HashSet::new();
        for pin in pins_by_key.get(key.as_str()).into_iter().flatten() {
            connected.insert(simulated.ir_pin(key, pin));
        }
        for pin in &device.ground {
            if !connected.contains(pin) {
                net_map.add(GROUND, &names[key], pin);
                tied.entry(key.clone()).or_default().push(pin.clone());
            }
        }
    }

    let (ext_modules, mut pin_domains) =
        external_modules(&comps, &simulated, &models, &pins_by_key);

    let module_name = name.unwrap_or(TOP).to_string();
    let mut references = Vec::new();
    let mut stimuli: Vec<Stimulus> = Vec::new();
    for (key, entry) in &comps {
        let (reference, stimulus) = instance_reference(
            key,
            entry,
            &simulated,
            &names[key],
            tied.get(key),
            &module_name,
        );
        references.push(reference);
        stimuli.extend(stimulus);
    }

    // ── the module: exposed port markers are its terminals
    let mut prefixes = IndexSet::new();
    for key in doc.keys() {
        if let Some((schema_prefix, _)) = key.split_once(':') {
            prefixes.insert(schema_prefix);
        }
    }
    let schema = match prefixes.len() {
        1 => Value::String(prefixes[0].to_string()),
        _ => Value::Null,
    };
    let mut ports = Vec::new();
    for (port_name, entry) in &top_pins {
        let attached = terminal_ref(entry.get("attached_port"));
        let mut domain = match entry.get("nature").and_then(Value::as_str) {
            Some("photonic") => Some(SignalDomain::Waveguide),
            Some("electric") => Some(SignalDomain::Electrical),
            _ => None,
        };
        if let (None, Some((key, pin))) = (domain, &attached) {
            if comps.contains_key(key) {
                domain = pin_domains[&simulated.master(key)]
                    .get(&simulated.ir_pin(key, pin))
                    .copied();
            }
        }
        ports.push(Port {
            name: port_name.clone(),
            domain: domain.unwrap_or_else(|| domain_of(port_name)),
            ..Default::default()
        });
    }
    let mut top_domains = IndexMap::new();
    for port in &ports {
        top_domains.insert(port.name.clone(), port.domain);
    }
    pin_domains.insert(module_name.clone(), top_domains);

    let mut module_info = JsonObject::new();
    module_info.insert("gsch_schema".to_string(), schema);
    for (bucket, items) in buckets {
        module_info.insert(bucket.to_string(), Value::Array(items));
    }
    if let Some(Value::Object(_)) = &inline {
        module_info.insert(
            "gsch_models".to_string(),
            inline.clone().unwrap_or_default(),
        );
    }
    let mut module_properties = Properties::new();
    module_properties.insert(
        INFO.to_string(),
        python_json(&Value::Object(module_info), None),
    );
    let module = Module {
        name: module_name.clone(),
        ports,
        module_references: references,
        connections: net_map_to_connections(&net_map), // a named single pin too
        properties: module_properties,
        ..Default::default()
    };
    let mut properties = Properties::new();
    properties.insert("ground_net".to_string(), GROUND.to_string());
    if sim == "vacask" {
        properties.insert("dialect".to_string(), "vacask".to_string());
    }
    let (directives, macros) = libraries(&comps, &models, sim, corners);
    let mut circuit = Circuit {
        top_module: module_name,
        modules: vec![module],
        ext_modules,
        directives,
        macros,
        properties,
        ..Default::default()
    };
    add_stimuli(&mut circuit, stimuli);
    propagate_domains(&mut circuit, &IndexMap::new(), Some(&pin_domains));
    Ok(circuit)
}

/// Library directives and inline-code macros (equal ones once) of the models nyancad
/// counts as used (devices with nets).
fn libraries(
    comps: &IndexMap<String, &JsonObject>,
    models: &JsonObject,
    sim: &str,
    corners: &[String],
) -> (Vec<Directive>, Vec<Macro>) {
    let mut directives = Vec::new();
    let mut macros = Vec::new();
    for entry in comps.values() {
        let Some(model_def) = model_definition(entry, models) else {
            continue;
        };
        if !truthy(entry.get("nets")) {
            continue;
        }
        let Some(chosen) = select_entry(model_def, sim) else {
            continue;
        };
        if let Some(directive) = library(chosen, sim, corners) {
            if !directives.contains(&directive) {
                directives.push(directive);
            }
        }
        if let Some(code) = code_macro(chosen) {
            add_macro(&mut macros, code);
        }
    }
    (directives, macros)
}

/// Append `code` unless an equal macro is there (kfnetlist warns on a same-named one with
/// other code; both are kept).
fn add_macro(macros: &mut Vec<Macro>, code: Macro) {
    if !macros.contains(&code) {
        macros.push(code);
    }
}

/// `std::path::Path::file_stem` of a model path, as Python's `Path(model).stem`.
#[cfg(not(target_arch = "wasm32"))]
fn file_stem(model: &str) -> String {
    std::path::Path::new(model)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads a `.gsch` hierarchy: every loaded file's circuit, children before parents.
#[cfg(not(target_arch = "wasm32"))]
struct HierarchyReader<'a> {
    root: String,
    models: JsonObject,
    sim: String,
    corners: &'a [String],
    /// Model path (as written) -> index into `loaded` of its circuit.
    children: IndexMap<String, usize>,
    loaded: Vec<Circuit>,
    /// The files being read, outermost first: a file met again is a cycle.
    stack: Vec<(std::path::PathBuf, String)>,
}

#[cfg(not(target_arch = "wasm32"))]
impl HierarchyReader<'_> {
    fn load(&mut self, file: &str, fqn: Option<&str>) -> Result<usize, Error> {
        let io_error = |error: std::io::Error| Error::Validation(format!("{file}: {error}"));
        let canonical = std::fs::canonicalize(file).map_err(io_error)?;
        let mut chain = Vec::new();
        for (_, shown) in &self.stack {
            chain.push(shown.as_str());
        }
        chain.push(file);
        if self.stack.iter().any(|(open, _)| *open == canonical) {
            return Err(Error::Validation(format!(
                "{file}: the schematic instantiates itself ({})",
                chain.join(" -> ")
            )));
        }
        if self.stack.len() > MAX_GSCH_DEPTH {
            return Err(Error::Validation(format!(
                "{file}: .gsch schematics nest deeper than {MAX_GSCH_DEPTH} levels"
            )));
        }
        self.stack.push((canonical, file.to_string()));
        let text = std::fs::read_to_string(file).map_err(io_error)?;
        let doc: Value = serde_json::from_str(&text).map_err(json_error)?;
        if let Value::Object(entries) = &doc {
            for entry in entries.values() {
                if let Value::Object(fields) = entry {
                    self.load_child(file, fields)?;
                }
            }
        }
        let circuit = parse_document(&text, &self.models, &self.sim, self.corners, fqn)?;
        self.loaded.push(circuit);
        self.stack.pop();
        Ok(self.loaded.len() - 1)
    }

    /// Read the child schematic `entry` instantiates, when nyancad would netlist it: a wired
    /// device whose `model` ends in `.gsch` and is no library model.
    fn load_child(&mut self, file: &str, entry: &JsonObject) -> Result<(), Error> {
        let Some(model) = entry.get("model").and_then(Value::as_str) else {
            return Ok(());
        };
        let key = format!("models:{model}");
        let library_model =
            object(self.models.get(&key)).is_some_and(|found| truthy(found.get("models")));
        if !model.ends_with(".gsch")
            || !truthy(entry.get("nets")) // nyancad netlists only wired devices
            || library_model
            || self.children.contains_key(model)
        {
            return Ok(());
        }
        // as kfnetlist does, an absolute or `..` model path may leave the project root
        let child = oxymorph_schema::paths::resolve_file(model, Some(&self.root));
        if !std::path::Path::new(&child).is_file() {
            return Err(Error::Validation(format!(
                "{file}: child schematic {child} not found"
            )));
        }
        let index = self.load(&child, Some(model))?;
        let model_def = object(self.models.get(&key)).cloned().unwrap_or_default();
        let ports = match model_def.get("ports") {
            Some(ports) if truthy(Some(ports)) => ports.clone(),
            _ => {
                let mut listed = Vec::new();
                for port in &self.loaded[index].modules[0].ports {
                    let mut fields = JsonObject::new();
                    fields.insert("name".to_string(), Value::String(port.name.clone()));
                    listed.push(Value::Object(fields));
                }
                Value::Array(listed)
            }
        };
        let name = match model_def.get("name") {
            Some(name) if truthy(Some(name)) => python_str(name),
            _ => file_stem(model),
        };
        for (other, other_index) in &self.children {
            if self.loaded[*other_index].modules[0].name == name {
                let other_path = oxymorph_schema::paths::resolve_file(other, Some(&self.root));
                return Err(Error::Validation(format!(
                    "{child} and {other_path} are both circuit {}",
                    super::python_repr(&name)
                )));
            }
        }
        let mut definition = model_def.clone();
        definition.insert("name".to_string(), Value::String(name.clone()));
        definition.insert("ports".to_string(), ports.clone());
        self.models.insert(key, Value::Object(definition));

        let module = &mut self.loaded[index].modules[0];
        let mut by_name = HashMap::new();
        for port in &module.ports {
            by_name.insert(port.name.clone(), port.clone());
        }
        let mut sorted = port_names(Some(&ports));
        sorted.sort();
        let mut module_ports = Vec::new();
        for port_name in sorted {
            module_ports.push(by_name.get(&port_name).cloned().unwrap_or_else(|| Port {
                domain: domain_of(&port_name),
                name: port_name,
                ..Default::default()
            }));
        }
        let mut header = Vec::new();
        for prop in object_items(model_def.get("props")) {
            if !truthy(prop.get("name")) {
                continue;
            }
            let zero = Value::String("0".to_string());
            let default = match prop.get("default") {
                None | Some(Value::Null) => &zero,
                Some(value) => value,
            };
            header.push(parameter(&python_str(&prop["name"]), default));
        }
        module.name = name;
        module.ports = module_ports;
        module.parameters = header;
        self.children.insert(model.to_string(), index);
        Ok(())
    }
}

/// The nyanlib models of a project: `build/models.nyanlib`, else `models.nyanlib`.
#[cfg(not(target_arch = "wasm32"))]
fn project_models(root: &str) -> Result<JsonObject, Error> {
    for directory in [format!("{root}/build"), root.to_string()] {
        let path = std::path::Path::new(&directory).join("models.nyanlib");
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| Error::Validation(format!("{}: {error}", path.display())))?;
            let value: Value = serde_json::from_str(&text).map_err(json_error)?;
            return models_object(&value);
        }
    }
    Ok(JsonObject::new())
}

/// Read a `.gsch` file and the `.gsch` schematics its devices instantiate.
///
/// Each child becomes a module named by its nyanlib `name` (else the file stem), with the
/// model's ports sorted by name as pins and its prop defaults as header parameters, like
/// nyancad's subcircuits. Only wired devices (with Mosaic `nets`) whose `model` ends in
/// `.gsch` and is not a library model (no nyanlib `models` entries) are followed, resolved
/// against `project_root` (default: the file's directory). Libraries and inline-code macros
/// of the whole hierarchy are gathered on the circuit, top first. `options.models` defaults
/// to the project's `build/models.nyanlib`, else `models.nyanlib`; `options.name` is not
/// used (the top is `__top__`).
///
/// `Error::Validation`: a child file is missing, two children share a module name, a
/// schematic instantiates itself (directly or not), or the hierarchy is deeper than
/// [`MAX_GSCH_DEPTH`].
#[cfg(not(target_arch = "wasm32"))]
pub fn read_gsch(
    path: &str,
    project_root: Option<&str>,
    options: &GschOptions,
) -> Result<Circuit, Error> {
    let sim = check_sim(&options.sim)?;
    let root = match project_root {
        Some(root) => root.to_string(),
        None => std::path::Path::new(path)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|parent| !parent.is_empty())
            .unwrap_or_else(|| ".".to_string()),
    };
    let models = match &options.models {
        Some(models) => models_object(models)?,
        None => project_models(&root)?,
    };
    let mut reader = HierarchyReader {
        root,
        models,
        sim,
        corners: &options.corners,
        children: IndexMap::new(),
        loaded: Vec::new(),
        stack: Vec::new(),
    };
    reader.load(path, None)?;
    let mut loaded = reader.loaded;
    let mut top = loaded.pop().unwrap_or_default();
    // every other loaded circuit is a child, in the order they finished loading
    let mut modules = Vec::new();
    for circuit in &loaded {
        modules.extend(circuit.modules.first().cloned());
    }
    modules.extend(top.modules.first().cloned());
    let defined: HashSet<String> = modules.iter().map(|module| module.name.clone()).collect();
    let mut ext_modules: IndexMap<String, ExternalModule> = IndexMap::new();
    let mut directives: Vec<Directive> = Vec::new();
    let mut macros: Vec<Macro> = Vec::new();
    for circuit in std::iter::once(&top).chain(loaded.iter()) {
        // top first
        for ext_module in &circuit.ext_modules {
            if !defined.contains(&ext_module.name) {
                ext_modules
                    .entry(ext_module.name.clone())
                    .or_insert_with(|| ext_module.clone());
            }
        }
        for directive in &circuit.directives {
            if !directives.contains(directive) {
                directives.push(directive.clone());
            }
        }
        for code in &circuit.macros {
            add_macro(&mut macros, code.clone());
        }
    }
    // named again after the children's module names
    let mut sources = Vec::new();
    for (module_index, circuit) in loaded.iter().chain(std::iter::once(&top)).enumerate() {
        let stimuli = StimulusIndex::new(circuit);
        for (reference_index, reference) in
            modules[module_index].module_references.iter().enumerate()
        {
            if let Some(stimulus) = stimuli.of(reference) {
                sources.push((module_index, reference_index, stimulus.clone()));
            }
        }
    }
    top.simulation = None;
    let mut named_sources = Vec::new();
    for (module_index, reference_index, stimulus) in sources {
        let module = &mut modules[module_index];
        let module_name = module.name.clone();
        named_sources.push(name_stimulus(
            &module_name,
            &mut module.module_references[reference_index],
            stimulus,
        ));
    }
    add_stimuli(&mut top, named_sources);
    top.modules = modules;
    top.ext_modules = ext_modules.into_values().collect();
    top.directives = directives;
    top.macros = macros;
    Ok(top)
}
