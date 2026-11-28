//! Write a circuit as a gdsfactory+ `.gsch` schematic (nyancir JSON), ported from kfnetlist
//! `netlist/gsch/writer.py`.
//!
//! Entries the parser stashed (positions, wires, polylines, port markers) are restored;
//! everything else is synthesized in the shape Mosaic and Livewire accept.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use oxymorph_schema::{Circuit, Module, ModuleReference, Parameter, SignalDomain, Siprefix};
use serde_json::{json, Value};

use super::parser::{
    bucket_of, empty_buckets, entry_type, find_nets, is_exposed, item_entry, item_key, parameter,
    terminal_ref, terminal_refs, value_prop_of, INFO, STASH,
};
use super::sim::{identifier_re, source_props, GROUND};
use super::{object, python_json, python_repr, python_str, JsonObject};
use crate::error::Error;
use crate::expression::{format_expression, Dialect};
use crate::helpers::{connections_to_instance_nets, format_si_value};
use crate::stimulus::StimulusIndex;

/// Mosaic device symbol types (frontend/mosaic common.cljc `device-types`). Any other module
/// is written as a `ckt` symbol whose `model` names it.
const DEVICE_TYPES: &str = "pmos nmos npn pnp resistor capacitor inductor vsource isource diode \
    led photodiode modulator ckt amp die straight bend sbend taper transition terminator \
    crossing ring-single ring-double spiral splitter-1x2 coupler coupler-ring mmi-1x2 mmi-2x2 \
    mzi-1x2 mzi-2x2 grating-coupler";

fn is_device_type(name: &str) -> bool {
    DEVICE_TYPES
        .split_whitespace()
        .any(|device_type| device_type == name)
}

/// Pins of Mosaic's built-in electrical symbols; Mosaic only connects these spellings.
fn mosaic_pins(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "resistor" | "capacitor" | "inductor" | "diode" | "vsource" | "isource" => &["P", "N"],
        "nmos" | "pmos" => &["D", "G", "S", "B"],
        "npn" | "pnp" => &["C", "B", "E"],
        _ => &[],
    }
}

/// Mosaic grid units between synthesized symbols.
const PITCH: usize = 4;

/// The `props` value of `param`: the original JSON value while unchanged.
fn prop_value(param: &Parameter, stashed: &JsonObject) -> Value {
    if let Some(raw) = stashed.get(&param.name) {
        if same(param, raw) {
            return raw.clone();
        }
    }
    let Some(value) = &param.default_value else {
        return Value::String(String::new());
    };
    if let Some(prefixed) = &value.prefixed_value {
        if prefixed.prefix != Siprefix::Unspecified {
            return Value::String(format_si_value(prefixed));
        }
        let number = prefixed.double_value;
        let fits_integer = number.fract() == 0.0 && number.abs() < 9.0e15;
        if fits_integer {
            return json!(number as i64);
        }
        return json!(number);
    }
    if let Some(expression) = &value.expression {
        return Value::String(format_expression(expression, Dialect::Gsch, None, false));
    }
    Value::String(String::new())
}

/// `param` still holds the JSON value `raw` (its role aside).
fn same(param: &Parameter, raw: &Value) -> bool {
    parameter(&param.name, raw).default_value == param.default_value
}

/// `Error::Validation` unless `param`, which is not a prop, still has its parsed value.
fn check_derived(
    reference: &ModuleReference,
    param: &Parameter,
    stash: &JsonObject,
) -> Result<(), Error> {
    let expression = object(stash.get("params"))
        .and_then(|params| params.get(&param.name))
        .map(python_str);
    let computed = object(stash.get("computed"));
    let parsed = match computed.and_then(|values| values.get(&param.name)) {
        Some(value) => value.clone(),
        None => expression.clone().map(Value::String).unwrap_or_default(),
    };
    if same(param, &parsed) {
        return Ok(());
    }
    let mut entry_props = Vec::new();
    if let Some(props) = stash_entry(stash).and_then(|entry| object(entry.get("props"))) {
        entry_props.extend(props.keys().cloned());
    }
    let props = match &expression {
        Some(text) => {
            let mut sources = HashSet::new();
            for source in &reference.parameters {
                if source.properties.get("role").map(String::as_str) == Some("source") {
                    sources.insert(source.name.clone());
                }
            }
            let mut read = Vec::new();
            for word in identifier_re().find_iter(text) {
                let word = word.as_str().to_string();
                let is_prop = sources.contains(&word) || entry_props.contains(&word);
                if is_prop && !read.contains(&word) {
                    read.push(word);
                }
            }
            read
        }
        None => entry_props, // computed by the simulation view from the entry's props
    };
    let origin = match &expression {
        Some(text) => format!("model expression {}", python_repr(text)),
        None => "the device's props".to_string(),
    };
    let quoted: Vec<String> = props.iter().map(|prop| python_repr(prop)).collect();
    let edit = if quoted.is_empty() {
        "its model".to_string()
    } else {
        quoted.join(", ")
    };
    Err(Error::Validation(format!(
        "{}: {} is derived from {origin}; edit {edit} instead",
        reference.name,
        python_repr(&param.name)
    )))
}

