//! The oxymorph IR.
//!
//! Hand-written Rust types mirroring the upstream Pydantic models in
//! `morphnet_schema/_circuit.py` and `_simulation.py` (see `decisions/0006`).
//! JSON/YAML field names, enum integer values and defaults match Pydantic's
//! `model_dump_json()` / `model_dump()` exactly, which is the parity oracle.
//! Types and fields added by the kfnetlist port (expressions, macros, stimuli)
//! mirror kfnetlist's models at `e596bb5` (`decisions/0011`) and are left out of
//! JSON/YAML when unset or empty, as kfnetlist's `to_json` does.

pub mod builder;
pub mod expression;
pub mod paths;
pub mod proto;

pub use expression::{
    BinaryOp, Call, Expression, Operator, Ternary, UnaryOp, MAX_EXPRESSION_DEPTH,
};

use indexmap::IndexMap;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

pub type Properties = IndexMap<String, String>;

// ======================================================================
// Enums (serialized as their integer values, like Pydantic IntEnum)
// ======================================================================

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum PortDirection {
    #[default]
    Inout = 0,
    Input = 1,
    Output = 2,
}

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum SignalDomain {
    #[default]
    Unspecified = 0,
    Electrical = 1,
    Waveguide = 2,
}

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum Siprefix {
    #[default]
    Unspecified = 0,
    Quecto = 1,
    Ronto = 2,
    Yocto = 3,
    Zepto = 4,
    Atto = 5,
    Femto = 6,
    Pico = 7,
    Nano = 8,
    Micro = 9,
    Milli = 10,
    Centi = 11,
    Deci = 12,
    Deca = 13,
    Hecto = 14,
    Kilo = 15,
    Mega = 16,
    Giga = 17,
    Tera = 18,
    Peta = 19,
    Exa = 20,
    Zetta = 21,
    Yotta = 22,
    Ronna = 23,
    Quetta = 24,
}

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum ExternalModuleKind {
    #[default]
    Unspecified = 0,
    Device = 1,
    Model = 2,
    Nature = 3,
    Discipline = 4,
}

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum DirectiveKind {
    #[default]
    Unspecified = 0,
    Include = 1,
    Lib = 2,
    Global = 3,
    Option = 4,
    Param = 5,
    Define = 6,
    Timescale = 7,
}

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum AnalysisKind {
    #[default]
    Unspecified = 0,
    Op = 1,
    Dc = 2,
    Ac = 3,
    Tran = 4,
    Noise = 5,
    Tf = 6,
    Sens = 7,
    Pz = 8,
    Disto = 9,
    Four = 10,
    Fft = 11,
}

#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum OutputRequestKind {
    #[default]
    Unspecified = 0,
    Print = 1,
    Plot = 2,
    Probe = 3,
    Save = 4,
}

/// Transient shape of a source: SPICE PULSE/SIN/EXP/SFFM/PWL, Spectre `type=`.
#[derive(Serialize_repr, Deserialize_repr, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum WaveformKind {
    #[default]
    Unspecified = 0,
    Pulse = 1,
    Sin = 2,
    Exp = 3,
    Sffm = 4,
    Pwl = 5,
}

