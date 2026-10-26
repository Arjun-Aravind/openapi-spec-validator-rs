//! Conversion between Python objects and [`serde_json::Value`].

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use openapi_spec_validator_rs::pyrepr::{self, Foreign, PathItem};
use pyo3::exceptions::{PyRecursionError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use serde_json::{Map, Number, Value};

/// The deepest container nesting accepted, the same limit the file reader applies
/// (upstream's `json.dumps` round trip). It also keeps the recursive validator well
/// within the stack.
const MAX_DEPTH: usize = 9989;

/// The Python objects a converted document stands in for (see [`pyrepr::foreign`]), so
/// that [`to_python`] can hand the objects themselves back (an error's `instance` is the
/// `datetime.date` from the document, as upstream's is), and the exceptions their
/// `repr()` or `str()` raised.
#[derive(Default)]
pub struct PythonObjects {
    /// By a hash of what their stand-ins record; objects described alike are alike to
    /// the validator (equal stand-ins), and the first one seen represents them all.
    by_description: HashMap<u64, Py<PyAny>>,
    /// The exceptions raised while describing objects, by the marker that took the
    /// place of the text (see [`PythonObjects::raised_for`]).
    failures: Vec<(String, PyErr)>,
}

impl PythonObjects {
    fn key(object: Foreign<'_>) -> u64 {
        let mut hasher = DefaultHasher::new();
        (object.type_name, object.repr, object.str).hash(&mut hasher);
        hasher.finish()
    }

    fn get(&self, object: Foreign<'_>) -> Option<&Py<PyAny>> {
        self.by_description.get(&Self::key(object))
    }

    /// The exception to raise for `text` (a message or rendering), if it includes the
    /// `repr()` or `str()` of an object that raised when asked for it: upstream asks for
    /// that text exactly when it builds such a message, and the exception propagates.
    pub fn raised_for(&self, py: Python<'_>, text: &str) -> Option<PyErr> {
        if !text.contains(FAILURE_MARKER) {
            return None;
        }
        self.failures
            .iter()
            .find(|(marker, _)| text.contains(marker.as_str()))
            .map(|(_, error)| error.clone_ref(py))
    }
}

/// Begins the text standing in for a `repr()` or `str()` that raised.
const FAILURE_MARKER: &str = "\u{0}raised:";

/// Converts a spec given as Python objects, with the objects its result stands in for.
///
/// Accepts what `json.loads` produces (`dict` with `str` keys, `list`, `str`, `int`,
/// `float`, `bool`, `None`) plus `tuple` as an array, including subclasses such as
/// `OrderedDict` or `StrEnum`. NaN and infinities become `null` and integers outside
/// the 64-bit range become floats, exactly as the file reader loads them. An object of
/// any other type that is not a number (`datetime.date`, `bytes`, `set`, ...) becomes
/// its stand-in (see [`pyrepr::foreign`]), which jsonschema treats as an object of no
/// type, as upstream does. Anything else raises: `TypeError` for other numbers
/// (`Decimal`, `Fraction`, ...) and non-`str` keys, `ValueError` for circular
/// references, `RecursionError` beyond [`MAX_DEPTH`].
///
/// The objects are described (`repr()`, `str()`) only once the whole document has been
/// read, so that code they run cannot change the document while it is being read. A
/// description that raises is recorded instead (see [`PythonObjects::raised_for`]).
pub fn to_value(obj: &Bound<'_, PyAny>) -> PyResult<(Value, PythonObjects)> {
    let mut converter = Converter {
        active: HashSet::new(),
        pending: Vec::new(),
        number_type: None,
    };
    let mut value = converter
        .convert(obj, 0)
        .map_err(|failure| failure.into_err())?;
    let mut objects = PythonObjects::default();
    if !converter.pending.is_empty() {
        let described = converter
            .pending
            .iter()
            .enumerate()
            .map(|(index, object)| describe(object.bind(obj.py()), index, &mut objects))
            .collect::<Vec<_>>();
        fill_in(&mut value, &described);
        for (object, description) in converter.pending.into_iter().zip(&described) {
            if let Some(described) = pyrepr::foreign(description) {
                objects
                    .by_description
                    .entry(PythonObjects::key(described))
                    .or_insert(object);
            }
        }
    }
    Ok((value, objects))
}

struct Converter {
    /// Containers on the current path, by address, to detect cycles.
    active: HashSet<usize>,
    /// Objects JSON has no counterpart for, in the order they were met; the document
    /// holds a placeholder for each until [`fill_in`] replaces it.
    pending: Vec<Py<PyAny>>,
    /// `numbers.Number`, imported on first use.
    number_type: Option<Py<PyType>>,
}

/// The key of a placeholder for the `n`th pending object (see [`Converter::pending`]).
const PLACEHOLDER_KEY: &str = "\u{0}pending python object";

/// The stand-in for `object`, the `index`th object met; a `repr()` or `str()` that
/// raises is recorded in `objects` and replaced by a marker.
fn describe(object: &Bound<'_, PyAny>, index: usize, objects: &mut PythonObjects) -> Value {
    let mut text = |result: PyResult<Bound<'_, PyString>>, what: &str| match result
        .and_then(|text| Ok(text.to_str()?.to_owned()))
    {
        Ok(text) => text,
        Err(error) => {
            let marker = format!("{FAILURE_MARKER}{what}:{index}\u{0}");
            objects.failures.push((marker.clone(), error));
            marker
        }
    };
    let class = object.get_type();
    let type_name = tp_name(&class);
    let class_repr = text(class.repr(), "class");
    let repr = text(object.repr(), "repr");
    let str = text(object.str(), "str");
    pyrepr::foreign_value(&type_name, &class_repr, &repr, &str)
}

/// `tp_name`, as C-level messages print a type: a heap type (a class defined in Python)
/// by its `__name__`, a static type by its module-qualified name, except in `builtins`.
fn tp_name(class: &Bound<'_, PyType>) -> String {
    // SAFETY: `as_type_ptr` is a valid type object pointer.
    let flags = unsafe { pyo3::ffi::PyType_GetFlags(class.as_type_ptr()) };
    let name = |r: PyResult<Bound<'_, PyString>>| r.map(|n| n.to_string()).unwrap_or_default();
    if flags & pyo3::ffi::Py_TPFLAGS_HEAPTYPE != 0 {
        return name(class.name());
    }
    let module = name(class.module());
    let qualname = name(class.qualname());
    if module == "builtins" || module.is_empty() {
        qualname
    } else {
        format!("{module}.{qualname}")
    }
}

/// Replaces every placeholder in `value` with the stand-in described for it.
fn fill_in(value: &mut Value, described: &[Value]) {
    match value {
        Value::Object(map) => {
            if let Some(index) = map
                .get(PLACEHOLDER_KEY)
                .and_then(Value::as_u64)
                .filter(|_| map.len() == 1)
                .and_then(|index| usize::try_from(index).ok())
            {
                if let Some(stand_in) = described.get(index) {
                    *value = stand_in.clone();
                }
                return;
            }
            map.values_mut().for_each(|child| fill_in(child, described));
        }
        Value::Array(items) => items.iter_mut().for_each(|child| fill_in(child, described)),
        _ => {}
    }
}

/// A conversion failure; its location is collected in reverse while unwinding so that the
/// successful path never pays for tracking it.
struct Failure {
    kind: FailureKind,
    reversed_path: Vec<PathItem>,
}

enum FailureKind {
    Python(PyErr),
    UnsupportedType(String),
    NonStringKey {
        type_name: String,
        repr: String,
    },
    Circular,
    TooDeep,
    /// Deeper than the stack of the thread the conversion had to run on allows.
    StackTooSmall,
}

impl Failure {
    fn new(kind: FailureKind) -> Self {
        Self {
            kind,
            reversed_path: Vec::new(),
        }
    }

    fn at(mut self, item: PathItem) -> Self {
        self.reversed_path.push(item);
        self
    }

    fn into_err(mut self) -> PyErr {
        self.reversed_path.reverse();
        let location = pyrepr::format_as_index("spec", &self.reversed_path);
        match self.kind {
            FailureKind::Python(err) => err,
            FailureKind::UnsupportedType(type_name) => PyTypeError::new_err(format!(
                "unsupported type '{type_name}' at {location}: a spec may only contain \
                 dict, list, tuple, str, int, float, bool and None"
            )),
            FailureKind::NonStringKey { type_name, repr } => PyTypeError::new_err(format!(
                "keys must be str, not '{type_name}': {repr} in {location}"
            )),
            FailureKind::Circular => {
                PyValueError::new_err(format!("Circular reference detected at {location}"))
            }
            FailureKind::TooDeep => PyRecursionError::new_err(format!(
                "maximum recursion depth exceeded: the spec nests more than {MAX_DEPTH} \
                 containers"
            )),
            FailureKind::StackTooSmall => {
                PyRuntimeError::new_err(openapi_spec_validator_rs::STACK_TOO_SMALL)
            }
        }
    }
}

impl From<PyErr> for Failure {
    fn from(err: PyErr) -> Self {
        Self::new(FailureKind::Python(err))
    }
}

fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map(|name| name.to_string())
        .unwrap_or_else(|_| "?".to_owned())
}

