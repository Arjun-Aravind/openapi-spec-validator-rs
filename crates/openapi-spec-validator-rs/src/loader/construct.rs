//! PyYAML's `SafeConstructor`, producing a small Python-like object graph.
//!
//! Collections are built the way PyYAML's generator constructors build them: the (empty)
//! container is created when first referenced and filled later, in FIFO order. That order
//! decides which error wins when a document has several, and it is what makes self-referencing
//! anchors produce circular containers.

use std::collections::{HashMap, VecDeque};

use super::bigint::BigInt;
use super::compose::{Composed, Node, NodeId, NodeKind};
use super::pyfmt::{marked_error, repr_str};
use super::resolve::Tag;
use super::scalars::{self, FloatValue, Timestamp};

/// A constructed Python object. Containers live in the `Graph` arenas; strings stay in the
/// scalar node that produced them.
#[derive(Clone, Copy, Debug)]
pub(super) enum Obj {
    None,
    Bool(bool),
    Int(i64),
    BigInt(u32),
    Float(f64),
    /// A NaN float together with its object identity (`SHARED_NAN` for PyYAML's `nan_value`).
    Nan(u32),
    Str(NodeId),
    Bytes,
    Date,
    DateTime,
    Set,
    List(u32),
    Dict(u32),
    Tuple(u32),
}

const SHARED_NAN: u32 = u32::MAX;

/// Roughly how deep PyYAML's recursive helpers (`flatten_mapping`, `construct_scalar`) can go
/// before Python's default recursion limit of 1000 frames trips.
const PY_RECURSION_LIMIT: usize = 990;
const RECURSION_ERROR: &str = "maximum recursion depth exceeded";

#[derive(Default)]
pub(super) struct Graph {
    pub lists: Vec<Vec<Obj>>,
    pub dicts: Vec<Vec<(Obj, Obj)>>,
    pub tuples: Vec<(Obj, Obj)>,
    pub bigints: Vec<BigInt>,
}

enum Task {
    Seq(NodeId, u32),
    Map(NodeId, u32),
    Set(NodeId),
    Omap {
        node: NodeId,
        list: u32,
        pairs: bool,
    },
}

struct Constructor<'a> {
    nodes: &'a mut [Node],
    name: &'a str,
    cache: Vec<Option<Obj>>,
    queue: VecDeque<Task>,
    graph: Graph,
}

/// `construct_document(get_single_node())`.
pub(super) fn construct(doc: &mut Composed, name: &str) -> Result<(Graph, Obj), String> {
    let Some(root) = doc.root else {
        return Ok((Graph::default(), Obj::None));
    };
    let mut c = Constructor {
        cache: vec![None; doc.nodes.len()],
        nodes: &mut doc.nodes,
        name,
        queue: VecDeque::new(),
        graph: Graph::default(),
    };
    let obj = c.construct_object(root)?;
    while let Some(task) = c.queue.pop_front() {
        c.run(task)?;
    }
    Ok((c.graph, obj))
}

