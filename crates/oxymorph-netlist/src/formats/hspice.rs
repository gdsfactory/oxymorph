//! HSPICE: the SPICE-family base transformer/writer with HSPICE handling.
//! Parsing entry point is `crate::formats::spice::parse_hspice`.

use oxymorph_schema::Circuit;

use crate::expression::Dialect;
use crate::formats::spice::write_spice_dialect;

/// SPICE with expressions in single quotes.
pub fn write_hspice(circuit: &Circuit) -> String {
    write_spice_dialect(circuit, Dialect::Hspice, "")
}
