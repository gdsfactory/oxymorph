//! The pyclass graph ↔ `oxymorph_schema` IR conversion, in one place.
//!
//! A [`Model`] is a `#[pyclass]` with one [`Field`] per IR field. The graph is converted to the
//! IR at each parse, write, serialization and comparison, so every in-place edit of a live list
//! or dict is seen. The walk is bounded by [`MAX_NESTING`] and names the [`Path`] of a wrong type.

use std::cell::RefCell;
use std::convert::Infallible;
use std::fmt::Write;
use std::marker::PhantomData;

use indexmap::IndexMap;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};
use pyo3::{PyClass, PyTraverseError, PyTypeInfo, PyVisit};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// The deepest object graph converted, in attribute and item steps, so a cycle stops here
/// instead of overflowing the stack. It is at least what the IR legitimately holds: an
/// expression `MAX_EXPRESSION_DEPTH` deep where the IR nests one deepest (asserted below).
/// serde_json, YAML and protobuf read that much back (`oxymorph_schema::MAX_EXPRESSION_DEPTH`).
pub const MAX_NESTING: usize = 128;

/// Steps from a `Circuit` to its deepest expression:
/// `modules[i].module_references[j].parameters[k].default_value.model_ref.arguments['w'].expression`.
const STEPS_TO_DEEPEST_EXPRESSION: usize = 11;
/// Steps of a call node (`.call.args[i]`), the costliest node.
const STEPS_PER_CALL: usize = 3;
/// Steps of a literal leaf (`.literal.double_value`), the costliest leaf.
const STEPS_PER_LITERAL: usize = 2;

const _: () = assert!(
    MAX_NESTING
        >= STEPS_TO_DEEPEST_EXPRESSION
            + STEPS_PER_CALL * (oxymorph_schema::MAX_EXPRESSION_DEPTH - 1)
            + STEPS_PER_LITERAL,
    "MAX_NESTING must convert every expression the parsers build"
);

pub fn value_error(error: impl ToString) -> PyErr {
    PyValueError::new_err(error.to_string())
}

#[derive(Clone, Copy)]
enum Step<'a> {
    Root,
    Field(&'static str),
    Index(usize),
    Key(&'a str),
}

/// Where the conversion is, rendered only on error (`modules[0].properties['k']`).
#[derive(Clone, Copy)]
pub struct Path<'a> {
    parent: Option<&'a Path<'a>>,
    step: Step<'a>,
    depth: usize,
}

impl<'a> Path<'a> {
    pub const ROOT: Path<'static> = Path {
        parent: None,
        step: Step::Root,
        depth: 0,
    };

    fn child(&'a self, step: Step<'a>) -> PyResult<Path<'a>> {
        let child = Path {
            parent: Some(self),
            step,
            depth: self.depth + 1,
        };
        if child.depth > MAX_NESTING {
            return Err(PyValueError::new_err(format!(
                "{}: nested deeper than {MAX_NESTING} levels (a cycle?)",
                child.render()
            )));
        }
        Ok(child)
    }

    pub fn field(&'a self, name: &'static str) -> PyResult<Path<'a>> {
        self.child(Step::Field(name))
    }

    pub fn index(&'a self, index: usize) -> PyResult<Path<'a>> {
        self.child(Step::Index(index))
    }

    pub fn key(&'a self, key: &'a str) -> PyResult<Path<'a>> {
        self.child(Step::Key(key))
    }

    pub fn render(&self) -> String {
        let mut steps = Vec::new();
        let mut current = Some(self);
        while let Some(path) = current {
            steps.push(path.step);
            current = path.parent;
        }
        let mut rendered = String::new();
        for step in steps.iter().rev() {
            match step {
                Step::Root => {}
                Step::Field(name) => {
                    if !rendered.is_empty() {
                        rendered.push('.');
                    }
                    rendered.push_str(name);
                }
                Step::Index(index) => {
                    let _ = write!(rendered, "[{index}]");
                }
                Step::Key(key) => {
                    let _ = write!(rendered, "['{key}']");
                }
            }
        }
        rendered
    }

    /// `ValueError` naming this path: `modules[0].parameters[1].default_value: <message>`.
    pub fn value_error(&self, message: &str) -> PyErr {
        let rendered = self.render();
        if rendered.is_empty() {
            PyValueError::new_err(message.to_string())
        } else {
            PyValueError::new_err(format!("{rendered}: {message}"))
        }
    }

    pub fn type_error(&self, expected: &str, value: &Bound<'_, PyAny>) -> PyErr {
        let found = match value.get_type().name() {
            Ok(name) => name.to_string(),
            Err(_) => "?".to_string(),
        };
        let rendered = self.render();
        if rendered.is_empty() {
            PyTypeError::new_err(format!("expected {expected}, got {found}"))
        } else {
            PyTypeError::new_err(format!("{rendered}: expected {expected}, got {found}"))
        }
    }
}

