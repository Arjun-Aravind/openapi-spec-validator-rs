//! `json.loads(json.dumps(data))` over the constructed object graph.
//!
//! Mirrors the C encoder's traversal order (depth first, dict items in insertion order, keys
//! converted before their values), its errors, and its recursion limit, then builds the value
//! `json.loads` would return: string keys, last duplicate wins in the first position.

use serde_json::{Map, Number, Value};

use super::bigint::BigInt;
use super::compose::{Node, NodeKind};
use super::construct::{Graph, Obj};
use super::pyfmt::json_float;
use super::scalars::INT_TO_STR_LIMIT;

const CIRCULAR: &str = "Circular reference detected";
const RECURSION: &str = "maximum recursion depth exceeded while encoding a JSON object";

enum Frame {
    List {
        items: ListRef,
        next: usize,
        out: Vec<Value>,
    },
    Dict {
        id: u32,
        next: usize,
        out: Map<String, Value>,
        key: String,
    },
}

#[derive(Clone, Copy)]
enum ListRef {
    List(u32),
    Tuple(u32),
}

enum Opened {
    Leaf(Value),
    Frame(Frame),
}

struct Encoder<'a> {
    nodes: &'a mut [Node],
    graph: &'a Graph,
    list_marks: Vec<bool>,
    dict_marks: Vec<bool>,
    depth_limit: usize,
    /// Strings can be moved out of the nodes when no node is reachable twice.
    move_strings: bool,
}

pub(super) fn to_value(
    nodes: &mut [Node],
    graph: &Graph,
    root: Obj,
    depth_limit: usize,
    move_strings: bool,
) -> Result<Value, String> {
    let mut enc = Encoder {
        nodes,
        graph,
        list_marks: vec![false; graph.lists.len()],
        dict_marks: vec![false; graph.dicts.len()],
        depth_limit,
        move_strings,
    };
    let mut stack: Vec<Frame> = Vec::new();
    match enc.open(root, 0)? {
        Opened::Leaf(v) => return Ok(v),
        Opened::Frame(f) => stack.push(f),
    }
    loop {
        let depth = stack.len();
        let Some(top) = stack.last_mut() else {
            return Ok(Value::Null);
        };
        let child = match top {
            Frame::List { items, next, .. } => {
                let child = enc.item(*items, *next);
                *next += 1;
                child
            }
            Frame::Dict { id, next, key, .. } => {
                let entry = enc.graph.dicts[*id as usize].get(*next).copied();
                *next += 1;
                match entry {
                    Some((k, v)) => {
                        *key = enc.key_string(k)?;
                        Some(v)
                    }
                    None => None,
                }
            }
        };
        match child {
            Some(obj) => match enc.open(obj, depth)? {
                Opened::Leaf(v) => attach(top, v),
                Opened::Frame(f) => stack.push(f),
            },
            None => {
                let Some(done) = stack.pop() else {
                    return Ok(Value::Null);
                };
                let value = enc.close(done);
                match stack.last_mut() {
                    Some(parent) => attach(parent, value),
                    None => return Ok(value),
                }
            }
        }
    }
}

fn attach(frame: &mut Frame, value: Value) {
    match frame {
        Frame::List { out, .. } => out.push(value),
        Frame::Dict { out, key, .. } => {
            out.insert(std::mem::take(key), value);
        }
    }
}

fn type_error(type_name: &str) -> String {
    format!("Object of type {type_name} is not JSON serializable")
}

