//! PyYAML's C composer (`CParser.get_single_node`) plus tag resolution.

use std::collections::HashMap;

use super::libyaml::{Event, Parser};
use super::pyfmt::{Mark, marked_error};
use super::resolve::{Tag, resolve_scalar};

pub(super) type NodeId = u32;

pub(super) enum NodeKind {
    Scalar(String),
    Sequence(Vec<NodeId>),
    Mapping(Vec<(NodeId, NodeId)>),
}

impl NodeKind {
    /// `node.id` in PyYAML.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Scalar(_) => "scalar",
            Self::Sequence(_) => "sequence",
            Self::Mapping(_) => "mapping",
        }
    }
}

pub(super) struct Node {
    pub kind: NodeKind,
    pub tag: Tag,
    pub start: Mark,
}

pub(super) struct Composed {
    pub nodes: Vec<Node>,
    pub root: Option<NodeId>,
    /// Whether any alias occurred, i.e. whether nodes can be shared.
    pub has_aliases: bool,
}

enum Frame {
    Seq(NodeId),
    Map(NodeId, Option<NodeId>),
}

/// Composes the single document of the stream, failing like PyYAML on extra documents.
pub(super) fn compose(parser: &mut Parser<'_>, name: &str) -> Result<Composed, String> {
    let mut composed = Composed {
        nodes: Vec::new(),
        root: None,
        has_aliases: false,
    };
    parser.next()?; // STREAM-START
    let (event, _) = parser.next()?;
    if matches!(event, Event::StreamEnd) {
        return Ok(composed);
    }
    // DOCUMENT-START was just consumed.
    let root = compose_node_tree(parser, name, &mut composed)?;
    composed.root = Some(root);
    parser.next()?; // DOCUMENT-END
    let (event, mark) = parser.next()?;
    if !matches!(event, Event::StreamEnd) {
        return Err(marked_error(
            name,
            Some("expected a single document in the stream"),
            Some(composed.nodes[root as usize].start),
            Some("but found another document"),
            Some(mark),
        ));
    }
    Ok(composed)
}

fn compose_node_tree(
    parser: &mut Parser<'_>,
    name: &str,
    out: &mut Composed,
) -> Result<NodeId, String> {
    let mut anchors: HashMap<String, NodeId> = HashMap::new();
    let mut stack: Vec<Frame> = Vec::new();
    loop {
        let (event, mark) = parser.next()?;
        let finished = match event {
            Event::Alias { anchor } => match anchors.get(&anchor) {
                Some(&id) => {
                    out.has_aliases = true;
                    id
                }
                None => {
                    return Err(marked_error(
                        name,
                        None,
                        None,
                        Some("found undefined alias"),
                        Some(mark),
                    ));
                }
            },
            Event::Scalar {
                anchor,
                tag,
                value,
                plain_implicit,
            } => {
                check_anchor(&anchors, anchor.as_deref(), out, mark, name)?;
                let tag = match tag.as_deref() {
                    None | Some("!") => resolve_scalar(&value, plain_implicit),
                    Some(t) => Tag::from_explicit(t),
                };
                let id = push(out, NodeKind::Scalar(value), tag, mark);
                if let Some(anchor) = anchor {
                    anchors.insert(anchor, id);
                }
                id
            }
            Event::SequenceStart { anchor, tag } => {
                check_anchor(&anchors, anchor.as_deref(), out, mark, name)?;
                let tag = collection_tag(tag.as_deref(), Tag::Seq);
                let id = push(out, NodeKind::Sequence(Vec::new()), tag, mark);
                // Registered before the children, so an alias inside can refer to it.
                if let Some(anchor) = anchor {
                    anchors.insert(anchor, id);
                }
                stack.push(Frame::Seq(id));
                continue;
            }
            Event::MappingStart { anchor, tag } => {
                check_anchor(&anchors, anchor.as_deref(), out, mark, name)?;
                let tag = collection_tag(tag.as_deref(), Tag::Map);
                let id = push(out, NodeKind::Mapping(Vec::new()), tag, mark);
                if let Some(anchor) = anchor {
                    anchors.insert(anchor, id);
                }
                stack.push(Frame::Map(id, None));
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => match stack.pop() {
                Some(Frame::Seq(id) | Frame::Map(id, _)) => id,
                None => return Err(String::from("unbalanced YAML event stream")),
            },
            Event::StreamStart | Event::StreamEnd | Event::DocumentStart | Event::DocumentEnd => {
                return Err(String::from("unexpected YAML event"));
            }
        };
        match stack.last_mut() {
            None => return Ok(finished),
            Some(Frame::Seq(seq)) => {
                if let NodeKind::Sequence(items) = &mut out.nodes[*seq as usize].kind {
                    items.push(finished);
                }
            }
            Some(Frame::Map(map, pending)) => match pending.take() {
                None => *pending = Some(finished),
                Some(key) => {
                    if let NodeKind::Mapping(pairs) = &mut out.nodes[*map as usize].kind {
                        pairs.push((key, finished));
                    }
                }
            },
        }
    }
}

/// A missing or non-specific (`!`) collection tag resolves to the default tag.
fn collection_tag(tag: Option<&str>, default: Tag) -> Tag {
    match tag {
        None | Some("!") => default,
        Some(t) => Tag::from_explicit(t),
    }
}

fn push(out: &mut Composed, kind: NodeKind, tag: Tag, start: Mark) -> NodeId {
    let id = NodeId::try_from(out.nodes.len()).unwrap_or(NodeId::MAX);
    out.nodes.push(Node { kind, tag, start });
    id
}

fn check_anchor(
    anchors: &HashMap<String, NodeId>,
    anchor: Option<&str>,
    out: &Composed,
    mark: Mark,
    name: &str,
) -> Result<(), String> {
    match anchor.and_then(|a| anchors.get(a)) {
        Some(&first) => Err(marked_error(
            name,
            Some("found duplicate anchor; first occurrence"),
            Some(out.nodes[first as usize].start),
            Some("second occurrence"),
            Some(mark),
        )),
        None => Ok(()),
    }
}