thread_local! {
    /// The models whose `repr` is being built on this thread, outermost first.
    static REPR_STACK: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// One model on this thread's `repr` stack, popped on drop. `enter` gives `None` when the
/// object is already on the stack (a cycle) or the stack is [`MAX_NESTING`] deep, so `repr`
/// prints `Name(...)` instead of recursing until the C stack overflows.
pub struct ReprGuard;

impl ReprGuard {
    pub fn enter(object: &Bound<'_, PyAny>) -> Option<ReprGuard> {
        let address = object.as_ptr() as usize;
        REPR_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.len() >= MAX_NESTING || stack.contains(&address) {
                return None;
            }
            stack.push(address);
            Some(ReprGuard)
        })
    }
}

impl Drop for ReprGuard {
    fn drop(&mut self) {
        REPR_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// A `#[pyclass]` mirroring one IR type (generated by [`model!`](crate::model)).
pub trait Model: PyClass + Into<PyClassInitializer<Self>> {
    type Ir: Serialize + DeserializeOwned + PartialEq;

    fn to_ir(&self, py: Python<'_>, path: &Path<'_>) -> PyResult<Self::Ir>;
    fn from_ir(py: Python<'_>, ir: &Self::Ir) -> PyResult<Self>;
}

/// The storage of one model field: what it is in the IR, its default, its `repr`, its GC edges.
pub trait Field: Sized {
    type Ir;

    fn to_ir(&self, py: Python<'_>, path: &Path<'_>) -> PyResult<Self::Ir>;
    fn from_ir(py: Python<'_>, ir: &Self::Ir) -> PyResult<Self>;
    fn empty(py: Python<'_>) -> Self;
    /// `repr` of the value, `None` when it is the default (left out of the model's `repr`).
    fn repr(&self, py: Python<'_>) -> PyResult<Option<String>>;

    fn traverse(&self, _visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        Ok(())
    }

    fn clear(&mut self) {}
}

macro_rules! plain_field {
    ($($plain:ty),*) => {$(
        impl Field for $plain {
            type Ir = $plain;

            fn to_ir(&self, _py: Python<'_>, _path: &Path<'_>) -> PyResult<$plain> {
                Ok(self.clone())
            }

            fn from_ir(_py: Python<'_>, ir: &$plain) -> PyResult<Self> {
                Ok(ir.clone())
            }

            fn empty(_py: Python<'_>) -> Self {
                Self::default()
            }

            fn repr(&self, py: Python<'_>) -> PyResult<Option<String>> {
                if *self == Self::default() {
                    return Ok(None);
                }
                Ok(Some(self.into_pyobject(py)?.repr()?.to_string()))
            }
        }
    )*};
}

plain_field!(String, f64, u32, Option<String>);

/// A nested object: the getter returns the stored object, so editing it persists.
impl<M: Model> Field for Option<Py<M>> {
    type Ir = Option<M::Ir>;

    fn to_ir(&self, py: Python<'_>, path: &Path<'_>) -> PyResult<Option<M::Ir>> {
        match self {
            None => Ok(None),
            Some(object) => Ok(Some(object.bind(py).try_borrow()?.to_ir(py, path)?)),
        }
    }

    fn from_ir(py: Python<'_>, ir: &Option<M::Ir>) -> PyResult<Self> {
        object_from_ir(py, ir.as_ref())
    }

    fn empty(_py: Python<'_>) -> Self {
        None
    }

    fn repr(&self, py: Python<'_>) -> PyResult<Option<String>> {
        match self {
            None => Ok(None),
            Some(object) => Ok(Some(object.bind(py).as_any().repr()?.to_string())),
        }
    }

    fn traverse(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(self)
    }

    fn clear(&mut self) {
        *self = None;
    }
}

/// A new graph object for an IR value (`Option<Box<_>>` IR fields pass `as_deref()`).
pub fn object_from_ir<M: Model>(py: Python<'_>, ir: Option<&M::Ir>) -> PyResult<Option<Py<M>>> {
    match ir {
        None => Ok(None),
        Some(ir) => Ok(Some(Py::new(py, M::from_ir(py, ir)?)?)),
    }
}

/// What a live list or dict holds: a model, `str` or `float`.
pub trait Element {
    type Ir;

    fn element_to_ir(item: &Bound<'_, PyAny>, path: &Path<'_>) -> PyResult<Self::Ir>;
    fn element_from_ir<'py>(py: Python<'py>, ir: &Self::Ir) -> PyResult<Bound<'py, PyAny>>;
}

impl<M: Model> Element for M {
    type Ir = M::Ir;

    fn element_to_ir(item: &Bound<'_, PyAny>, path: &Path<'_>) -> PyResult<M::Ir> {
        match item.cast::<M>() {
            Ok(object) => object.try_borrow()?.to_ir(item.py(), path),
            Err(_) => Err(path.type_error(M::NAME, item)),
        }
    }

    fn element_from_ir<'py>(py: Python<'py>, ir: &M::Ir) -> PyResult<Bound<'py, PyAny>> {
        Ok(Bound::new(py, M::from_ir(py, ir)?)?.into_any())
    }
}

impl Element for String {
    type Ir = String;

    fn element_to_ir(item: &Bound<'_, PyAny>, path: &Path<'_>) -> PyResult<String> {
        match item.cast::<PyString>() {
            Ok(text) => Ok(text.to_cow()?.into_owned()),
            Err(_) => Err(path.type_error("str", item)),
        }
    }

    fn element_from_ir<'py>(py: Python<'py>, ir: &String) -> PyResult<Bound<'py, PyAny>> {
        Ok(PyString::new(py, ir).into_any())
    }
}

impl Element for f64 {
    type Ir = f64;