impl Converter {
    fn convert(&mut self, obj: &Bound<'_, PyAny>, depth: usize) -> Result<Value, Failure> {
        if let Ok(s) = obj.cast::<PyString>() {
            return Ok(Value::String(s.to_str()?.to_owned()));
        }
        if let Ok(dict) = obj.cast::<PyDict>() {
            let id = self.enter(obj, depth)?;
            let size = dict.len();
            let mut map = Map::with_capacity(size);
            let mut position: pyo3::ffi::Py_ssize_t = 0;
            loop {
                // `PyDict_Next` rather than PyO3's iterator, which panics when the dict
                // changes size; code run while converting (a `__class__` property, say)
                // can change it, and Python raises `RuntimeError` then.
                let mut key_ptr = std::ptr::null_mut();
                let mut value_ptr = std::ptr::null_mut();
                // SAFETY: `dict` is a dict; the borrowed references are turned into
                // owned ones at once, before any Python code can run.
                let (key, value) = unsafe {
                    if pyo3::ffi::PyDict_Next(
                        dict.as_ptr(),
                        &mut position,
                        &mut key_ptr,
                        &mut value_ptr,
                    ) == 0
                    {
                        break;
                    }
                    (
                        Bound::from_borrowed_ptr(obj.py(), key_ptr),
                        Bound::from_borrowed_ptr(obj.py(), value_ptr),
                    )
                };
                let Ok(key) = key.cast::<PyString>() else {
                    let repr = key.repr().map(|r| r.to_string()).unwrap_or_default();
                    return Err(Failure::new(FailureKind::NonStringKey {
                        type_name: type_name(&key),
                        repr,
                    }));
                };
                let key = key.to_str()?;
                let value = self
                    .convert(&value, depth + 1)
                    .map_err(|failure| failure.at(PathItem::Key(key.to_owned())))?;
                map.insert(key.to_owned(), value);
                if dict.len() != size {
                    return Err(Failure::new(FailureKind::Python(PyRuntimeError::new_err(
                        "dictionary changed size during iteration",
                    ))));
                }
            }
            self.active.remove(&id);
            return Ok(Value::Object(map));
        }
        if let Ok(list) = obj.cast::<PyList>() {
            return self.array(obj, list.iter(), list.len(), depth);
        }
        // `bool` is a subclass of `int`, so it must be checked first.
        if let Ok(b) = obj.cast::<PyBool>() {
            return Ok(Value::Bool(b.is_true()));
        }
        if obj.is_instance_of::<PyInt>() {
            if let Ok(n) = obj.extract::<i64>() {
                return Ok(Value::from(n));
            }
            if let Ok(n) = obj.extract::<u64>() {
                return Ok(Value::from(n));
            }
            // Beyond 64 bits: the nearest float (what `float(n)` gives, and what the
            // reader produces for such literals), or null when even that overflows.
            return Ok(obj
                .extract::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map_or(Value::Null, Value::Number));
        }
        if let Ok(f) = obj.cast::<PyFloat>() {
            return Ok(Number::from_f64(f.value()).map_or(Value::Null, Value::Number));
        }
        if obj.is_none() {
            return Ok(Value::Null);
        }
        if let Ok(tuple) = obj.cast::<PyTuple>() {
            return self.array(obj, tuple.iter(), tuple.len(), depth);
        }
        // Other numbers (`Decimal`, `Fraction`, ...) are numbers to jsonschema, which
        // the document model cannot express: refused rather than misjudged.
        if self.is_number(obj)? {
            return Err(Failure::new(FailureKind::UnsupportedType(type_name(obj))));
        }
        self.foreign(obj)
    }