/// `terminal` as `{component_id, port_name}`.
fn component_ref(terminal: &Value) -> Value {
    if let Value::Object(fields) = terminal {
        if fields.contains_key("component_id") && fields.contains_key("port_name") {
            return terminal.clone();
        }
    }
    let (key, pin) = terminal_ref(Some(terminal)).unwrap_or_default(); // the caller checked it
    json!({"component_id": key, "port_name": pin})
}

fn attached_port(keys: &IndexMap<String, String>, instance: &str, pin: &str) -> Value {
    json!({"component_id": keys[instance], "port_name": pin})
}

/// The stashed `entry` of an instance stash.
fn stash_entry(stash: &JsonObject) -> Option<&JsonObject> {
    object(stash.get("entry"))
}

/// The instance stash of `reference`, None when it did not come from a `.gsch`.
fn stash_of(reference: &ModuleReference) -> Result<Option<JsonObject>, Error> {
    let Some(text) = reference
        .properties
        .get(INFO)
        .filter(|text| !text.is_empty())
    else {
        return Ok(None);
    };
    let info: Value = serde_json::from_str(text).map_err(|error| {
        Error::Validation(format!("{}: {INFO} is not JSON: {error}", reference.name))
    })?;
    Ok(object(info.get(STASH)).cloned())
}

/// The module name the parser gave the instance of `stash`.
fn parsed_master(stash: &JsonObject) -> Option<String> {
    if let Some(module_name) = stash.get("circuit") {
        return Some(python_str(module_name));
    }
    let entry = stash_entry(stash)?;
    let master = super::first_truthy(&[entry.get("model"), entry.get("type")]);
    master.map(python_str)
}

/// The Mosaic symbol `reference` is written as (before any master change).
fn symbol_of(reference: &ModuleReference, stash: Option<&JsonObject>) -> String {
    match stash {
        Some(stash) => stash_entry(stash)
            .and_then(|entry| entry.get("type"))
            .map(python_str)
            .unwrap_or_default(),
        None => reference.module_name.clone(),
    }
}

/// Keys and grid places of synthesized entries.
struct Layout {
    taken: HashSet<String>,
    columns: usize,
    placed: usize,
}

impl Layout {
    fn fresh(&mut self, base: &str) -> String {
        let mut key = base.to_string();
        while self.taken.contains(&key) {
            key.push('_');
        }
        self.taken.insert(key.clone());
        key
    }

    /// `x`, `y` on the grid and an identity `transform`.
    fn place(&mut self, entry: &mut JsonObject) {
        let x = (self.placed % self.columns) * PITCH;
        let y = (self.placed / self.columns) * PITCH;
        self.placed += 1;
        entry.insert("x".to_string(), json!(x));
        entry.insert("y".to_string(), json!(y));
        entry.insert("transform".to_string(), json!([1, 0, 0, 1, 0, 0]));
    }

    /// Key for a new entry: its name, else a free variant plus an `id`.
    fn synthesized(&mut self, entry: &mut JsonObject, name: &str) -> String {
        let key = self.fresh(name);
        if key != name {
            entry.insert("id".to_string(), Value::String(key.clone())); // gfp matches `id` first
        }
        key
    }
}

/// Instance name -> pin -> net, as written.
type InstancePins = IndexMap<String, IndexMap<String, String>>;

/// What the instance entries say about connectivity, for the markers, net entries and
/// polylines written after them, and where new entries go.
struct Written {
    layout: Layout,
    /// Instances written without `nets` (Livewire), with the nets the IR gives them.
    pending: InstancePins,
    /// Instance name -> entry key.
    keys: IndexMap<String, String>,
    /// Net -> its `(instance name, pin)`s.
    members: IndexMap<String, Vec<(String, String)>>,
    /// `(instance name, pin.lower())` -> net.
    net_of: HashMap<(String, String), String>,
}

