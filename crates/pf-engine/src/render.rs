//! Placeholders in workflow text.
//!
//! A placeholder is `{name}`, where `name` holds only `A-Z a-z 0-9 _ . -`. Any other brace text,
//! such as a JSON object or a `jq` program, is literal. The names are a closed set:
//! `unit.id`, `unit.vars.<k>`, `run.id`, `run.dir`, `run.package` and `param.<k>`, and, in an
//! `on_answer` command only, `answer.file`, `answer.request`, `answer.revision`,
//! `answer.generation` and `request.body_file`.
//!
//! A value goes into the text once. Nothing re-reads a rendered value for placeholders, so a value
//! that holds `{param.repo}` stays that literal text.

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
    /// `{run.package}`: the run's own copy of the workflow package, `<run dir>/package`.
    RunPackage,
    /// `{param.<k>}`
    Param(String),
    /// `{answer.*}` and `{request.body_file}`. Only an answer hook has them.
    Answer(AnswerField),
}

/// The placeholders that only an answer hook (`on_answer`) may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerField {
    /// `{answer.file}`: a file that holds the answer text.
    File,
    /// `{answer.request}`: the request ID, `R-<n>`.
    Request,
    /// `{answer.revision}`: the revision the answer was bound to.
    Revision,
    /// `{answer.generation}`: the claim generation that answered.
    Generation,
    /// `{request.body_file}`: a file that holds the request body as JSON.
    BodyFile,
}

impl AnswerField {
    /// The name as written between the braces.
    pub fn name(self) -> &'static str {
        match self {
            AnswerField::File => "answer.file",
            AnswerField::Request => "answer.request",
            AnswerField::Revision => "answer.revision",
            AnswerField::Generation => "answer.generation",
            AnswerField::BodyFile => "request.body_file",
        }
    }
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
            "run.package" => Ok(Placeholder::RunPackage),
            "answer.file" => Ok(Placeholder::Answer(AnswerField::File)),
            "answer.request" => Ok(Placeholder::Answer(AnswerField::Request)),
            "answer.revision" => Ok(Placeholder::Answer(AnswerField::Revision)),
            "answer.generation" => Ok(Placeholder::Answer(AnswerField::Generation)),
            "request.body_file" => Ok(Placeholder::Answer(AnswerField::BodyFile)),
            _ => {
                if let Some(rest) = name.strip_prefix("unit.vars.") {
                    key(rest).map(Placeholder::UnitVar)
                } else if let Some(rest) = name.strip_prefix("param.") {
                    key(rest).map(Placeholder::Param)
                } else {
                    Err(format!(
                        "unknown placeholder `{{{name}}}`; use unit.id, unit.vars.<k>, run.id, run.dir, run.package or param.<k> (an on_answer command also has answer.file, answer.request, answer.revision, answer.generation and request.body_file)"
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
            Placeholder::RunPackage => "run.package".to_string(),
            Placeholder::Param(k) => format!("param.{k}"),
            Placeholder::Answer(field) => field.name().to_string(),
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

/// What an answer hook's templates may use besides the run and the unit: the answer being applied.
/// The controller makes the two files and owns their paths.
#[derive(Clone, Copy, Debug)]
pub struct AnswerView<'a> {
    pub file: &'a str,
    pub request: &'a str,
    pub revision: u32,
    pub generation: u64,
    pub body_file: &'a str,
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
    /// An answer placeholder, and no answer is being applied.
    NeedsAnswer(String),
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
            RenderError::NeedsAnswer(name) => write!(
                f,
                "placeholder {{{name}}} is allowed only in an `on_answer` command"
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

/// Render a template. Each placeholder becomes its value as one piece of text. An answer
/// placeholder is an error here: use [`render_with`] in an answer hook.
pub fn render(template: &str, ctx: &RenderContext) -> Result<Rendered, RenderError> {
    render_with(template, ctx, None)
}

/// Render a template for an answer hook: `answer` gives the values of the answer placeholders.
pub fn render_with(
    template: &str,
    ctx: &RenderContext,
    answer: Option<&AnswerView>,
) -> Result<Rendered, RenderError> {
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
                    Placeholder::RunPackage => {
                        text.push_str(ctx.run_dir);
                        text.push_str("/package");
                    }
                    Placeholder::Answer(field) => match answer {
                        Some(a) => match field {
                            AnswerField::File => text.push_str(a.file),
                            AnswerField::Request => text.push_str(a.request),
                            AnswerField::Revision => text.push_str(&a.revision.to_string()),
                            AnswerField::Generation => text.push_str(&a.generation.to_string()),
                            AnswerField::BodyFile => text.push_str(a.body_file),
                        },
                        None => return Err(RenderError::NeedsAnswer(name)),
                    },
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