    fn element_to_ir(item: &Bound<'_, PyAny>, path: &Path<'_>) -> PyResult<f64> {
        let is_number = item.is_instance_of::<PyFloat>()
            || (item.is_instance_of::<PyInt>() && !item.is_instance_of::<PyBool>());
        if !is_number {
            return Err(path.type_error("float", item));
        }
        item.extract()
    }

    fn element_from_ir<'py>(py: Python<'py>, ir: &f64) -> PyResult<Bound<'py, PyAny>> {
        Ok(PyFloat::new(py, *ir).into_any())
    }
}

/// `repr` of a live container: its items when few, else its length.
fn container_repr(container: &Bound<'_, PyAny>, length: usize) -> PyResult<Option<String>> {
    const MAX_ITEMS_SHOWN: usize = 3;
    if length == 0 {
        Ok(None)
    } else if length <= MAX_ITEMS_SHOWN {
        Ok(Some(container.repr()?.to_string()))
    } else {
        Ok(Some(format!("<{length} items>")))
    }
}

/// A list field. The getter returns this same list, so `append`, `extend` and item assignment
/// persist; the setter takes any iterable and stores a new list. Items are checked at conversion.
pub struct LiveList<E> {
    list: Py<PyList>,
    element: PhantomData<fn() -> E>,
}

impl<E> LiveList<E> {
    pub fn bind<'py>(&self, py: Python<'py>) -> &Bound<'py, PyList> {
        self.list.bind(py)
    }
}

impl<'a, 'py, E> FromPyObject<'a, 'py> for LiveList<E> {
    type Error = PyErr;

    fn extract(object: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
        let list = PyList::type_object(object.py()).call1((object,))?;
        Ok(LiveList {
            list: list.cast_into::<PyList>()?.unbind(),
            element: PhantomData,
        })
    }
}

impl<'py, E> IntoPyObject<'py> for &LiveList<E> {
    type Target = PyList;
    type Output = Bound<'py, PyList>;
    type Error = Infallible;

    fn into_pyobject(self, py: Python<'py>) -> Result<Bound<'py, PyList>, Infallible> {
        Ok(self.list.bind(py).clone())
    }
}

impl<E: Element> Field for LiveList<E> {
    type Ir = Vec<E::Ir>;

    fn to_ir(&self, py: Python<'_>, path: &Path<'_>) -> PyResult<Vec<E::Ir>> {
        let list = self.list.bind(py);
        let mut items = Vec::with_capacity(list.len());
        for (index, item) in list.iter().enumerate() {
            items.push(E::element_to_ir(&item, &path.index(index)?)?);
        }
        Ok(items)
    }