    /// `isinstance(obj, numbers.Number)`.
    fn is_number(&mut self, obj: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = obj.py();
        if self.number_type.is_none() {
            let number = py
                .import("numbers")?
                .getattr("Number")?
                .cast_into::<PyType>()?;
            self.number_type = Some(number.unbind());
        }
        let number = self.number_type.as_ref().expect("set above").bind(py);
        obj.is_instance(number)
    }

    /// A placeholder for an object of a type JSON has no counterpart for, described
    /// once the whole document has been read (see [`to_value`]).
    fn foreign(&mut self, obj: &Bound<'_, PyAny>) -> Result<Value, Failure> {
        let index = self.pending.len();
        self.pending.push(obj.clone().unbind());
        let mut map = Map::with_capacity(1);
        map.insert(PLACEHOLDER_KEY.to_owned(), Value::from(index));
        Ok(Value::Object(map))
    }

    fn array<'py>(
        &mut self,
        obj: &Bound<'py, PyAny>,
        items: impl Iterator<Item = Bound<'py, PyAny>>,
        len: usize,
        depth: usize,
    ) -> Result<Value, Failure> {
        let id = self.enter(obj, depth)?;
        let mut values = Vec::with_capacity(len);
        for (index, item) in items.enumerate() {
            let value = self
                .convert(&item, depth + 1)
                .map_err(|failure| failure.at(PathItem::Index(index)))?;
            values.push(value);
        }
        self.active.remove(&id);
        Ok(Value::Array(values))
    }

    /// Registers a container on the current path, rejecting cycles and deep nesting.
    fn enter(&mut self, obj: &Bound<'_, PyAny>, depth: usize) -> Result<usize, Failure> {
        if depth >= MAX_DEPTH {
            return Err(Failure::new(FailureKind::TooDeep));
        }
        if openapi_spec_validator_rs::nesting_limit().is_some_and(|limit| depth >= limit) {
            return Err(Failure::new(FailureKind::StackTooSmall));
        }
        let id = obj.as_ptr() as usize;
        if !self.active.insert(id) {
            return Err(Failure::new(FailureKind::Circular));
        }
        Ok(id)
    }
}

