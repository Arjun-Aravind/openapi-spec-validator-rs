//! Conversion between Python objects and [`serde_json::Value`].

use std::collections::HashSet;

use openapi_spec_validator_rs::pyrepr::{self, PathItem};
use pyo3::exceptions::{PyRecursionError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde_json::{Map, Number, Value};

/// The deepest container nesting accepted, the same limit the file reader applies
/// (upstream's `json.dumps` round trip). It also keeps the recursive validator well
/// within the stack.
const MAX_DEPTH: usize = 9989;

/// Converts a spec given as Python objects.
///
/// Accepts what `json.loads` produces (`dict` with `str` keys, `list`, `str`, `int`,
/// `float`, `bool`, `None`) plus `tuple` as an array, including subclasses such as
/// `OrderedDict` or `StrEnum`. NaN and infinities become `null` and integers outside
/// the 64-bit range become floats, exactly as the file reader loads them. Anything
/// else raises: `TypeError` for other types and non-`str` keys, `ValueError` for
/// circular references, `RecursionError` beyond [`MAX_DEPTH`].
pub fn to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let mut converter = Converter {
        active: HashSet::new(),
    };
    converter
        .convert(obj, 0)
        .map_err(|failure| failure.into_err())
}

struct Converter {
    /// Containers on the current path, by address, to detect cycles.
    active: HashSet<usize>,
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
    NonStringKey { type_name: String, repr: String },
    Circular,
    TooDeep,
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
            let mut map = Map::with_capacity(dict.len());
            for (key, value) in dict.iter() {
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
        Err(Failure::new(FailureKind::UnsupportedType(type_name(obj))))
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
        let id = obj.as_ptr() as usize;
        if !self.active.insert(id) {
            return Err(Failure::new(FailureKind::Circular));
        }
        Ok(id)
    }
}

/// Converts a value to the Python objects `json.loads` would produce.
pub fn to_python<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
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
                list.append(to_python(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, item) in map {
                dict.set_item(key, to_python(py, item)?)?;
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