/// `entry` limited to the pins still on its net, None if none are.
///
/// Its net is the one it names (`id`, `_id` or key) while that net exists, else the net its
/// instance pins share. Pins of vias and tapers cannot be checked and stay.
fn live_net_entry(
    entry: &JsonObject,
    key: &str,
    by_key: &HashMap<String, String>,
    junctions: &HashSet<String>,
    written: &Written,
) -> Option<Value> {
    let mut ports = Vec::new();
    match entry.get("ports") {
        Some(Value::Array(items)) => ports.extend(items.iter().cloned()),
        Some(Value::Object(fields)) => ports.extend(fields.keys().cloned().map(Value::String)),
        _ => {}
    }
    // (port, is a junction pin, net of an instance pin)
    let mut pins: Vec<(Value, bool, Option<String>)> = Vec::new();
    for (port, reference) in ports.into_iter().zip(terminal_refs(entry)) {
        let Some((component, pin)) = reference else {
            continue;
        };
        if let Some(instance) = by_key.get(&component) {
            let net = written
                .net_of
                .get(&(instance.clone(), pin.to_lowercase()))
                .cloned();
            pins.push((port, false, net));
        } else if junctions.contains(&component) {
            pins.push((port, true, None));
        }
    }
    let key_value = Value::String(key.to_string());
    let name = super::first_truthy(&[entry.get("id"), entry.get("_id"), Some(&key_value)])
        .map(python_str)
        .unwrap_or_default();
    let mut shared = HashSet::new();
    for (_, junction, net) in &pins {
        if !junction {
            shared.insert(net.clone());
        }
    }
    let net = if written.members.contains_key(&name) {
        Some(name)
    } else if shared.len() == 1 {
        shared.into_iter().next().flatten()
    } else {
        None
    };
    let mut kept = Vec::new();
    for (port, junction, pin_net) in pins {
        if junction || (net.is_some() && pin_net == net) {
            kept.push(port);
        }
    }
    if kept.is_empty() {
        return None;
    }
    let mut live = entry.clone();
    live.insert("ports".to_string(), Value::Array(kept));
    Some(Value::Object(live))
}

/// The pins of `pending` (entries that had no Mosaic `nets`) whose net the written
/// polylines, net entries and markers no longer join: an edit in the IR.
///
/// Adding `nets` to every Livewire entry would make nyancad (and the simulation view)
/// netlist devices it skipped. A pin joins its net by name: when part of the net already
/// carries that name (Mosaic `nets`, a marker, a net entry), only the pins outside it get
/// `nets`; otherwise all its pending pins do.
fn edited_pins(
    out: &JsonObject,
    keys: &IndexMap<String, String>,
    pins: &InstancePins,
    pending: &InstancePins,
) -> InstancePins {
    if pending.is_empty() {
        return InstancePins::new();
    }
    let mut by_key = IndexMap::new();
    for (instance, key) in keys {
        by_key.insert(key.clone(), instance.clone());
    }
    let mut comps: IndexMap<String, &JsonObject> = IndexMap::new();
    let mut buckets = empty_buckets();
    for (key, entry) in out {
        match (by_key.contains_key(key), entry) {
            (true, Value::Object(fields)) => {
                comps.insert(key.clone(), fields);
            }
            _ => {
                let bucket = entry_type(entry)
                    .and_then(bucket_of)
                    .unwrap_or("gsch_other");
                buckets[bucket].push(super::parser::stash_item(key, entry));
            }
        }
    }
    let (found, _) = find_nets(&comps, &buckets, &by_key);
    let mut group: HashMap<(String, String), String> = HashMap::new();
    let mut on_net: HashMap<String, HashSet<(String, String)>> = HashMap::new();
    for (name, nodes) in &found {
        for (key, pin) in nodes {
            let key = key.as_deref().unwrap_or("");
            let instance = by_key.get(key).cloned().unwrap_or_default();
            let member = (instance, pin.to_lowercase());
            group.insert(member, name.clone());
        }
    }
    for (member, name) in &group {
        on_net
            .entry(name.clone())
            .or_default()
            .insert(member.clone());
    }
    let mut named = HashSet::new(); // names that bind on their own (weak hints do not)
    for (key, entry) in out {
        let kind = entry_type(entry);
        let Some(fields) = entry.as_object() else {
            continue;
        };
        let unattached_port = kind == Some("port") && !super::truthy(fields.get("attached_port"));
        if by_key.contains_key(key) || unattached_port {
            if let Some(nets) = object(fields.get("nets")) {
                for net in nets.values() {
                    named.insert(python_str(net));
                }
            }
        } else if kind == Some("net") {
            let key_value = Value::String(key.clone());
            let name =
                super::first_truthy(&[fields.get("id"), fields.get("_id"), Some(&key_value)]);
            named.insert(name.map(python_str).unwrap_or_default());
        }
    }
    let mut members: HashMap<String, HashSet<(String, String)>> = HashMap::new();
    for (instance, instance_pins) in pins {
        for (pin, net) in instance_pins {
            members
                .entry(net.clone())
                .or_default()
                .insert((instance.clone(), pin.to_lowercase()));
        }
    }
    let empty = HashSet::new();
    let mut add = InstancePins::new();
    for (instance, nets) in pending {
        for (pin, net) in nets {
            if on_net.get(net).unwrap_or(&empty) == members.get(net).unwrap_or(&empty) {
                continue; // the file still says exactly this, name included
            }
            let pin_group = group.get(&(instance.clone(), pin.to_lowercase()));
            if named.contains(net) && pin_group == Some(net) {
                continue; // already on the part that carries the name
            }
            add.entry(instance.clone())
                .or_default()
                .insert(pin.clone(), net.clone());
        }
    }
    // ponytail: an exposed marker without `nets` on an edited, otherwise unnamed net is not
    // named; name the marker too if that edit shows up
    add
}