impl Encoder<'_> {
    fn item(&self, items: ListRef, i: usize) -> Option<Obj> {
        match items {
            ListRef::List(id) => self.graph.lists[id as usize].get(i).copied(),
            ListRef::Tuple(id) => {
                let (a, b) = self.graph.tuples[id as usize];
                [a, b].get(i).copied()
            }
        }
    }

    fn string(&mut self, id: u32) -> String {
        match &mut self.nodes[id as usize].kind {
            NodeKind::Scalar(s) if self.move_strings => std::mem::take(s),
            NodeKind::Scalar(s) => s.clone(),
            _ => String::new(),
        }
    }

    /// `encoder_listencode_obj` for one object, `parent_depth` containers deep.
    fn open(&mut self, obj: Obj, parent_depth: usize) -> Result<Opened, String> {
        let leaf = match obj {
            Obj::None | Obj::Nan(_) => Value::Null,
            Obj::Bool(b) => Value::Bool(b),
            Obj::Int(i) => Value::Number(i.into()),
            Obj::BigInt(id) => big_value(&self.graph.bigints[id as usize])?,
            Obj::Float(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
            Obj::Str(id) => Value::String(self.string(id)),
            Obj::Bytes => return Err(type_error("bytes")),
            Obj::Date => return Err(type_error("date")),
            Obj::DateTime => return Err(type_error("datetime")),
            Obj::Set => return Err(type_error("set")),
            Obj::List(_) | Obj::Tuple(_) | Obj::Dict(_) => {
                // Py_EnterRecursiveCall happens before the emptiness and circularity checks.
                if parent_depth + 1 > self.depth_limit {
                    return Err(RECURSION.to_owned());
                }
                let opened = self.open_container(obj);
                if let Opened::Frame(frame) = &opened {
                    self.enter(frame)?;
                }
                return Ok(opened);
            }
        };
        Ok(Opened::Leaf(leaf))
    }

    fn open_container(&mut self, obj: Obj) -> Opened {
        match obj {
            Obj::Dict(id) => {
                let len = self.graph.dicts[id as usize].len();
                if len == 0 {
                    return Opened::Leaf(Value::Object(Map::new()));
                }
                Opened::Frame(Frame::Dict {
                    id,
                    next: 0,
                    out: Map::with_capacity(len),
                    key: String::new(),
                })
            }
            Obj::List(id) => {
                let len = self.graph.lists[id as usize].len();
                if len == 0 {
                    return Opened::Leaf(Value::Array(Vec::new()));
                }
                Opened::Frame(Frame::List {
                    items: ListRef::List(id),
                    next: 0,
                    out: Vec::with_capacity(len),
                })
            }
            // Tuples (from !!omap / !!pairs) always hold two items and are never shared.
            Obj::Tuple(id) => Opened::Frame(Frame::List {
                items: ListRef::Tuple(id),
                next: 0,
                out: Vec::with_capacity(2),
            }),
            _ => Opened::Leaf(Value::Null),
        }
    }

    fn close(&mut self, frame: Frame) -> Value {
        match frame {
            Frame::List { items, out, .. } => {
                if let ListRef::List(id) = items {
                    self.list_marks[id as usize] = false;
                }
                Value::Array(out)
            }
            Frame::Dict { id, out, .. } => {
                self.dict_marks[id as usize] = false;
                Value::Object(out)
            }
        }
    }

    fn key_string(&mut self, key: Obj) -> Result<String, String> {
        Ok(match key {
            Obj::Str(id) => self.string(id),
            Obj::Float(f) => json_float(f),
            Obj::Nan(_) => "NaN".to_owned(),
            Obj::Bool(b) => if b { "true" } else { "false" }.to_owned(),
            Obj::None => "null".to_owned(),
            Obj::Int(i) => i.to_string(),
            Obj::BigInt(id) => {
                let digits = self.graph.bigints[id as usize].to_decimal();
                if digits.trim_start_matches('-').len() > 4300 {
                    return Err(INT_TO_STR_LIMIT.to_owned());
                }
                digits
            }
            other => {
                // The C encoder names the key type by `tp_name`, which is module-qualified for
                // the datetime types (unlike the value error, which uses `__name__`).
                let name = match other {
                    Obj::Bytes => "bytes",
                    Obj::Date => "datetime.date",
                    Obj::DateTime => "datetime.datetime",
                    Obj::Set => "set",
                    Obj::Dict(_) => "dict",
                    Obj::Tuple(_) => "tuple",
                    _ => "list",
                };
                return Err(format!(
                    "keys must be str, int, float, bool or None, not {name}"
                ));
            }
        })
    }
}

impl Encoder<'_> {
    /// Marks a non-empty list/dict as being encoded, failing if it already is.
    fn enter(&mut self, frame: &Frame) -> Result<(), String> {
        let mark = match frame {
            Frame::List {
                items: ListRef::List(id),
                ..
            } => &mut self.list_marks[*id as usize],
            Frame::Dict { id, .. } => &mut self.dict_marks[*id as usize],
            Frame::List { .. } => return Ok(()),
        };
        if *mark {
            return Err(CIRCULAR.to_owned());
        }
        *mark = true;
        Ok(())
    }
}

/// A Python int after the JSON round trip: exact when it fits 64 bits, otherwise the f64
/// serde_json produces for the same decimal literal (so JSON and YAML inputs agree).
fn big_value(n: &BigInt) -> Result<Value, String> {
    if let Some(i) = n.to_i64() {
        return Ok(Value::Number(i.into()));
    }
    if let Some(u) = n.to_u64() {
        return Ok(Value::Number(u.into()));
    }
    let digits = n.to_decimal();
    if digits.trim_start_matches('-').len() > 4300 {
        return Err(INT_TO_STR_LIMIT.to_owned());
    }
    // Beyond f64's range serde_json reports "number out of range"; NaN/inf-like values have
    // no `Value` representation, so they become null like infinite floats do.
    Ok(serde_json::from_str::<Value>(&digits).unwrap_or(Value::Null))
}
