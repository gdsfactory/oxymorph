//! Builder functions mirroring `morphnet/_api.py` and `oxymorph_schema::builder`, over the
//! object model. Properties dicts are copied; parameter specs are a `Parameter`, a name or a
//! `(name, float)` pair.

use oxymorph_schema as ir;
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};

use crate::convert::{Field, LiveDict, LiveList};
use crate::enums::Enum;
use crate::model::{
    Circuit, Connection, ExternalLibrary, ExternalModule, ModelInterface, Module, ModuleReference,
    Parameter, ParameterValue, Port, PortReference, PrefixedValue,
};

fn or_empty<T: Field>(py: Python<'_>, value: Option<T>) -> T {
    match value {
        Some(value) => value,
        None => T::empty(py),
    }
}

fn prefixed_parameter(py: Python<'_>, name: String, value: f64) -> PyResult<Parameter> {
    let prefixed_value = PrefixedValue {
        double_value: value,
        prefix: Enum::default(),
    };
    let default_value = ParameterValue {
        prefixed_value: Some(Py::new(py, prefixed_value)?),
        model_ref: None,
        expression: None,
    };
    Ok(Parameter {
        name,
        default_value: Some(Py::new(py, default_value)?),
        description: String::new(),
        properties: LiveDict::empty(py),
    })
}

/// Mirrors `_make_params`: the specs, then `name=default` keywords.
fn make_parameters(
    py: Python<'_>,
    specs: Option<&Bound<'_, PyAny>>,
    values: Option<&Bound<'_, PyDict>>,
) -> PyResult<LiveList<Parameter>> {
    let parameters = PyList::empty(py);
    if let Some(specs) = specs {
        for spec in specs.try_iter()? {
            let spec = spec?;
            if spec.cast::<Parameter>().is_ok() {
                parameters.append(spec)?;
            } else if let Ok(name) = spec.cast::<PyString>() {
                let parameter =
                    parameter(py, name.to_cow()?.into_owned(), None, String::new(), None);
                parameters.append(Py::new(py, parameter)?)?;
            } else if let Ok((name, value)) = spec.extract::<(String, f64)>() {
                parameters.append(Py::new(py, prefixed_parameter(py, name, value)?)?)?;
            } else {
                return Err(PyTypeError::new_err(format!(
                    "parameter must be a Parameter, a name or a (name, float) tuple, got {}",
                    spec.repr()?
                )));
            }
        }
    }
    if let Some(values) = values {
        for (name, value) in values.iter() {
            let parameter = prefixed_parameter(py, name.extract()?, value.extract()?)?;
            parameters.append(Py::new(py, parameter)?)?;
        }
    }
    parameters.extract()
}

#[pyfunction]
#[pyo3(signature = (name, *, top_module = String::new(), properties = None))]
fn circuit(
    py: Python<'_>,
    name: String,
    top_module: String,
    properties: Option<LiveDict<String>>,
) -> Circuit {
    Circuit {
        name,
        top_module,
        modules: LiveList::empty(py),
        ext_modules: LiveList::empty(py),
        properties: or_empty(py, properties),
        directives: LiveList::empty(py),
        simulation: None,
        libraries: LiveDict::empty(py),
        macros: LiveList::empty(py),
    }
}

#[pyfunction]
#[pyo3(signature = (name, *, class_name = String::new(), parameters = None, properties = None))]
fn module(
    py: Python<'_>,
    name: String,
    class_name: String,
    parameters: Option<&Bound<'_, PyAny>>,
    properties: Option<LiveDict<String>>,
) -> PyResult<Module> {
    Ok(Module {
        name,
        class_name,
        ports: LiveList::empty(py),
        parameters: make_parameters(py, parameters, None)?,
        model_interfaces: LiveList::empty(py),
        module_references: LiveList::empty(py),
        connections: LiveList::empty(py),
        buses: LiveList::empty(py),
        properties: or_empty(py, properties),
        directives: LiveList::empty(py),
        macros: LiveList::empty(py),
    })
}

/// An instance of `module_name`; `name=default` keywords (`value=2.0`) become parameters.
#[pyfunction]
#[pyo3(signature = (name, module_name, *, class_name = String::new(), parameters = None, properties = None, **parameter_values))]
fn module_reference(
    py: Python<'_>,
    name: String,
    module_name: String,
    class_name: String,
    parameters: Option<&Bound<'_, PyAny>>,
    properties: Option<LiveDict<String>>,
    parameter_values: Option<&Bound<'_, PyDict>>,
) -> PyResult<ModuleReference> {
    Ok(ModuleReference {
        name,
        module_name,
        class_name,
        parameters: make_parameters(py, parameters, parameter_values)?,
        properties: or_empty(py, properties),
    })
}