    fn from_ir(py: Python<'_>, ir: &Vec<E::Ir>) -> PyResult<Self> {
        let mut items = Vec::with_capacity(ir.len());
        for item in ir {
            items.push(E::element_from_ir(py, item)?);
        }
        Ok(LiveList {
            list: PyList::new(py, items)?.unbind(),
            element: PhantomData,
        })
    }

    fn empty(py: Python<'_>) -> Self {
        LiveList {
            list: PyList::empty(py).unbind(),
            element: PhantomData,
        }
    }

    fn repr(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let list = self.list.bind(py);
        container_repr(list.as_any(), list.len())
    }

    fn traverse(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.list)
    }
}

/// A `str`-keyed dict field, live like [`LiveList`]; the setter takes any mapping (or pairs).
pub struct LiveDict<E> {
    dict: Py<PyDict>,
    element: PhantomData<fn() -> E>,
}

impl<E> LiveDict<E> {
    pub fn bind<'py>(&self, py: Python<'py>) -> &Bound<'py, PyDict> {
        self.dict.bind(py)
    }
}

impl<'a, 'py, E> FromPyObject<'a, 'py> for LiveDict<E> {
    type Error = PyErr;

    fn extract(object: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
        let dict = PyDict::type_object(object.py()).call1((object,))?;
        Ok(LiveDict {
            dict: dict.cast_into::<PyDict>()?.unbind(),
            element: PhantomData,
        })
    }
}

impl<'py, E> IntoPyObject<'py> for &LiveDict<E> {
    type Target = PyDict;
    type Output = Bound<'py, PyDict>;
    type Error = Infallible;

    fn into_pyobject(self, py: Python<'py>) -> Result<Bound<'py, PyDict>, Infallible> {
        Ok(self.dict.bind(py).clone())
    }
}

impl<E: Element> Field for LiveDict<E> {
    type Ir = IndexMap<String, E::Ir>;

    fn to_ir(&self, py: Python<'_>, path: &Path<'_>) -> PyResult<IndexMap<String, E::Ir>> {
        let dict = self.dict.bind(py);
        let mut items = IndexMap::with_capacity(dict.len());
        for (key, value) in dict.iter() {
            let key = match key.cast::<PyString>() {
                Ok(text) => text.to_cow()?.into_owned(),
                Err(_) => return Err(path.type_error("str keys", &key)),
            };
            let value = E::element_to_ir(&value, &path.key(&key)?)?;
            items.insert(key, value);
        }
        Ok(items)
    }

    fn from_ir(py: Python<'_>, ir: &IndexMap<String, E::Ir>) -> PyResult<Self> {
        let dict = PyDict::new(py);
        for (key, value) in ir {
            dict.set_item(key, E::element_from_ir(py, value)?)?;
        }
        Ok(LiveDict {
            dict: dict.unbind(),
            element: PhantomData,
        })
    }

    fn empty(py: Python<'_>) -> Self {
        LiveDict {
            dict: PyDict::new(py).unbind(),
            element: PhantomData,
        }
    }

    fn repr(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let dict = self.dict.bind(py);
        container_repr(dict.as_any(), dict.len())
    }

    fn traverse(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.dict)
    }
}

/// The IR value of a field, boxed when the IR boxes it (`Option<Box<Expression>>`).
#[macro_export]
macro_rules! ir_of_field {
    ($value:expr, boxed) => {
        $value.map(Box::new)
    };
    ($value:expr) => {
        $value
    };
}

#[macro_export]
macro_rules! field_from_ir {
    ($py:ident, $ir_field:expr, boxed) => {
        $crate::convert::object_from_ir($py, $ir_field.as_deref())?
    };
    ($py:ident, $ir_field:expr) => {
        $crate::convert::Field::from_ir($py, &$ir_field)?
    };
}