impl Constructor<'_> {
    fn error(&self, context: Option<(&str, NodeId)>, problem: &str, at: NodeId) -> String {
        marked_error(
            self.name,
            context.map(|(text, _)| text),
            context.map(|(_, node)| self.nodes[node as usize].start),
            Some(problem),
            Some(self.nodes[at as usize].start),
        )
    }

    fn kind(&self, node: NodeId) -> &NodeKind {
        &self.nodes[node as usize].kind
    }

    fn scalar_str(&self, node: NodeId) -> &str {
        match self.kind(node) {
            NodeKind::Scalar(s) => s,
            _ => "",
        }
    }

    fn construct_object(&mut self, node: NodeId) -> Result<Obj, String> {
        if let Some(obj) = self.cache[node as usize] {
            return Ok(obj);
        }
        let obj = match &self.nodes[node as usize].tag {
            Tag::Null => {
                self.construct_scalar(node, 0)?;
                Obj::None
            }
            Tag::Bool => {
                let s = self.construct_scalar(node, 0)?;
                Obj::Bool(scalars::bool_value(self.scalar_str(s))?)
            }
            Tag::Int => {
                let s = self.construct_scalar(node, 0)?;
                let n = scalars::int_value(self.scalar_str(s))?;
                self.int_obj(n)
            }
            Tag::Float => {
                let s = self.construct_scalar(node, 0)?;
                match scalars::float_value(self.scalar_str(s))? {
                    FloatValue::SharedNan => Obj::Nan(SHARED_NAN),
                    FloatValue::Num(f) if f.is_nan() => Obj::Nan(node),
                    FloatValue::Num(f) => Obj::Float(f),
                }
            }
            Tag::Binary => {
                let s = self.construct_scalar(node, 0)?;
                if let Some(problem) = scalars::binary_problem(self.scalar_str(s)) {
                    return Err(self.error(None, &problem, node));
                }
                Obj::Bytes
            }
            Tag::Timestamp => {
                let s = self.construct_scalar(node, 0)?;
                // The regex is matched against `node.value`, which is a list for mappings.
                if !matches!(self.kind(node), NodeKind::Scalar(_)) {
                    return Err(crate::pyobj::not_a_string("list"));
                }
                match scalars::timestamp_value(self.scalar_str(s))? {
                    Timestamp::Date => Obj::Date,
                    Timestamp::DateTime => Obj::DateTime,
                }
            }
            Tag::Str => Obj::Str(self.construct_scalar(node, 0)?),
            Tag::Seq => {
                let list = self.new_list();
                self.queue.push_back(Task::Seq(node, list));
                Obj::List(list)
            }
            Tag::Omap | Tag::Pairs => {
                let pairs = self.nodes[node as usize].tag == Tag::Pairs;
                let list = self.new_list();
                self.queue.push_back(Task::Omap { node, list, pairs });
                Obj::List(list)
            }
            Tag::Map => {
                let dict = u32::try_from(self.graph.dicts.len()).unwrap_or(u32::MAX);
                self.graph.dicts.push(Vec::new());
                self.queue.push_back(Task::Map(node, dict));
                Obj::Dict(dict)
            }
            Tag::Set => {
                self.queue.push_back(Task::Set(node));
                Obj::Set
            }
            tag @ (Tag::Merge | Tag::Value | Tag::Yaml | Tag::Other(_)) => {
                let problem = format!(
                    "could not determine a constructor for the tag {}",
                    repr_str(&tag.uri())
                );
                return Err(self.error(None, &problem, node));
            }
        };
        self.cache[node as usize] = Some(obj);
        Ok(obj)
    }

    fn new_list(&mut self) -> u32 {
        self.graph.lists.push(Vec::new());
        u32::try_from(self.graph.lists.len() - 1).unwrap_or(u32::MAX)
    }

    fn int_obj(&mut self, n: BigInt) -> Obj {
        match n.to_i64() {
            Some(i) => Obj::Int(i),
            None => {
                self.graph.bigints.push(n);
                Obj::BigInt(u32::try_from(self.graph.bigints.len() - 1).unwrap_or(u32::MAX))
            }
        }
    }

    /// `SafeConstructor.construct_scalar`: the scalar node whose value is used.
    fn construct_scalar(&self, node: NodeId, depth: usize) -> Result<NodeId, String> {
        if depth > PY_RECURSION_LIMIT {
            return Err(RECURSION_ERROR.to_owned());
        }
        match self.kind(node) {
            NodeKind::Scalar(_) => Ok(node),
            NodeKind::Mapping(pairs) => {
                if let Some(&(_, value)) = pairs
                    .iter()
                    .find(|(k, _)| self.nodes[*k as usize].tag == Tag::Value)
                {
                    return self.construct_scalar(value, depth + 1);
                }
                Err(self.error(None, "expected a scalar node, but found mapping", node))
            }
            NodeKind::Sequence(_) => {
                Err(self.error(None, "expected a scalar node, but found sequence", node))
            }
        }
    }

    fn run(&mut self, task: Task) -> Result<(), String> {
        match task {
            Task::Seq(node, list) => {
                let len = match self.kind(node) {
                    NodeKind::Sequence(items) => items.len(),
                    other => {
                        let problem = format!("expected a sequence node, but found {}", other.id());
                        return Err(self.error(None, &problem, node));
                    }
                };
                let mut items = Vec::with_capacity(len);
                for i in 0..len {
                    let NodeKind::Sequence(children) = self.kind(node) else {
                        break;
                    };
                    let child = children[i];
                    items.push(self.construct_object(child)?);
                }
                self.graph.lists[list as usize] = items;
            }
            Task::Map(node, dict) => {
                let entries = self.construct_mapping(node)?;
                self.graph.dicts[dict as usize] = entries;
            }
            Task::Set(node) => {
                self.construct_mapping(node)?;
            }
            Task::Omap { node, list, pairs } => self.construct_omap(node, list, pairs)?,
        }
        Ok(())
    }

    fn construct_omap(&mut self, node: NodeId, list: u32, pairs: bool) -> Result<(), String> {
        let context = if pairs {
            "while constructing pairs"
        } else {
            "while constructing an ordered map"
        };
        let items = match self.kind(node) {
            NodeKind::Sequence(items) => items.clone(),
            other => {
                let problem = format!("expected a sequence, but found {}", other.id());
                return Err(self.error(Some((context, node)), &problem, node));
            }
        };
        for sub in items {
            let (key, value) = match self.kind(sub) {
                NodeKind::Mapping(p) if p.len() == 1 => p[0],
                NodeKind::Mapping(p) => {
                    let problem = format!(
                        "expected a single mapping item, but found {} items",
                        p.len()
                    );
                    return Err(self.error(Some((context, node)), &problem, sub));
                }
                other => {
                    let problem =
                        format!("expected a mapping of length 1, but found {}", other.id());
                    return Err(self.error(Some((context, node)), &problem, sub));
                }
            };
            let key = self.construct_object(key)?;
            let value = self.construct_object(value)?;
            self.graph.tuples.push((key, value));
            let tuple = u32::try_from(self.graph.tuples.len() - 1).unwrap_or(u32::MAX);
            self.graph.lists[list as usize].push(Obj::Tuple(tuple));
        }
        Ok(())
    }

    /// `SafeConstructor.construct_mapping`: merge keys flattened, Python dict semantics.
    fn construct_mapping(&mut self, node: NodeId) -> Result<Vec<(Obj, Obj)>, String> {
        if matches!(self.kind(node), NodeKind::Mapping(_)) {
            self.flatten_mapping(node, 0)?;
        }
        let len = match self.kind(node) {
            NodeKind::Mapping(pairs) => pairs.len(),
            other => {
                let problem = format!("expected a mapping node, but found {}", other.id());
                return Err(self.error(None, &problem, node));
            }
        };
        let mut dict = DictBuilder::with_capacity(len);
        for i in 0..len {
            let NodeKind::Mapping(pairs) = self.kind(node) else {
                break;
            };
            let (key_node, value_node) = pairs[i];
            let key = self.construct_object(key_node)?;
            if matches!(key, Obj::List(_) | Obj::Dict(_) | Obj::Set) {
                return Err(self.error(
                    Some(("while constructing a mapping", node)),
                    "found unhashable key",
                    key_node,
                ));
            }
            let value = self.construct_object(value_node)?;
            dict.insert(key, value, self.nodes, &self.graph.bigints);
        }
        Ok(dict.entries)
    }

    fn pairs(&self, node: NodeId) -> Vec<(NodeId, NodeId)> {
        match self.kind(node) {
            NodeKind::Mapping(p) => p.clone(),
            _ => Vec::new(),
        }
    }

    fn pairs_mut(&mut self, node: NodeId) -> Option<&mut Vec<(NodeId, NodeId)>> {
        match &mut self.nodes[node as usize].kind {
            NodeKind::Mapping(p) => Some(p),
            _ => None,
        }
    }

    /// `SafeConstructor.flatten_mapping`, including its in-place mutation of the node graph.
    fn flatten_mapping(&mut self, node: NodeId, depth: usize) -> Result<(), String> {
        if depth > PY_RECURSION_LIMIT {
            return Err(RECURSION_ERROR.to_owned());
        }
        let mut merge: Vec<(NodeId, NodeId)> = Vec::new();
        let mut index = 0;
        while let Some(&(key, value)) = self.pairs_mut(node).and_then(|p| p.get(index)) {
            match self.nodes[key as usize].tag {
                Tag::Merge => {
                    if let Some(p) = self.pairs_mut(node) {
                        p.remove(index);
                    }
                    match self.kind(value) {
                        NodeKind::Mapping(_) => {
                            self.flatten_mapping(value, depth + 1)?;
                            merge.extend(self.pairs(value));
                        }
                        NodeKind::Sequence(items) => {
                            let items = items.clone();
                            let mut submerge = Vec::with_capacity(items.len());
                            for sub in items {
                                if !matches!(self.kind(sub), NodeKind::Mapping(_)) {
                                    let problem = format!(
                                        "expected a mapping for merging, but found {}",
                                        self.kind(sub).id()
                                    );
                                    return Err(self.error(
                                        Some(("while constructing a mapping", node)),
                                        &problem,
                                        sub,
                                    ));
                                }
                                self.flatten_mapping(sub, depth + 1)?;
                                submerge.push(self.pairs(sub));
                            }
                            for pairs in submerge.into_iter().rev() {
                                merge.extend(pairs);
                            }
                        }
                        NodeKind::Scalar(_) => {
                            return Err(self.error(
                                Some(("while constructing a mapping", node)),
                                "expected a mapping or list of mappings for merging, but found scalar",
                                value,
                            ));
                        }
                    }
                }
                Tag::Value => {
                    self.nodes[key as usize].tag = Tag::Str;
                    index += 1;
                }
                _ => index += 1,
            }
        }
        if merge.is_empty() {
            return Ok(());
        }
        if let Some(p) = self.pairs_mut(node) {
            merge.append(p);
            *p = merge;
        }
        Ok(())
    }
}