/// A module-level stash list (`gsch_ports`, …).
fn module_stash(info: &JsonObject, field: &str) -> Vec<Value> {
    match info.get(field) {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

fn nets_object(nets: &IndexMap<String, String>) -> Value {
    let mut fields = JsonObject::new();
    for (pin, net) in nets {
        fields.insert(pin.clone(), Value::String(net.clone()));
    }
    Value::Object(fields)
}

/// Write the top module of `circuit` as `.gsch` JSON text.
///
/// Instances and port markers parsed from a `.gsch` keep their keys and layout fields;
/// others get `type`, `name`, `x`, `y` on a grid and the key `name`. Wires and
/// `net`/`via`/`taper` entries are restored, polylines whose endpoints survive are restored
/// in `{component_id, port_name}` form. `Error::Validation` when the circuit has no module,
/// or a param derived from a model expression (not a prop) was edited.
pub fn write_gsch(circuit: &Circuit) -> Result<String, Error> {
    let Some(last) = circuit.modules.last() else {
        return Err(Error::Validation(
            "netlist has no circuit to write".to_string(),
        ));
    };
    let mut module = last;
    for candidate in &circuit.modules {
        if candidate.name == circuit.top_module {
            module = candidate;
            break;
        }
    }
    let info: JsonObject = match module.properties.get(INFO).filter(|text| !text.is_empty()) {
        Some(text) => match serde_json::from_str(text) {
            Ok(Value::Object(fields)) => fields,
            Ok(_) => JsonObject::new(),
            Err(error) => {
                return Err(Error::Validation(format!(
                    "{}: {INFO} is not JSON: {error}",
                    module.name
                )))
            }
        },
        None => JsonObject::new(),
    };
    // ponytail: files written from non-gsch sources have no Mosaic wires, so Mosaic
    // recomputes `nets` on its first edit; Livewire keeps them.
    let mut stashes: HashMap<String, Option<JsonObject>> = HashMap::new();
    for reference in &module.module_references {
        stashes.insert(reference.name.clone(), stash_of(reference)?);
    }
    let mut symbols: HashMap<String, &'static [&'static str]> = HashMap::new(); // built-ins
    let mut parsed: HashMap<String, JsonObject> = HashMap::new(); // still on its parsed master
    for reference in &module.module_references {
        let stash = stashes[&reference.name].as_ref();
        symbols.insert(
            reference.name.clone(),
            mosaic_pins(&symbol_of(reference, stash)),
        );
        if let Some(stash) = stash {
            if parsed_master(stash).as_deref() == Some(reference.module_name.as_str()) {
                parsed.insert(reference.name.clone(), stash.clone());
            }
        }
    }
    let mut pins = InstancePins::new();
    for ((instance, pin), net) in connections_to_instance_nets(&module.connections, "") {
        let device = parsed.get(&instance);
        let tied = device
            .and_then(|stash| stash.get("tied"))
            .and_then(Value::as_array)
            .is_some_and(|tied| tied.contains(&Value::String(pin.clone())));
        if net == GROUND && tied {
            continue; // the parser tied this open pin to ground
        }
        let mut pin = device
            .and_then(|stash| object(stash.get("pins")))
            .and_then(|renamed| renamed.get(&pin))
            .map(python_str)
            .unwrap_or(pin); // SPICE `p` -> the entry's `A`
        let upper = pin.to_uppercase();
        if symbols
            .get(&instance)
            .is_some_and(|names| names.contains(&upper.as_str()))
        {
            pin = upper; // `p` -> `P`
        }
        pins.entry(instance).or_default().insert(pin, net);
    }

    let mut taken = HashSet::new();
    for stash in stashes.values().flatten() {
        taken.insert(python_str(stash.get("key").unwrap_or(&Value::Null)));
    }
    for (field, items) in &info {
        if let (true, Value::Array(items)) = (field.starts_with("gsch_"), items) {
            for item in items {
                taken.insert(item_key(item));
            }
        }
    }
    let count = module.module_references.len() + module.ports.len();
    let columns = ((count as f64).sqrt().ceil() as usize).max(1);
    let mut out = JsonObject::new();
    let mut written = Written {
        layout: Layout {
            taken,
            columns,
            placed: 0,
        },
        pending: InstancePins::new(),
        keys: IndexMap::new(),
        members: IndexMap::new(),
        net_of: HashMap::new(),
    };
    let stimuli = StimulusIndex::new(circuit);
    for reference in &module.module_references {
        let stash = stashes[&reference.name].as_ref();
        let (key, entry) =
            instance_entry(&stimuli, reference, stash, &parsed, &pins, &mut written)?;
        out.insert(key, Value::Object(entry));
    }

    write_markers(&info, module, &pins, &mut written, &mut out);

    let other = module_stash(&info, "gsch_other");
    let mut junctions = HashSet::new();
    for item in &other {
        junctions.insert(item_key(item));
    }
    let mut alive: HashSet<String> = written.keys.values().cloned().collect();
    alive.extend(junctions.iter().cloned());
    let mut by_key = HashMap::new();
    for (instance, key) in &written.keys {
        by_key.insert(key.clone(), instance.clone());
    }
    for item in module_stash(&info, "gsch_wires") {
        out.insert(item_key(&item), item_entry(&item).clone());
    }
    for item in &other {
        let entry = item_entry(item);
        let key = item_key(item);
        if let (Some("net"), Some(fields)) = (entry_type(entry), entry.as_object()) {
            if let Some(live) = live_net_entry(fields, &key, &by_key, &junctions, &written) {
                out.insert(key, live);
            }
            continue;
        }
        out.insert(key, entry.clone());
    }
    for item in module_stash(&info, "gsch_routes") {
        if let Some(route) = live_route(item_entry(&item), &by_key, &alive, &written) {
            out.insert(item_key(&item), route);
        }
    }
    let edits = edited_pins(&out, &written.keys, &pins, &written.pending);
    for (instance, nets) in edits {
        if let Some(Value::Object(entry)) = out.get_mut(&written.keys[&instance]) {
            entry.insert("nets".to_string(), nets_object(&nets));
        }
    }
    if let Some(models) = info.get("gsch_models") {
        out.insert("models".to_string(), models.clone());
    }
    Ok(python_json(&Value::Object(out), Some(2)))
}

/// The entry (and its key) of one instance; `parsed` holds the stashes of the instances
/// still on their parsed master.
fn instance_entry(
    stimuli: &StimulusIndex,
    reference: &ModuleReference,
    stash: Option<&JsonObject>,
    parsed: &HashMap<String, JsonObject>,
    pins: &InstancePins,
    written: &mut Written,
) -> Result<(String, JsonObject), Error> {
    let stashed_entry = stash.and_then(stash_entry);
    let mut entry = match stashed_entry {
        Some(fields) => fields.clone(),
        None => {
            let mut fresh = JsonObject::new();
            fresh.insert("type".to_string(), Value::String(String::new()));
            written.layout.place(&mut fresh);
            fresh
        }
    };
    let key = match stash {
        Some(stash) => python_str(stash.get("key").unwrap_or(&Value::Null)),
        None => written.layout.synthesized(&mut entry, &reference.name),
    };
    written.keys.insert(reference.name.clone(), key.clone());
    let new_master = !parsed.contains_key(&reference.name); // synthesized, or master changed
    if new_master {
        let builtin = is_device_type(&reference.module_name);
        let symbol = if builtin {
            reference.module_name.as_str()
        } else {
            "ckt"
        };
        entry.insert("type".to_string(), Value::String(symbol.to_string()));
        if builtin {
            entry.shift_remove("model");
        } else {
            entry.insert(
                "model".to_string(),
                Value::String(reference.module_name.clone()),
            );
        }
    }
    let had_name = stashed_entry.is_some_and(|fields| fields.contains_key("name"));
    if stash.is_none() || had_name || reference.name != key {
        // a parsed entry without `name` is named by its key
        entry.insert("name".to_string(), Value::String(reference.name.clone()));
    }
    let mut spelling = HashMap::new();
    if let Some(nets) = object(entry.get("nets")) {
        for pin in nets.keys() {
            spelling.insert(pin.to_lowercase(), pin.clone());
        }
    }
    if let Some(Value::Array(ports)) = entry.get("ports") {
        for port in ports {
            if let Some(name) = port.as_object().and_then(|fields| fields.get("name")) {
                let name = python_str(name);
                spelling.insert(name.to_lowercase(), name);
            }
        }
    }
    let mut nets = IndexMap::new();
    if let Some(instance_pins) = pins.get(&reference.name) {
        for (pin, net) in instance_pins {
            let spelled = spelling
                .get(&pin.to_lowercase())
                .cloned()
                .unwrap_or_else(|| pin.clone());
            nets.insert(spelled, net.clone());
        }
    }
    for (pin, net) in &nets {
        written
            .members
            .entry(net.clone())
            .or_default()
            .push((reference.name.clone(), pin.clone()));
        written
            .net_of
            .insert((reference.name.clone(), pin.to_lowercase()), net.clone());
    }
    let livewire = stashed_entry.is_some_and(|fields| !fields.contains_key("nets"));
    if livewire {
        // Livewire: wired by polylines
        written.pending.insert(reference.name.clone(), nets);
    } else {
        entry.insert("nets".to_string(), nets_object(&nets));
    }

    let stashed_props = object(entry.get("props")).cloned().unwrap_or_default();
    let skipped = reference.properties.get("sim_skip").map(String::as_str) == Some("1");
    let simulated = stash.is_some() && !new_master && !skipped;
    let empty = JsonObject::new();
    let sim = if simulated {
        stash.unwrap_or(&empty)
    } else {
        &empty
    };
    let mut derived = HashSet::new();
    for field in ["params", "computed"] {
        if let Some(values) = object(sim.get(field)) {
            derived.extend(values.keys().cloned());
        }
    }
    let defaults = object(sim.get("defaults")).cloned().unwrap_or_default();
    // a simulated instance keeps the props its params do not show (`model`, `tran`
    // strings, …); otherwise the params are the props
    let mut props = if simulated {
        stashed_props.clone()
    } else {
        JsonObject::new()
    };
    let symbol = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    for param in &reference.parameters {
        let is_source = param.properties.get("role").map(String::as_str) == Some("source");
        if derived.contains(&param.name) && !is_source {
            check_derived(reference, param, sim)?; // no prop to go back to
            continue;
        }
        let unset = !stashed_props.contains_key(&param.name);
        if let (true, Some(default)) = (unset, defaults.get(&param.name)) {
            if same(param, default) {
                continue; // still the model's default
            }
        }
        let prop = match (param.name.as_str(), value_prop_of(&symbol)) {
            ("value", Some(value_prop)) => value_prop.to_string(),
            _ => param.name.clone(),
        };
        let renamed = Parameter {
            name: prop.clone(),
            ..param.clone()
        };
        props.insert(prop, prop_value(&renamed, &stashed_props));
    }
    if let Some(stimulus) = stimuli.of(reference) {
        // also for a source from another format
        props = source_props(&reference.name, stimulus, &props)?;
    }
    let had_props = stashed_entry.is_some_and(|fields| fields.contains_key("props"));
    if stash.is_some() && !had_props && props.is_empty() {
        entry.shift_remove("props");
    } else {
        entry.insert("props".to_string(), Value::Object(props));
    }
    // ponytail: a param removed from a simulated instance keeps its prop; drop props without
    // a param once instances can say which props they show.
    Ok((key, entry))
}

/// The port markers: stashed ones (rebound when their pin moved), then new ones.
fn write_markers(
    info: &JsonObject,
    module: &Module,
    pins: &InstancePins,
    written: &mut Written,
    out: &mut JsonObject,
) {
    let mut markers: IndexMap<String, Value> = IndexMap::new();
    for item in module_stash(info, "gsch_ports") {
        let Some(entry) = item_entry(&item).as_object() else {
            continue;
        };
        if !is_exposed(entry) {
            out.insert(item_key(&item), item_entry(&item).clone()); // names a net
            continue;
        }
        let name = match super::first_truthy(&[entry.get("name"), None]) {
            Some(name) => python_str(name),
            None => item_key(&item),
        };
        markers.entry(name).or_insert(item);
    }
    for port in &module.ports {
        let net = pins
            .get("")
            .and_then(|top| top.get(&port.name))
            .cloned()
            .unwrap_or_else(|| port.name.clone());
        let net_members = written.members.get(&net);
        let first_member = net_members.and_then(|found| found.first());
        if let Some(item) = markers.get(&port.name) {
            let mut entry = item_entry(item).as_object().cloned().unwrap_or_default();
            if entry.contains_key("nets") {
                entry.insert("nets".to_string(), json!({"P": net}));
            }
            if let Some((component, pin)) = terminal_ref(entry.get("attached_port")) {
                let mut on_net = HashSet::new();
                for (instance, member_pin) in net_members.into_iter().flatten() {
                    on_net.insert((written.keys[instance].clone(), member_pin.to_lowercase()));
                }
                if !on_net.contains(&(component, pin.to_lowercase())) {
                    entry.shift_remove("attached_port"); // edited away: rebind to the net's pin
                    if let Some((instance, member_pin)) = first_member {
                        let bound = attached_port(&written.keys, instance, member_pin);
                        entry.insert("attached_port".to_string(), bound);
                    }
                }
            }
            out.insert(item_key(item), Value::Object(entry));
            continue;
        }
        let mut entry = JsonObject::new();
        entry.insert("type".to_string(), json!("port"));
        entry.insert("name".to_string(), Value::String(port.name.clone()));
        written.layout.place(&mut entry);
        entry.insert("nets".to_string(), json!({"P": net}));
        let nature = if port.domain == SignalDomain::Waveguide {
            "photonic"
        } else {
            "electric"
        };
        entry.insert("nature".to_string(), json!(nature));
        if let Some((instance, member_pin)) = first_member {
            let bound = attached_port(&written.keys, instance, member_pin);
            entry.insert("attached_port".to_string(), bound);
        }
        let key = written.layout.synthesized(&mut entry, &port.name);
        out.insert(key, Value::Object(entry));
    }
}

/// A stashed polyline while its ends survive on one net, in `{component_id, port_name}`
/// form; None once it would join pins the IR no longer joins.
fn live_route(
    entry: &Value,
    by_key: &HashMap<String, String>,
    alive: &HashSet<String>,
    written: &Written,
) -> Option<Value> {
    let fields = entry.as_object()?;
    let ends = terminal_refs(fields);
    let refs: Vec<&(String, String)> = ends.iter().flatten().collect();
    let mut pin_nets = Vec::new();
    for (component, pin) in &refs {
        if let Some(instance) = by_key.get(component) {
            pin_nets.push(written.net_of.get(&(instance.clone(), pin.to_lowercase())));
        }
    }
    // a polyline joins its pins: drop it once they are on different nets
    let distinct: HashSet<&Option<&String>> = pin_nets.iter().collect();
    let joined = !pin_nets.contains(&None) && distinct.len() <= 1;
    let all_alive = refs.iter().all(|(component, _)| alive.contains(component));
    if ends.is_empty() || refs.len() != ends.len() || !joined || !all_alive {
        return None;
    }
    let Some(Value::Object(terminals)) = fields.get("terminals") else {
        return Some(entry.clone());
    };
    // Livewire drops {device, port} polylines
    let mut rewritten = JsonObject::new();
    for (role, terminal) in terminals {
        rewritten.insert(role.clone(), component_ref(terminal));
    }
    let mut route = fields.clone();
    route.insert("terminals".to_string(), Value::Object(rewritten));
    Some(Value::Object(route))
}