/// A `#[pyclass]` for one IR type: a keyword-only constructor, `get`/`set` attributes, the
/// conversion, and `==`, `copy`, `deepcopy`, pickle, `to_json`/`from_json` and `repr` through
/// the IR. Fields the IR boxes are marked `as boxed`; extra methods go in `methods { ... }`.
/// `check <fn>` validates each converted value (`fn(&Ir) -> Result<(), String>`), so an invalid
/// one is a `ValueError` naming its path.
#[macro_export]
macro_rules! model {
    (
        $(#[$meta:meta])*
        $name:ident => $ir_module:ident :: $ir:ident $(check $check:path)? {
            $($field:ident : $field_type:ty $(as $mode:ident)?),* $(,)?
        }
        $(methods { $($methods:tt)* })?
    ) => {
        $(#[$meta])*
        #[pyclass(module = "oxymorph")]
        pub struct $name {
            $(
                #[pyo3(get, set)]
                pub $field: $field_type,
            )*
        }

        impl $crate::convert::Model for $name {
            type Ir = $ir_module::$ir;

            fn to_ir(
                &self,
                py: Python<'_>,
                path: &$crate::convert::Path<'_>,
            ) -> PyResult<$ir_module::$ir> {
                let ir = $ir_module::$ir {
                    $(
                        $field: $crate::ir_of_field!(
                            $crate::convert::Field::to_ir(
                                &self.$field,
                                py,
                                &path.field(stringify!($field))?,
                            )?
                            $(, $mode)?
                        ),
                    )*
                };
                $($check(&ir).map_err(|message| path.value_error(&message))?;)?
                Ok(ir)
            }

            fn from_ir(py: Python<'_>, ir: &$ir_module::$ir) -> PyResult<Self> {
                Ok($name {
                    $($field: $crate::field_from_ir!(py, ir.$field $(, $mode)?),)*
                })
            }
        }

        #[pymethods]
        impl $name {
            #[new]
            #[allow(clippy::too_many_arguments)]
            #[pyo3(signature = (*, $($field = None::<$field_type>),*))]
            fn new(py: Python<'_>, $($field: Option<$field_type>),*) -> Self {
                $name {
                    $(
                        $field: match $field {
                            Some(value) => value,
                            None => $crate::convert::Field::empty(py),
                        },
                    )*
                }
            }

            fn __eq__(&self, py: Python<'_>, other: PyRef<'_, Self>) -> PyResult<bool> {
                use $crate::convert::Model;
                let root = $crate::convert::Path::ROOT;
                Ok(self.to_ir(py, &root)? == other.to_ir(py, &root)?)
            }

            fn __deepcopy__(&self, py: Python<'_>, _memo: &Bound<'_, PyAny>) -> PyResult<Self> {
                use $crate::convert::Model;
                Self::from_ir(py, &self.to_ir(py, &$crate::convert::Path::ROOT)?)
            }

            fn __reduce__<'py>(
                slf: &Bound<'py, Self>,
            ) -> PyResult<(Bound<'py, PyAny>, (String,))> {
                let text = slf.try_borrow()?.to_json(slf.py())?;
                Ok((slf.get_type().getattr("from_json")?, (text,)))
            }

            /// The IR's JSON: deterministic, equal for equal values (a content-hash input).
            fn to_json(&self, py: Python<'_>) -> PyResult<String> {
                use $crate::convert::Model;
                let ir = self.to_ir(py, &$crate::convert::Path::ROOT)?;
                serde_json::to_string(&ir).map_err($crate::convert::value_error)
            }

            #[classmethod]
            fn from_json(
                _class: &Bound<'_, pyo3::types::PyType>,
                py: Python<'_>,
                text: &str,
            ) -> PyResult<Self> {
                use $crate::convert::Model;
                let ir: $ir_module::$ir = serde_json::from_str(text).map_err($crate::convert::value_error)?;
                Self::from_ir(py, &ir)
            }

            fn __repr__(slf: &Bound<'_, Self>) -> PyResult<String> {
                let Some(_guard) = $crate::convert::ReprGuard::enter(slf.as_any()) else {
                    return Ok(format!("{}(...)", stringify!($name)));
                };
                let py = slf.py();
                let this = slf.try_borrow()?;
                let mut parts: Vec<String> = Vec::new();
                $(
                    if let Some(text) = $crate::convert::Field::repr(&this.$field, py)? {
                        parts.push(format!("{}={}", stringify!($field), text));
                    }
                )*
                Ok(format!("{}({})", stringify!($name), parts.join(", ")))
            }

            fn __traverse__(
                &self,
                visit: pyo3::PyVisit<'_>,
            ) -> Result<(), pyo3::PyTraverseError> {
                $($crate::convert::Field::traverse(&self.$field, &visit)?;)*
                Ok(())
            }

            fn __clear__(&mut self) {
                $($crate::convert::Field::clear(&mut self.$field);)*
            }

            $($($methods)*)?
        }
    };
}
