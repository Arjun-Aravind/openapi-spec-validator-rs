//! `oneOf` and `anyOf` without their error context, for the document checks of Swagger
//! 2.0 and OpenAPI 3.0.
//!
//! When the crate's `oneOf` or `anyOf` fails, it collects every branch's errors (the
//! context) at once, copying the instance into each, and its keywords collect their
//! errors eagerly. The 3.0 meta-schema wraps every schema in such a `oneOf`, so on a
//! document whose invalid schemas nest and repeat, a single error holds copies growing
//! with depth times width, and even the first error costs all of them. Upstream builds a
//! context only for the errors it yields. These keywords only decide validity; when an
//! error of theirs is reported, its context is computed from a validator for each
//! branch ([`Branches`]), borrowing the instance.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};

use jsonschema::paths::Location;
use jsonschema::{Keyword, Validator};
use serde_json::{Map, Value};

/// The message of the error when no branch is valid.
pub(crate) const NONE_VALID: &str = "\0none of the branches is valid";
/// The message of the error when more than one `oneOf` branch is valid.
pub(crate) const SEVERAL_VALID: &str = "\0several branches are valid";

/// Builds a validator for a schema, with the options of the validator using these
/// keywords.
pub(crate) type Build = fn(&Value) -> Validator;

/// The branches of one `oneOf` or `anyOf`, each validated by a validator of its own built
/// on first use.
pub(crate) struct Branches {
    one_of: bool,
    /// Absolute URIs of the branches.
    uris: Vec<String>,
    validators: Vec<OnceLock<Validator>>,
    build: Build,
}

impl Branches {
    pub(crate) fn len(&self) -> usize {
        self.uris.len()
    }

    /// The validator of branch `index`.
    pub(crate) fn validator(&self, index: usize) -> &Validator {
        self.validators[index]
            .get_or_init(|| (self.build)(&serde_json::json!({ "$ref": self.uris[index] })))
    }

    /// How many branches `instance` is valid against, counting no further than `stop`.
    fn valid(&self, instance: &Value, stop: usize) -> usize {
        let mut valid = 0;
        for index in 0..self.len() {
            if self.validator(index).is_valid(instance) {
                valid += 1;
                if valid == stop {
                    break;
                }
            }
        }
        valid
    }
}

/// Every `oneOf` / `anyOf` compiled so far, by the address of its value in the bundled
/// meta-schemas, which outlive the process.
static BY_ADDRESS: LazyLock<Mutex<HashMap<usize, Arc<Branches>>>> = LazyLock::new(Mutex::default);

fn address(value: &Value) -> usize {
    std::ptr::from_ref(value) as usize
}

/// The branches of the `oneOf` / `anyOf` whose value is `value`, a part of a bundled
/// meta-schema, if one of these keywords was compiled from it.
pub(crate) fn branches(value: &Value) -> Option<Arc<Branches>> {
    BY_ADDRESS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&address(value))
        .cloned()
}

/// What a keyword factory returns.
type Compiled<'a> = Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>>;

/// A keyword factory for `oneOf` (`one_of`) or `anyOf` in the bundled `resources`, given
/// as their URI and contents.
pub(crate) fn factory(
    resources: &'static [(&'static str, &'static LazyLock<Value>)],
    one_of: bool,
    build: Build,
) -> impl for<'a> Fn(&'a Map<String, Value>, &'a Value, Location) -> Compiled<'a> + Send + Sync + 'static
{
    move |_, value, location| {
        let pointer = location.to_string();
        // The resource the keyword belongs to: the one holding this very value.
        let uri = resources
            .iter()
            .find(|(_, contents)| {
                contents
                    .pointer(&pointer)
                    .is_some_and(|found| std::ptr::eq(found, value))
            })
            .map(|(uri, _)| *uri)
            .ok_or_else(|| {
                jsonschema::ValidationError::custom(format!(
                    "no bundled meta-schema holds the keyword at {pointer}"
                ))
            })?;
        let branches = Arc::clone(
            BY_ADDRESS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(address(value))
                .or_insert_with(|| {
                    let count = value.as_array().map_or(0, Vec::len);
                    Arc::new(Branches {
                        one_of,
                        uris: (0..count)
                            .map(|index| format!("{uri}#{}/{index}", encode(&pointer)))
                            .collect(),
                        validators: (0..count).map(|_| OnceLock::new()).collect(),
                        build,
                    })
                }),
        );
        Ok(Box::new(Combinator(branches)))
    }
}

/// A JSON pointer as a URI fragment: everything but unreserved characters and `/`
/// percent-encoded (pattern keys such as `^[a-zA-Z0-9\.\-_]+$` are not valid there).
fn encode(pointer: &str) -> String {
    let mut encoded = String::with_capacity(pointer.len());
    for byte in pointer.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

struct Combinator(Arc<Branches>);

impl<'i> Keyword<'i> for Combinator {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        crate::context::check();
        let branches = &self.0;
        match branches.valid(instance, if branches.one_of { 2 } else { 1 }) {
            0 => Err(jsonschema::ValidationError::custom(NONE_VALID)),
            1 => Ok(()),
            _ => Err(jsonschema::ValidationError::custom(SEVERAL_VALID)),
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        crate::context::check();
        let branches = &self.0;
        let valid = branches.valid(instance, if branches.one_of { 2 } else { 1 });
        valid == 1
    }
}
