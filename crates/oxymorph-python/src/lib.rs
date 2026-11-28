//! The `oxymorph` Python package (`decisions/0012`): a mutable object model of `#[pyclass]`
//! types mirroring the `oxymorph-schema` IR, converted to the IR at each parse, write and
//! serialization. No netlist logic lives here.

mod builders;
mod convert;
mod enums;
mod io;
mod model;

use pyo3::prelude::*;

#[pymodule]
fn oxymorph(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add("MAX_NESTING", convert::MAX_NESTING)?;
    module.add(
        "MAX_EXPRESSION_DEPTH",
        oxymorph_schema::MAX_EXPRESSION_DEPTH,
    )?;
    enums::add_enums(module)?;
    model::add_classes(module)?;
    builders::add_functions(module)?;
    io::add_functions(module)?;
    Ok(())
}