/// How a dict key compares under Python's `__eq__`/`__hash__`.
#[derive(PartialEq)]
enum KeyRef<'a> {
    Str(&'a str),
    Int(i64),
    Big(BigInt),
    Float(u64),
    Nan(u32),
    None,
    /// Keys `json.dumps` rejects anyway; their equality never changes the outcome.
    Unique,
}

fn key_ref<'a>(obj: Obj, nodes: &'a [Node], bigints: &[BigInt]) -> KeyRef<'a> {
    match obj {
        Obj::Str(id) => match &nodes[id as usize].kind {
            NodeKind::Scalar(s) => KeyRef::Str(s),
            _ => KeyRef::Str(""),
        },
        Obj::Bool(b) => KeyRef::Int(i64::from(b)),
        Obj::Int(i) => KeyRef::Int(i),
        Obj::BigInt(id) => KeyRef::Big(bigints[id as usize].clone()),
        Obj::Float(f) if f.fract() == 0.0 => {
            // Integral floats equal the int of the same value (and -0.0 == 0).
            if (-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&f) {
                KeyRef::Int(f as i64)
            } else {
                KeyRef::Big(BigInt::from_integral_f64(f))
            }
        }
        Obj::Float(f) => KeyRef::Float(f.to_bits()),
        Obj::Nan(id) => KeyRef::Nan(id),
        Obj::None => KeyRef::None,
        _ => KeyRef::Unique,
    }
}

