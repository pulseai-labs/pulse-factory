//! Placeholders in workflow text.
//!
//! A placeholder is `{name}`, where `name` holds only `A-Z a-z 0-9 _ . -`. Any other brace text,
//! such as a JSON object or a `jq` program, is literal. The names are a closed set:
//! `unit.id`, `unit.vars.<k>`, `run.id`, `run.dir` and `param.<k>`.

use std::collections::BTreeMap;

/// One placeholder name, read from a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placeholder {
    /// `{unit.id}`
    UnitId,
    /// `{unit.vars.<k>}`
    UnitVar(String),
    /// `{run.id}`
    RunId,
    /// `{run.dir}`
    RunDir,
    /// `{param.<k>}`
    Param(String),
}

impl Placeholder {
    /// Read a placeholder name such as `unit.vars.branch`.
    pub fn parse(name: &str) -> Result<Placeholder, String> {
        let key = |rest: &str| -> Result<String, String> {
            if rest.is_empty() {
                Err(format!("placeholder `{name}` has no key"))
            } else {
                Ok(rest.to_string())
            }
        };
        match name {
            "unit.id" => Ok(Placeholder::UnitId),
            "run.id" => Ok(Placeholder::RunId),
            "run.dir" => Ok(Placeholder::RunDir),
            _ => {
                if let Some(rest) = name.strip_prefix("unit.vars.") {
                    key(rest).map(Placeholder::UnitVar)
                } else if let Some(rest) = name.strip_prefix("param.") {
                    key(rest).map(Placeholder::Param)
                } else {
                    Err(format!(
                        "unknown placeholder `{{{name}}}`; use unit.id, unit.vars.<k>, run.id, run.dir or param.<k>"
                    ))
                }
            }
        }
    }

    /// Does the value of this placeholder come from a unit record?
    pub fn is_from_record(&self) -> bool {
        matches!(self, Placeholder::UnitId | Placeholder::UnitVar(_))
    }

    /// The name as written between the braces.
    pub fn name(&self) -> String {
        match self {
            Placeholder::UnitId => "unit.id".to_string(),
            Placeholder::UnitVar(k) => format!("unit.vars.{k}"),
            Placeholder::RunId => "run.id".to_string(),
            Placeholder::RunDir => "run.dir".to_string(),
            Placeholder::Param(k) => format!("param.{k}"),
        }
    }
}

/// A piece of a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Hole(String),
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// Split a template into literal text and placeholder names.
pub fn split(template: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let name_len = after
            .find(|c: char| !is_name_char(c))
            .unwrap_or(after.len());
        let is_hole = name_len > 0 && after[name_len..].starts_with('}');
        if is_hole {
            text.push_str(&rest[..open]);
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            pieces.push(Piece::Hole(after[..name_len].to_string()));
            rest = &after[name_len + 1..];
        } else {
            text.push_str(&rest[..=open]);
            rest = after;
        }
    }
    text.push_str(rest);
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    pieces
}

/// The placeholder names in a template, in order. A name is not checked here.
pub fn placeholder_names(template: &str) -> Vec<String> {
    split(template)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Hole(name) => Some(name),
            Piece::Text(_) => None,
        })
        .collect()
}

// ------------------------------------------------------------------ rendering

/// The unit a template renders for.
#[derive(Clone, Copy, Debug)]
pub struct UnitView<'a> {
    pub id: &'a str,
    pub vars: &'a BTreeMap<String, String>,
}

/// What a template may use: the run, the parameters and, for a unit, its record.
#[derive(Clone, Copy, Debug)]
pub struct RenderContext<'a> {
    pub run_id: &'a str,
    pub run_dir: &'a str,
    pub params: &'a BTreeMap<String, String>,
    pub unit: Option<UnitView<'a>>,
}

/// Why a template cannot render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// The name is not a placeholder of the closed set.
    Unknown(String),
    /// A unit placeholder, and no unit.
    NeedsUnit(String),
    /// The unit's record has no such variable.
    MissingVariable(String),
    /// The run has no such parameter.
    MissingParam(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Unknown(why) => write!(f, "{why}"),
            RenderError::NeedsUnit(name) => {
                write!(f, "placeholder {{{name}}} needs a unit, and none is given")
            }
            RenderError::MissingVariable(name) => write!(
                f,
                "placeholder {{unit.vars.{name}}}: the unit record has no variable `{name}`"
            ),
            RenderError::MissingParam(name) => write!(
                f,
                "placeholder {{param.{name}}}: the run has no parameter `{name}`"
            ),
        }
    }
}

impl std::error::Error for RenderError {}

/// A rendered template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// True when a unit-record placeholder (`unit.id` or `unit.vars.<k>`) contributed to the text.
    pub from_record: bool,
}

/// Render a template. Each placeholder becomes its value as one piece of text.
pub fn render(template: &str, ctx: &RenderContext) -> Result<Rendered, RenderError> {
    let mut text = String::new();
    let mut from_record = false;
    for piece in split(template) {
        match piece {
            Piece::Text(t) => text.push_str(&t),
            Piece::Hole(name) => {
                let hole = Placeholder::parse(&name).map_err(RenderError::Unknown)?;
                from_record |= hole.is_from_record();
                match hole {
                    Placeholder::RunId => text.push_str(ctx.run_id),
                    Placeholder::RunDir => text.push_str(ctx.run_dir),
                    Placeholder::Param(k) => match ctx.params.get(&k) {
                        Some(v) => text.push_str(v),
                        None => return Err(RenderError::MissingParam(k)),
                    },
                    Placeholder::UnitId => match ctx.unit {
                        Some(u) => text.push_str(u.id),
                        None => return Err(RenderError::NeedsUnit(name)),
                    },
                    Placeholder::UnitVar(k) => match ctx.unit {
                        Some(u) => match u.vars.get(&k) {
                            Some(v) => text.push_str(v),
                            None => return Err(RenderError::MissingVariable(k)),
                        },
                        None => return Err(RenderError::NeedsUnit(name)),
                    },
                }
            }
        }
    }
    Ok(Rendered { text, from_record })
}