// ======================================================================
// Parameter models
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct PrefixedValue {
    #[serde(default)]
    pub double_value: f64,
    #[serde(default)]
    pub prefix: Siprefix,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct ModelReference {
    #[serde(default)]
    pub model_interface_name: String,
    #[serde(default)]
    pub arguments: IndexMap<String, ParameterValue>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct ParameterValue {
    #[serde(default)]
    pub prefixed_value: Option<PrefixedValue>,
    #[serde(default)]
    pub model_ref: Option<ModelReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<Expression>,
}

impl ParameterValue {
    pub fn prefixed(double_value: f64, prefix: Siprefix) -> Self {
        Self {
            prefixed_value: Some(PrefixedValue {
                double_value,
                prefix,
            }),
            model_ref: None,
            expression: None,
        }
    }

    pub fn model(model_interface_name: impl Into<String>) -> Self {
        Self {
            prefixed_value: None,
            model_ref: Some(ModelReference {
                model_interface_name: model_interface_name.into(),
                arguments: IndexMap::new(),
            }),
            expression: None,
        }
    }

    /// Mirrors `ParameterValue._check_oneof`: at most one variant may be set
    /// (and an expression is itself a valid tree).
    pub fn validate(&self) -> Result<(), String> {
        self.check_oneof()?;
        if let Some(expression) = &self.expression {
            expression.validate()?;
        }
        Ok(())
    }

    /// [`Self::validate`] of this value alone, not of its expression.
    pub fn check_oneof(&self) -> Result<(), String> {
        let set_count = self.prefixed_value.is_some() as u8
            + self.model_ref.is_some() as u8
            + self.expression.is_some() as u8;
        if set_count > 1 {
            return Err(
                "ParameterValue is a oneof: set exactly one of 'prefixed_value', \
                 'model_ref', 'expression'"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Parameter {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub default_value: Option<ParameterValue>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub properties: Properties,
}

// ======================================================================
// Directives
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Directive {
    #[serde(default)]
    pub kind: DirectiveKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// A multi-line block in a simulator `language`, written verbatim.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Macro {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub properties: Properties,
}

// ======================================================================
// Model interfaces
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct ModelInterface {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub function_name: String,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub properties: Properties,
}

// ======================================================================
// Ports and connections
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct PortReference {
    #[serde(default)]
    pub instance_name: String,
    #[serde(default)]
    pub port_name: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Port {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub direction: PortDirection,
    #[serde(default)]
    pub domain: SignalDomain,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub properties: Properties,
    #[serde(default)]
    pub discipline: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Connection {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub source: Option<PortReference>,
    #[serde(default)]
    pub target: Option<PortReference>,
    #[serde(default)]
    pub domain: SignalDomain,
    #[serde(default)]
    pub weight: u32,
    #[serde(default)]
    pub properties: Properties,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Bus {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub domain: SignalDomain,
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default)]
    pub properties: Properties,
}

// ======================================================================
// Module hierarchy
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct ModuleReference {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub module_name: String,
    #[serde(default)]
    pub class_name: String,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub properties: Properties,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Module {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub class_name: String,
    #[serde(default)]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub model_interfaces: Vec<ModelInterface>,
    #[serde(default)]
    pub module_references: Vec<ModuleReference>,
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default)]
    pub buses: Vec<Bus>,
    #[serde(default)]
    pub properties: Properties,
    #[serde(default)]
    pub directives: Vec<Directive>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macros: Vec<Macro>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct ExternalModule {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub class_name: String,
    #[serde(default)]
    pub library_name: String,
    #[serde(default)]
    pub group_name: String,
    #[serde(default)]
    pub ports: Vec<Port>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub properties: Properties,
    #[serde(default)]
    pub kind: ExternalModuleKind,
}

// ======================================================================
// External libraries
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct ExternalLibrary {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub model_type: String,
    #[serde(default)]
    pub external_model_names: Vec<String>,
}

// ======================================================================
// Simulation
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Analysis {
    #[serde(default)]
    pub kind: AnalysisKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub options: Properties,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct OutputRequest {
    #[serde(default)]
    pub kind: OutputRequestKind,
    #[serde(default)]
    pub analysis_type: String,
    #[serde(default)]
    pub variables: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Measurement {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub analysis_type: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct InitialCondition {
    #[serde(default)]
    pub conditions: Properties,
}

/// A transient source shape; `args` use SPICE semantics with Spectre names. A
/// PWL keeps its time/value pairs in `points` (t0 v0 t1 v1 ...) or reads them
/// from `file`, stored POSIX ([`paths::posix_file`]).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Waveform {
    #[serde(default)]
    pub kind: WaveformKind,
    #[serde(default)]
    pub args: IndexMap<String, ParameterValue>,
    #[serde(default)]
    pub points: Vec<ParameterValue>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_file",
        deserialize_with = "deserialize_file"
    )]
    pub file: Option<String>,
}

/// The stored form of a waveform file: POSIX, and `""` is no file (proto's unset string).
fn stored_file(path: &str) -> Option<String> {
    if path.is_empty() {
        None
    } else {
        Some(paths::posix_file(path))
    }
}

fn serialize_file<S: serde::Serializer>(
    file: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    file.as_deref().and_then(stored_file).serialize(serializer)
}

fn deserialize_file<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let file: Option<String> = Option::deserialize(deserializer)?;
    Ok(file.as_deref().and_then(stored_file))
}

impl Waveform {
    /// Mirrors `Waveform._points_or_file`.
    pub fn validate(&self) -> Result<(), String> {
        if !self.points.is_empty() && self.file.is_some() {
            return Err("a PWL waveform has points or a file, not both".to_string());
        }
        if self.file.is_some() && self.kind != WaveformKind::Pwl {
            let kind_name = format!("{:?}", self.kind).to_uppercase();
            return Err(format!("a file belongs to a PWL waveform, not {kind_name}"));
        }
        Ok(())
    }
}

/// What drives one V/I source, named `"<module>/<instance>"`; the source element
/// names it in `properties["stimulus"]`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Stimulus {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dc: Option<ParameterValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_magnitude: Option<ParameterValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_phase: Option<ParameterValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waveform: Option<Waveform>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Simulation {
    #[serde(default)]
    pub analyses: Vec<Analysis>,
    #[serde(default)]
    pub output_requests: Vec<OutputRequest>,
    #[serde(default)]
    pub measurements: Vec<Measurement>,
    #[serde(default)]
    pub initial_conditions: Option<InitialCondition>,
    #[serde(default)]
    pub node_sets: Option<InitialCondition>,
    #[serde(default)]
    pub temperatures: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stimuli: Vec<Stimulus>,
    #[serde(default)]
    pub options: Properties,
}

// ======================================================================
// Circuit (top-level container)
// ======================================================================

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Circuit {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub top_module: String,
    #[serde(default)]
    pub modules: Vec<Module>,
    #[serde(default)]
    pub ext_modules: Vec<ExternalModule>,
    #[serde(default)]
    pub properties: Properties,
    #[serde(default)]
    pub directives: Vec<Directive>,
    #[serde(default)]
    pub simulation: Option<Simulation>,
    #[serde(default)]
    pub libraries: IndexMap<String, ExternalLibrary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macros: Vec<Macro>,
}

impl Circuit {
    /// Mirrors `Circuit.add`: appends modules and sets `top_module` if unset.
    pub fn add(&mut self, modules: impl IntoIterator<Item = Module>) -> &mut Self {
        for m in modules {
            if self.top_module.is_empty() {
                self.top_module = m.name.clone();
            }
            self.modules.push(m);
        }
        self
    }

    pub fn add_ext(&mut self, ext: impl IntoIterator<Item = ExternalModule>) -> &mut Self {
        self.ext_modules.extend(ext);
        self
    }

    pub fn add_library(&mut self, name: impl Into<String>, lib: ExternalLibrary) -> &mut Self {
        self.libraries.insert(name.into(), lib);
        self
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }

    pub fn from_yaml(s: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(s)
    }
}
