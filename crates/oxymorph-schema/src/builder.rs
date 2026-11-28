//! Factory functions mirroring `morphnet/_api.py`.

use crate::*;

/// A parameter specification: an existing [`Parameter`], a `(name, default)`
/// pair, or a bare name.
// A short-lived argument; `Parameter` is large since `ParameterValue` holds an `Expression`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ParamSpec {
    Parameter(Parameter),
    WithDefault(String, f64),
    Named(String),
}

impl From<Parameter> for ParamSpec {
    fn from(p: Parameter) -> Self {
        ParamSpec::Parameter(p)
    }
}

impl From<(String, f64)> for ParamSpec {
    fn from((n, v): (String, f64)) -> Self {
        ParamSpec::WithDefault(n, v)
    }
}

impl From<&str> for ParamSpec {
    fn from(n: &str) -> Self {
        ParamSpec::Named(n.to_string())
    }
}

impl From<String> for ParamSpec {
    fn from(n: String) -> Self {
        ParamSpec::Named(n)
    }
}

/// Mirrors `_make_params`.
pub fn make_params(specs: &[ParamSpec], kwargs: &[(String, f64)]) -> Vec<Parameter> {
    let mut result = Vec::new();
    for spec in specs {
        match spec {
            ParamSpec::Parameter(p) => result.push(p.clone()),
            ParamSpec::WithDefault(name, val) => result.push(Parameter {
                name: name.clone(),
                default_value: Some(ParameterValue::prefixed(*val, Siprefix::Unspecified)),
                ..Default::default()
            }),
            ParamSpec::Named(name) => result.push(Parameter {
                name: name.clone(),
                ..Default::default()
            }),
        }
    }
    for (name, val) in kwargs {
        result.push(Parameter {
            name: name.clone(),
            default_value: Some(ParameterValue::prefixed(*val, Siprefix::Unspecified)),
            ..Default::default()
        });
    }
    result
}

pub fn circuit(
    name: impl Into<String>,
    top_module: impl Into<String>,
    properties: Properties,
) -> Circuit {
    Circuit {
        name: name.into(),
        top_module: top_module.into(),
        properties,
        ..Default::default()
    }
}

pub fn module(
    name: impl Into<String>,
    class_name: impl Into<String>,
    parameters: Vec<ParamSpec>,
    properties: Properties,
) -> Module {
    Module {
        name: name.into(),
        class_name: class_name.into(),
        parameters: make_params(&parameters, &[]),
        properties,
        ..Default::default()
    }
}

pub fn module_reference(
    name: impl Into<String>,
    module_name: impl Into<String>,
    class_name: impl Into<String>,
    parameters: Vec<ParamSpec>,
    properties: Properties,
    param_kwargs: Vec<(String, f64)>,
) -> ModuleReference {
    ModuleReference {
        name: name.into(),
        module_name: module_name.into(),
        class_name: class_name.into(),
        parameters: make_params(&parameters, &param_kwargs),
        properties,
    }
}

pub fn ext_module(
    name: impl Into<String>,
    class_name: impl Into<String>,
    library_name: impl Into<String>,
    group_name: impl Into<String>,
    ports: Vec<Port>,
    parameters: Vec<ParamSpec>,
    properties: Properties,
) -> ExternalModule {
    ExternalModule {
        name: name.into(),
        class_name: class_name.into(),
        library_name: library_name.into(),
        group_name: group_name.into(),
        ports,
        parameters: make_params(&parameters, &[]),
        properties,
        ..Default::default()
    }
}

pub fn port(
    name: impl Into<String>,
    direction: PortDirection,
    domain: SignalDomain,
    width: u32,
    discipline: impl Into<String>,
    properties: Properties,
) -> Port {
    Port {
        name: name.into(),
        direction,
        domain,
        width,
        discipline: discipline.into(),
        properties,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn connection(
    name: impl Into<String>,
    src: (impl Into<String>, impl Into<String>),
    tgt: (impl Into<String>, impl Into<String>),
    domain: SignalDomain,
    properties: Properties,
) -> Connection {
    Connection {
        name: name.into(),
        source: Some(PortReference {
            instance_name: src.0.into(),
            port_name: src.1.into(),
        }),
        target: Some(PortReference {
            instance_name: tgt.0.into(),
            port_name: tgt.1.into(),
        }),
        domain,
        weight: 0,
        properties,
    }
}

pub fn parameter(
    name: impl Into<String>,
    default_value: Option<ParameterValue>,
    description: impl Into<String>,
    properties: Properties,
) -> Parameter {
    Parameter {
        name: name.into(),
        default_value,
        description: description.into(),
        properties,
    }
}

pub fn external_library(
    path: impl Into<String>,
    model_type: impl Into<String>,
    external_model_names: Vec<String>,
) -> ExternalLibrary {
    ExternalLibrary {
        path: path.into(),
        model_type: model_type.into(),
        external_model_names,
    }
}

pub fn model_interface(
    name: impl Into<String>,
    function_name: impl Into<String>,
    parameters: Vec<ParamSpec>,
    properties: Properties,
) -> ModelInterface {
    ModelInterface {
        name: name.into(),
        function_name: function_name.into(),
        parameters: make_params(&parameters, &[]),
        properties,
    }
}
