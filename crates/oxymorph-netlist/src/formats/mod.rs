#[cfg(feature = "gsch")]
pub mod gsch;
#[cfg(feature = "hspice")]
pub mod hspice;
#[cfg(feature = "spectre")]
pub mod spectre;
#[cfg(feature = "spectre_spice")]
pub mod spectre_spice;
#[cfg(feature = "spice")]
pub mod spice;
#[cfg(feature = "systemverilog")]
pub mod systemverilog;
#[cfg(feature = "vams")]
pub mod vams;
#[cfg(feature = "xyce")]
pub mod xyce;

#[cfg(all(feature = "gsch", not(target_arch = "wasm32")))]
pub use gsch::read_gsch;
#[cfg(feature = "gsch")]
pub use gsch::{parse_gsch, write_gsch, GschOptions};
#[cfg(feature = "hspice")]
pub use hspice::write_hspice;
#[cfg(feature = "spectre")]
pub use spectre::{parse_spectre, write_spectre};
#[cfg(feature = "spectre_spice")]
pub use spectre_spice::{parse_spectre_spice, write_spectre_spice};
#[cfg(feature = "spice")]
pub use spice::{parse_hspice, parse_spice, parse_xyce, write_spice};
#[cfg(feature = "systemverilog")]
pub use systemverilog::{parse_systemverilog, write_systemverilog};
#[cfg(feature = "vams")]
pub use vams::{parse_vams, parse_vams_with, write_vams};
#[cfg(feature = "xyce")]
pub use xyce::write_xyce;

use oxymorph_schema::Circuit;

use crate::error::Error;

/// Dispatch parse by format name (used by bindings).
pub fn parse_dispatch(format: &str, text: &str) -> Result<Circuit, Error> {
    match format {
        #[cfg(feature = "spice")]
        "spice" => parse_spice(text),
        #[cfg(feature = "hspice")]
        "hspice" => parse_hspice(text),
        #[cfg(feature = "xyce")]
        "xyce" => parse_xyce(text),
        #[cfg(feature = "spectre")]
        "spectre" => parse_spectre(text),
        #[cfg(feature = "spectre_spice")]
        "spectre_spice" => parse_spectre_spice(text),
        #[cfg(feature = "vams")]
        "vams" => parse_vams(text),
        #[cfg(feature = "systemverilog")]
        "systemverilog" => parse_systemverilog(text, &crate::hdl::HdlOptions::default()),
        #[cfg(feature = "gsch")]
        "gsch" => parse_gsch(text, &GschOptions::default()),
        other => Err(Error::Validation(format!(
            "unknown or disabled format: {other}"
        ))),
    }
}

/// Dispatch write by format name (used by bindings). Only `gsch` can fail: an edited
/// param that is derived from a model expression has no prop to go back to.
pub fn write_dispatch(format: &str, circuit: &Circuit) -> Result<String, Error> {
    let text = match format {
        #[cfg(feature = "gsch")]
        "gsch" => return write_gsch(circuit),
        #[cfg(feature = "spectre")]
        "spectre" => write_spectre(circuit),
        #[cfg(feature = "vams")]
        "vams" => write_vams(circuit),
        #[cfg(feature = "systemverilog")]
        "systemverilog" => write_systemverilog(circuit),
        #[cfg(feature = "hspice")]
        "hspice" => write_hspice(circuit),
        #[cfg(feature = "xyce")]
        "xyce" => write_xyce(circuit),
        #[cfg(feature = "spectre_spice")]
        "spectre_spice" => write_spectre_spice(circuit),
        #[cfg(feature = "spice")]
        _ => write_spice(circuit),
        #[cfg(not(feature = "spice"))]
        _ => String::new(),
    };
    Ok(text)
}
