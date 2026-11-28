//! Xyce: the SPICE-family base transformer/writer with Xyce handling.

use oxymorph_schema::Circuit;

use crate::expression::Dialect;
use crate::formats::spice::write_spice_dialect;

/// SPICE with `{…}` expressions and `PARAMS:` before subckt header and X-call params.
pub fn write_xyce(circuit: &Circuit) -> String {
    write_spice_dialect(circuit, Dialect::Xyce, "PARAMS:")
}
