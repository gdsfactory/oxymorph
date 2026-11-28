//! Parsers, writers, expressions and paths: each converts the object model to or from the IR and
//! calls `oxymorph-netlist` / `oxymorph-schema`. Errors are `ValueError`.

use std::path::PathBuf;

use indexmap::IndexMap;
use oxymorph_netlist::expression::Dialect;
use oxymorph_netlist::hdl::HdlOptions;
use oxymorph_netlist::GschOptions;
use oxymorph_schema::{paths, SignalDomain};
use pyo3::prelude::*;
use pyo3::types::PyString;

use crate::convert::{value_error, Model, Path};
use crate::enums::Enum;
use crate::model::{Circuit, Expression};

fn circuit_from(
    py: Python<'_>,
    parsed: Result<oxymorph_schema::Circuit, impl ToString>,
) -> PyResult<Circuit> {
    Circuit::from_ir(py, &parsed.map_err(value_error)?)
}

fn write(py: Python<'_>, format: &str, circuit: &Circuit) -> PyResult<String> {
    let circuit = circuit.to_ir(py, &Path::ROOT)?;
    oxymorph_netlist::write_dispatch(format, &circuit).map_err(value_error)
}

/// `parse_<format>(text)` and `write_<format>(circuit)` for each format name.
macro_rules! format_functions {
    ($($parse:ident, $write:ident => $format:literal;)*) => {
        $(
            #[pyfunction]
            fn $parse(py: Python<'_>, text: &str) -> PyResult<Circuit> {
                circuit_from(py, oxymorph_netlist::parse_dispatch($format, text))
            }

            #[pyfunction]
            fn $write(py: Python<'_>, circuit: PyRef<'_, Circuit>) -> PyResult<String> {
                write(py, $format, &circuit)
            }
        )*

        fn add_format_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
            $(
                module.add_function(wrap_pyfunction!($parse, module)?)?;
                module.add_function(wrap_pyfunction!($write, module)?)?;
            )*
            Ok(())
        }
    };
}

format_functions! {
    parse_spice, write_spice => "spice";
    parse_hspice, write_hspice => "hspice";
    parse_xyce, write_xyce => "xyce";
    parse_spectre, write_spectre => "spectre";
    parse_spectre_spice, write_spectre_spice => "spectre_spice";
}

#[pyfunction]
fn write_vams(py: Python<'_>, circuit: PyRef<'_, Circuit>) -> PyResult<String> {
    write(py, "vams", &circuit)
}

#[pyfunction]
fn write_systemverilog(py: Python<'_>, circuit: PyRef<'_, Circuit>) -> PyResult<String> {
    write(py, "systemverilog", &circuit)
}

#[pyfunction]
fn write_gsch(py: Python<'_>, circuit: PyRef<'_, Circuit>) -> PyResult<String> {
    write(py, "gsch", &circuit)
}

type DomainMap = IndexMap<String, Enum<SignalDomain>>;

fn domains(map: DomainMap) -> IndexMap<String, SignalDomain> {
    let mut converted = IndexMap::with_capacity(map.len());
    for (name, domain) in map {
        converted.insert(name, domain.0);
    }
    converted
}

fn hdl_options(
    defines: Option<IndexMap<String, String>>,
    disciplines: Option<DomainMap>,
    pin_domains: Option<IndexMap<String, DomainMap>>,
) -> HdlOptions {
    let pin_domains = match pin_domains {
        None => None,
        Some(cells) => {
            let mut converted = IndexMap::with_capacity(cells.len());
            for (cell, pins) in cells {
                converted.insert(cell, domains(pins));
            }
            Some(converted)
        }
    };
    HdlOptions {
        defines: defines.unwrap_or_default(),
        disciplines: domains(disciplines.unwrap_or_default()),
        pin_domains,
    }
}

/// `defines` drive `` `ifdef `` (none: the `` `else `` branches); `disciplines` map a net type to
/// its domain; `pin_domains` (cell → pin → domain) win over them.
#[pyfunction]
#[pyo3(signature = (text, defines = None, disciplines = None, pin_domains = None))]
fn parse_systemverilog(
    py: Python<'_>,
    text: &str,
    defines: Option<IndexMap<String, String>>,
    disciplines: Option<DomainMap>,
    pin_domains: Option<IndexMap<String, DomainMap>>,
) -> PyResult<Circuit> {
    let options = hdl_options(defines, disciplines, pin_domains);
    circuit_from(py, oxymorph_netlist::parse_systemverilog(text, &options))
}

/// As `parse_systemverilog`, for Verilog-AMS.
#[pyfunction]
#[pyo3(signature = (text, defines = None, disciplines = None, pin_domains = None))]
fn parse_vams(
    py: Python<'_>,
    text: &str,
    defines: Option<IndexMap<String, String>>,
    disciplines: Option<DomainMap>,
    pin_domains: Option<IndexMap<String, DomainMap>>,
) -> PyResult<Circuit> {
    let options = hdl_options(defines, disciplines, pin_domains);
    circuit_from(py, oxymorph_netlist::parse_vams_with(text, &options))
}

