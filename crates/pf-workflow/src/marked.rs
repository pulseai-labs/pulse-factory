//! A YAML tree whose every node knows its line and column.
//!
//! `yaml-rust2` gives a position with each event. This module builds a small tree from the events.
//! It refuses what the strict workflow file does not use: anchors, aliases, tags, more than one
//! document and a repeated key in one mapping. Line and column both count from 1.

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// Why a file is refused, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The line, counting from 1.
    pub line: usize,
    /// The column, counting from 1.
    pub col: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl ParseError {
    pub fn new(pos: Pos, message: impl Into<String>) -> ParseError {
        ParseError {
            line: pos.line,
            col: pos.col,
            message: message.into(),
        }
    }
}

/// A place in the text. Both parts count from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    fn from_marker(m: Marker) -> Pos {
        // The parser counts columns from 0.
        Pos {
            line: m.line(),
            col: m.col() + 1,
        }
    }
}

/// One node of the tree.
#[derive(Clone, Debug)]
pub struct Node {
    pub pos: Pos,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub enum Value {
    /// An empty value, `~` or `null`.
    Null,
    Scalar {
        text: String,
        /// False for a quoted or block scalar. Only a plain scalar can be a number or a flag.
        plain: bool,
    },
    Seq(Vec<Node>),
    Map(Vec<Entry>),
}

/// One `key: value` pair of a mapping.
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub key_pos: Pos,
    pub value: Node,
}

impl Node {
    /// A short name of the node's kind, for error text.
    pub fn kind(&self) -> &'static str {
        match self.value {
            Value::Null => "an empty value",
            Value::Scalar { .. } => "a scalar",
            Value::Seq(_) => "a list",
            Value::Map(_) => "a mapping",
        }
    }
}

enum Open {
    Seq(Pos, Vec<Node>),
    Map {
        pos: Pos,
        entries: Vec<Entry>,
        key: Option<(String, Pos)>,
    },
}

#[derive(Default)]
struct Builder {
    stack: Vec<Open>,
    docs: Vec<Node>,
    error: Option<ParseError>,
}

impl Builder {
    fn fail(&mut self, pos: Pos, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(ParseError::new(pos, message));
        }
    }

    /// Put a finished node where it belongs.
    fn place(&mut self, mut node: Node) {
        let problem: Option<(Pos, String)> = match self.stack.last_mut() {
            None => {
                self.docs.push(node);
                None
            }
            Some(Open::Seq(_, items)) => {
                items.push(node);
                None
            }
            Some(Open::Map { entries, key, .. }) => match key.take() {
                None => match node.value {
                    // A scalar key arrives first.
                    Value::Scalar { text, .. } => {
                        *key = Some((text, node.pos));
                        None
                    }
                    Value::Null => {
                        *key = Some((String::new(), node.pos));
                        None
                    }
                    _ => Some((node.pos, "a mapping key must be plain text".to_string())),
                },
                Some((name, key_pos)) => {
                    if entries.iter().any(|e| e.key == name) {
                        Some((key_pos, format!("duplicate key `{name}`")))
                    } else {
                        if matches!(node.value, Value::Null) {
                            // The parser marks an empty value at the next token. Point at the key.
                            node.pos = key_pos;
                        }
                        entries.push(Entry {
                            key: name,
                            key_pos,
                            value: node,
                        });
                        None
                    }
                }
            },
        };
        if let Some((pos, message)) = problem {
            self.fail(pos, message);
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        if self.error.is_some() {
            return;
        }
        let pos = Pos::from_marker(mark);
        match ev {
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
            Event::DocumentStart => {
                if !self.docs.is_empty() {
                    self.fail(pos, "the workflow file must be one YAML document");
                }
            }
            Event::Alias(_) => self.fail(pos, "aliases are not supported"),
            Event::Scalar(text, style, anchor, tag) => {
                if anchor != 0 {
                    self.fail(pos, "anchors are not supported");
                } else if tag.is_some() {
                    self.fail(pos, "tags are not supported");
                } else {
                    let plain = style == TScalarStyle::Plain;
                    let value = if plain && (text.is_empty() || text == "~" || text == "null") {
                        Value::Null
                    } else {
                        Value::Scalar { text, plain }
                    };
                    self.place(Node { pos, value });
                }
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 {
                    self.fail(pos, "anchors are not supported");
                } else if tag.is_some() {
                    self.fail(pos, "tags are not supported");
                } else {
                    self.stack.push(Open::Seq(pos, Vec::new()));
                }
            }
            Event::SequenceEnd => {
                if let Some(Open::Seq(start, items)) = self.stack.pop() {
                    self.place(Node {
                        pos: start,
                        value: Value::Seq(items),
                    });
                }
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 {
                    self.fail(pos, "anchors are not supported");
                } else if tag.is_some() {
                    self.fail(pos, "tags are not supported");
                } else {
                    self.stack.push(Open::Map {
                        pos,
                        entries: Vec::new(),
                        key: None,
                    });
                }
            }
            Event::MappingEnd => {
                if let Some(Open::Map {
                    pos: start,
                    entries,
                    key,
                }) = self.stack.pop()
                {
                    if let Some((name, key_pos)) = key {
                        self.fail(key_pos, format!("key `{name}` has no value"));
                        return;
                    }
                    // The parser marks a block mapping at its first colon. The first key is a
                    // better place to point at.
                    let pos = entries.first().map_or(start, |e| e.key_pos);
                    self.place(Node {
                        pos,
                        value: Value::Map(entries),
                    });
                }
            }
        }
    }
}

/// Parse YAML text into a tree.
pub fn parse(text: &str) -> Result<Node, ParseError> {
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(text);
    let loaded = parser.load(&mut builder, true);
    if let Some(e) = builder.error {
        return Err(e);
    }
    if let Err(e) = loaded {
        return Err(ParseError::new(
            Pos::from_marker(*e.marker()),
            e.info().to_string(),
        ));
    }
    match builder.docs.len() {
        1 => Ok(builder.docs.remove(0)),
        0 => Err(ParseError::new(
            Pos { line: 1, col: 1 },
            "the file holds no YAML document",
        )),
        _ => Err(ParseError::new(
            Pos { line: 1, col: 1 },
            "the workflow file must be one YAML document",
        )),
    }
}