#[pyfunction]
#[pyo3(signature = (name, *, class_name = String::new(), library_name = String::new(), group_name = String::new(), ports = None, parameters = None, properties = None))]
#[allow(clippy::too_many_arguments)]
fn ext_module(
    py: Python<'_>,
    name: String,
    class_name: String,
    library_name: String,
    group_name: String,
    ports: Option<LiveList<Port>>,
    parameters: Option<&Bound<'_, PyAny>>,
    properties: Option<LiveDict<String>>,
) -> PyResult<ExternalModule> {
    Ok(ExternalModule {
        name,
        class_name,
        library_name,
        group_name,
        ports: or_empty(py, ports),
        parameters: make_parameters(py, parameters, None)?,
        properties: or_empty(py, properties),
        kind: Enum::default(),
    })
}

#[pyfunction]
#[pyo3(signature = (name, *, direction = None, domain = None, width = 0, discipline = String::new(), properties = None))]
pub fn port(
    py: Python<'_>,
    name: String,
    direction: Option<Enum<ir::PortDirection>>,
    domain: Option<Enum<ir::SignalDomain>>,
    width: u32,
    discipline: String,
    properties: Option<LiveDict<String>>,
) -> Port {
    Port {
        name,
        direction: direction.unwrap_or_default(),
        domain: domain.unwrap_or_default(),
        width,
        properties: or_empty(py, properties),
        discipline,
    }
}

fn port_reference(py: Python<'_>, endpoint: (String, String)) -> PyResult<Py<PortReference>> {
    let (instance_name, port_name) = endpoint;
    Py::new(
        py,
        PortReference {
            instance_name,
            port_name,
        },
    )
}

/// A connection on net `name` from `source` to `target`, each `(instance_name, port_name)`.
#[pyfunction]
#[pyo3(signature = (name, source, target, *, domain = None, properties = None))]
pub fn connection(
    py: Python<'_>,
    name: String,
    source: (String, String),
    target: (String, String),
    domain: Option<Enum<ir::SignalDomain>>,
    properties: Option<LiveDict<String>>,
) -> PyResult<Connection> {
    Ok(Connection {
        name,
        source: Some(port_reference(py, source)?),
        target: Some(port_reference(py, target)?),
        domain: domain.unwrap_or_default(),
        weight: 0,
        properties: or_empty(py, properties),
    })
}

#[pyfunction]
#[pyo3(signature = (name, *, default_value = None, description = String::new(), properties = None))]
fn parameter(
    py: Python<'_>,
    name: String,
    default_value: Option<Py<ParameterValue>>,
    description: String,
    properties: Option<LiveDict<String>>,
) -> Parameter {
    Parameter {
        name,
        default_value,
        description,
        properties: or_empty(py, properties),
    }
}

#[pyfunction]
#[pyo3(signature = (path, *, model_type = String::new(), external_model_names = None))]
fn external_library(
    py: Python<'_>,
    path: String,
    model_type: String,
    external_model_names: Option<LiveList<String>>,
) -> ExternalLibrary {
    ExternalLibrary {
        path,
        model_type,
        external_model_names: or_empty(py, external_model_names),
    }
}

#[pyfunction]
#[pyo3(signature = (name, *, function_name = String::new(), parameters = None, properties = None))]
fn model_interface(
    py: Python<'_>,
    name: String,
    function_name: String,
    parameters: Option<&Bound<'_, PyAny>>,
    properties: Option<LiveDict<String>>,
) -> PyResult<ModelInterface> {
    Ok(ModelInterface {
        name,
        function_name,
        parameters: make_parameters(py, parameters, None)?,
        properties: or_empty(py, properties),
    })
}

pub fn add_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(circuit, module)?)?;
    module.add_function(wrap_pyfunction!(self::module, module)?)?;
    module.add_function(wrap_pyfunction!(module_reference, module)?)?;
    module.add_function(wrap_pyfunction!(ext_module, module)?)?;
    module.add_function(wrap_pyfunction!(port, module)?)?;
    module.add_function(wrap_pyfunction!(connection, module)?)?;
    module.add_function(wrap_pyfunction!(parameter, module)?)?;
    module.add_function(wrap_pyfunction!(external_library, module)?)?;
    module.add_function(wrap_pyfunction!(model_interface, module)?)?;
    Ok(())
}
