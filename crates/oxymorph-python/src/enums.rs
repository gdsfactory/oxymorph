//! The IR's enums as `enum.IntEnum` classes, built once at import from [`ENUM_TABLE`], so members
//! compare equal to the IR integers. [`Enum`] is a model field holding one IR enum value; its
//! getter returns the member.

use oxymorph_schema as ir;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyDict;
use serde::de::DeserializeOwned;

use crate::convert::{Field, Path};

/// `(class name, member names)`; a member's value is its index.
pub const ENUM_TABLE: [(&str, &[&str]); 9] = [
    ("PortDirection", &["INOUT", "INPUT", "OUTPUT"]),
    ("SignalDomain", &["UNSPECIFIED", "ELECTRICAL", "WAVEGUIDE"]),
    (
        "SIPrefix",
        &[
            "UNSPECIFIED",
            "QUECTO",
            "RONTO",
            "YOCTO",
            "ZEPTO",
            "ATTO",
            "FEMTO",
            "PICO",
            "NANO",
            "MICRO",
            "MILLI",
            "CENTI",
            "DECI",
            "DECA",
            "HECTO",
            "KILO",
            "MEGA",
            "GIGA",
            "TERA",
            "PETA",
            "EXA",
            "ZETTA",
            "YOTTA",
            "RONNA",
            "QUETTA",
        ],
    ),
    (
        "ExternalModuleKind",
        &["UNSPECIFIED", "DEVICE", "MODEL", "NATURE", "DISCIPLINE"],
    ),
    (
        "DirectiveKind",
        &[
            "UNSPECIFIED",
            "INCLUDE",
            "LIB",
            "GLOBAL",
            "OPTION",
            "PARAM",
            "DEFINE",
            "TIMESCALE",
        ],
    ),
    (
        "AnalysisKind",
        &[
            "UNSPECIFIED",
            "OP",
            "DC",
            "AC",
            "TRAN",
            "NOISE",
            "TF",
            "SENS",
            "PZ",
            "DISTO",
            "FOUR",
            "FFT",
        ],
    ),
    (
        "OutputRequestKind",
        &["UNSPECIFIED", "PRINT", "PLOT", "PROBE", "SAVE"],
    ),
    (
        "WaveformKind",
        &["UNSPECIFIED", "PULSE", "SIN", "EXP", "SFFM", "PWL"],
    ),
    (
        "Operator",
        &[
            "UNSPECIFIED",
            "ADD",
            "SUB",
            "MUL",
            "DIV",
            "POW",
            "MOD",
            "NEG",
            "NOT",
            "EQ",
            "NE",
            "LT",
            "LE",
            "GT",
            "GE",
            "AND",
            "OR",
        ],
    ),
];

/// Per [`ENUM_TABLE`] row: the class and its members by value.
type EnumClasses = Vec<(Py<PyAny>, Vec<Py<PyAny>>)>;

static ENUM_CLASSES: PyOnceLock<EnumClasses> = PyOnceLock::new();

fn build_enum_classes(py: Python<'_>) -> PyResult<EnumClasses> {
    let int_enum = py.import("enum")?.getattr("IntEnum")?;
    let keywords = PyDict::new(py);
    keywords.set_item("module", "oxymorph")?;
    let mut classes = Vec::new();
    for (class_name, member_names) in ENUM_TABLE {
        let mut pairs = Vec::new();
        for (value, member_name) in member_names.iter().enumerate() {
            pairs.push((*member_name, value));
        }
        let enum_class = int_enum.call((class_name, pairs), Some(&keywords))?;
        let mut members = Vec::new();
        for value in 0..member_names.len() {
            members.push(enum_class.call1((value,))?.unbind());
        }
        classes.push((enum_class.unbind(), members));
    }
    Ok(classes)
}

fn enum_classes(py: Python<'_>) -> PyResult<&EnumClasses> {
    ENUM_CLASSES.get_or_try_init(py, || build_enum_classes(py))
}

pub fn add_enums(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let classes = enum_classes(module.py())?;
    for (row, (class_name, _)) in ENUM_TABLE.iter().enumerate() {
        module.add(*class_name, classes[row].0.bind(module.py()))?;
    }
    Ok(())
}

/// An IR enum and its [`ENUM_TABLE`] row.
pub trait IrEnum: Copy + Default + DeserializeOwned {
    const ROW: usize;

    fn value(self) -> i32;
}

macro_rules! ir_enum {
    ($($enum_type:ty => $row:expr),* $(,)?) => {$(
        impl IrEnum for $enum_type {
            const ROW: usize = $row;

            fn value(self) -> i32 {
                self as i32
            }
        }
    )*};
}

ir_enum!(
    ir::PortDirection => 0,
    ir::SignalDomain => 1,
    ir::Siprefix => 2,
    ir::ExternalModuleKind => 3,
    ir::DirectiveKind => 4,
    ir::AnalysisKind => 5,
    ir::OutputRequestKind => 6,
    ir::WaveformKind => 7,
    ir::Operator => 8,
);

/// An enum field: set from any int that is a member's value, read as the member.
#[derive(Clone, Copy, Default)]
pub struct Enum<T>(pub T);

impl<'a, 'py, T: IrEnum> FromPyObject<'a, 'py> for Enum<T> {
    type Error = PyErr;

    fn extract(object: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
        let value: i64 = object.extract()?;
        match serde_json::from_value::<T>(serde_json::Value::from(value)) {
            Ok(member) => Ok(Enum(member)),
            Err(_) => Err(PyValueError::new_err(format!(
                "{value} is not a valid {}",
                ENUM_TABLE[T::ROW].0
            ))),
        }
    }
}

impl<'py, T: IrEnum> IntoPyObject<'py> for &Enum<T> {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = PyErr;

    fn into_pyobject(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let members = &enum_classes(py)?[T::ROW].1;
        Ok(members[self.0.value() as usize].bind(py).clone())
    }
}

impl<T: IrEnum> Field for Enum<T> {
    type Ir = T;

    fn to_ir(&self, _py: Python<'_>, _path: &Path<'_>) -> PyResult<T> {
        Ok(self.0)
    }

    fn from_ir(_py: Python<'_>, ir: &T) -> PyResult<Self> {
        Ok(Enum(*ir))
    }

    fn empty(_py: Python<'_>) -> Self {
        Enum(T::default())
    }

    fn repr(&self, _py: Python<'_>) -> PyResult<Option<String>> {
        let value = self.0.value();
        if value == 0 {
            return Ok(None);
        }
        let (class_name, member_names) = ENUM_TABLE[T::ROW];
        Ok(Some(format!(
            "{class_name}.{}",
            member_names[value as usize]
        )))
    }
}