impl KeyRef<'_> {
    fn same(&self, other: &Self) -> bool {
        !matches!(self, KeyRef::Unique) && self == other
    }

    fn hash(&self) -> u64 {
        let mut h = Fx::default();
        match self {
            KeyRef::Str(s) => h.write(s.as_bytes()),
            KeyRef::Int(i) => h.write(&i.to_le_bytes()),
            KeyRef::Big(b) => h.write(b.to_decimal().as_bytes()),
            KeyRef::Float(bits) => h.write(&bits.to_le_bytes()),
            KeyRef::Nan(id) => h.write(&id.to_le_bytes()),
            KeyRef::None => h.write(b"None"),
            KeyRef::Unique => {}
        }
        h.0
    }
}

#[derive(Default)]
struct Fx(u64);

impl Fx {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(word))
                .wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
        self.0 ^= bytes.len() as u64;
    }
}

/// Small dicts are deduplicated by linear scan; large ones get a hash index.
const LINEAR_LIMIT: usize = 16;

struct DictBuilder {
    entries: Vec<(Obj, Obj)>,
    index: Option<HashMap<u64, Vec<u32>>>,
}

impl DictBuilder {
    fn with_capacity(n: usize) -> Self {
        Self {
            entries: Vec::with_capacity(n),
            index: None,
        }
    }

    /// `d[key] = value`: an equal key keeps its position and object, the value is replaced.
    fn insert(&mut self, key: Obj, value: Obj, nodes: &[Node], bigints: &[BigInt]) {
        let k = key_ref(key, nodes, bigints);
        if matches!(k, KeyRef::Unique) {
            self.entries.push((key, value));
            return;
        }
        if self.index.is_none() && self.entries.len() >= LINEAR_LIMIT {
            let mut index: HashMap<u64, Vec<u32>> = HashMap::new();
            for (i, (existing, _)) in self.entries.iter().enumerate() {
                let r = key_ref(*existing, nodes, bigints);
                if !matches!(r, KeyRef::Unique) {
                    index
                        .entry(r.hash())
                        .or_default()
                        .push(u32::try_from(i).unwrap_or(u32::MAX));
                }
            }
            self.index = Some(index);
        }
        let found = match &self.index {
            None => self
                .entries
                .iter()
                .position(|(existing, _)| key_ref(*existing, nodes, bigints).same(&k)),
            Some(index) => index.get(&k.hash()).and_then(|candidates| {
                candidates
                    .iter()
                    .map(|&i| i as usize)
                    .find(|&i| key_ref(self.entries[i].0, nodes, bigints).same(&k))
            }),
        };
        match found {
            Some(i) => self.entries[i].1 = value,
            None => {
                if let Some(index) = &mut self.index {
                    index
                        .entry(k.hash())
                        .or_default()
                        .push(u32::try_from(self.entries.len()).unwrap_or(u32::MAX));
                }
                self.entries.push((key, value));
            }
        }
    }
}