/// Converts a value to the Python objects `json.loads` would produce; a stand-in (see
/// [`pyrepr::foreign`]) becomes the object it was converted from, found in `objects`.
pub fn to_python<'py>(
    py: Python<'py>,
    value: &Value,
    objects: &PythonObjects,
) -> PyResult<Bound<'py, PyAny>> {
    if let Some(described) = pyrepr::foreign(value) {
        return Ok(match objects.get(described) {
            Some(object) => object.bind(py).clone(),
            // Not from this conversion (a document read from a file holding the stand-in's
            // shape, say): the next best thing is how it prints.
            None => PyString::new(py, described.repr).into_any(),
        });
    }
    Ok(match value {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any()
            } else {
                PyFloat::new(py, n.as_f64().unwrap_or(f64::NAN)).into_any()
            }
        }
        Value::String(s) => PyString::new(py, s).into_any(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(to_python(py, item, objects)?)?;
            }
            list.into_any()
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, item) in map {
                dict.set_item(key, to_python(py, item, objects)?)?;
            }
            dict.into_any()
        }
    })
}

/// Converts a jsonschema path (`path` / `schema_path`) to `str` and `int` items.
pub fn path_to_python<'py>(py: Python<'py>, path: &[PathItem]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for item in path {
        match item {
            PathItem::Key(key) => list.append(key)?,
            PathItem::Index(index) => list.append(index)?,
        }
    }
    Ok(list)
}