/// `models` is the nyanlib document (any JSON-like Python value); `corners` a name or names.
fn gsch_options(
    py: Python<'_>,
    models: Option<&Bound<'_, PyAny>>,
    sim: String,
    corners: Option<&Bound<'_, PyAny>>,
    name: Option<String>,
) -> PyResult<GschOptions> {
    let models = match models {
        None => None,
        Some(models) => {
            let text: String = py
                .import("json")?
                .call_method1("dumps", (models,))?
                .extract()?;
            Some(serde_json::from_str(&text).map_err(value_error)?)
        }
    };
    let corners = match corners {
        None => Vec::new(),
        Some(corner) if corner.is_instance_of::<PyString>() => vec![corner.extract()?],
        Some(corners) => corners.extract()?,
    };
    Ok(GschOptions {
        models,
        sim,
        corners,
        name,
    })
}

/// A `.gsch` schematic as one module (named `name`, else `__top__`), devices resolved for
/// `sim` from the nyanlib `models`.
#[pyfunction]
#[pyo3(signature = (text, *, models = None, sim = String::from("ngspice"), corners = None, name = None))]
fn parse_gsch(
    py: Python<'_>,
    text: &str,
    models: Option<&Bound<'_, PyAny>>,
    sim: String,
    corners: Option<&Bound<'_, PyAny>>,
    name: Option<String>,
) -> PyResult<Circuit> {
    let options = gsch_options(py, models, sim, corners, name)?;
    circuit_from(py, oxymorph_netlist::parse_gsch(text, &options))
}

fn path_text(path: &std::path::Path) -> PyResult<&str> {
    path.to_str()
        .ok_or_else(|| value_error(format!("not a UTF-8 path: {}", path.display())))
}

/// A `.gsch` file and the `.gsch` schematics its devices instantiate.
#[pyfunction]
#[pyo3(signature = (path, *, project_root = None, models = None, sim = String::from("ngspice"), corners = None))]
fn read_gsch(
    py: Python<'_>,
    path: PathBuf,
    project_root: Option<PathBuf>,
    models: Option<&Bound<'_, PyAny>>,
    sim: String,
    corners: Option<&Bound<'_, PyAny>>,
) -> PyResult<Circuit> {
    let options = gsch_options(py, models, sim, corners, None)?;
    let project_root = match &project_root {
        None => None,
        Some(root) => Some(path_text(root)?),
    };
    circuit_from(
        py,
        oxymorph_netlist::read_gsch(path_text(&path)?, project_root, &options),
    )
}

/// The expression tree of `text` (one outer `{…}` or `'…'` stripped); `Expression(raw=text)`
/// when it does not parse.
#[pyfunction]
fn parse_expression(py: Python<'_>, text: &str) -> PyResult<Expression> {
    let expression = oxymorph_netlist::expression::parse_expression(text).map_err(value_error)?;
    Expression::from_ir(py, &expression)
}

/// Canonical text of `expression` for `dialect` (ngspice, xyce, hspice, spectre, vacask,
/// systemverilog, vams, gsch); `named` is a `k=v` value, where raw text is wrapped too.
#[pyfunction]
#[pyo3(signature = (expression, dialect, *, named = false))]
fn format_expression(
    py: Python<'_>,
    expression: PyRef<'_, Expression>,
    dialect: &str,
    named: bool,
) -> PyResult<String> {
    let expression = expression.to_ir(py, &Path::ROOT)?;
    expression.validate().map_err(value_error)?;
    let dialect: Dialect = dialect.parse().map_err(value_error)?;
    Ok(oxymorph_netlist::expression::format_expression(
        &expression,
        dialect,
        None,
        named,
    ))
}

/// `path` in the stored form: POSIX separators, `.`/`..` resolved lexically.
#[pyfunction]
fn posix_file(path: PathBuf) -> PyResult<String> {
    Ok(paths::posix_file(path_text(&path)?))
}

/// `/x`, `//server/share`, `C:/x`, `C:x` (either separator) on every host.
#[pyfunction]
fn is_absolute(path: PathBuf) -> PyResult<bool> {
    Ok(paths::is_absolute(path_text(&path)?))
}

/// `path` joined to `base` when relative, in the stored form.
#[pyfunction]
#[pyo3(signature = (path, base = None))]
fn resolve_file(path: PathBuf, base: Option<PathBuf>) -> PyResult<String> {
    let base = match &base {
        None => None,
        Some(base) => Some(path_text(base)?),
    };
    Ok(paths::resolve_file(path_text(&path)?, base))
}

pub fn add_functions(module: &Bound<'_, PyModule>) -> PyResult<()> {
    add_format_functions(module)?;
    module.add_function(wrap_pyfunction!(write_vams, module)?)?;
    module.add_function(wrap_pyfunction!(write_systemverilog, module)?)?;
    module.add_function(wrap_pyfunction!(write_gsch, module)?)?;
    module.add_function(wrap_pyfunction!(parse_systemverilog, module)?)?;
    module.add_function(wrap_pyfunction!(parse_vams, module)?)?;
    module.add_function(wrap_pyfunction!(parse_gsch, module)?)?;
    module.add_function(wrap_pyfunction!(read_gsch, module)?)?;
    module.add_function(wrap_pyfunction!(parse_expression, module)?)?;
    module.add_function(wrap_pyfunction!(format_expression, module)?)?;
    module.add_function(wrap_pyfunction!(posix_file, module)?)?;
    module.add_function(wrap_pyfunction!(is_absolute, module)?)?;
    module.add_function(wrap_pyfunction!(resolve_file, module)?)?;
    Ok(())
}
