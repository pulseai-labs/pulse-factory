//! The workflow file, version 1 (ADR-0004), documented in `docs/contracts/workflow-file.md`.
//!
//! The parser is strict. It refuses an unknown field, an unknown version, a duplicate key and a
//! missing required field. Each error carries the line and the column of the text it blames.

use std::collections::{BTreeMap, BTreeSet};

use pf_engine::MergeRule;
use pf_engine::capacity::Settings;
use pf_engine::render::{Placeholder, placeholder_names};

use crate::commands::{CommandSpec, DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS};
use crate::marked::{self, Entry, Node, ParseError, Pos, Value};

/// The only file version this parser reads.
pub const SUPPORTED_VERSION: u32 = 1;

/// A plugin the workflow's seats need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginReq {
    pub name: String,
    pub min_version: String,
}

/// One role: the agent runtime, the model and the rule file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleDef {
    pub name: String,
    /// A free name such as `omp`. The engine knows no runtime.
    pub runtime: String,
    pub model: String,
    /// The path of the role's rule file, relative to the workflow file.
    pub rules: String,
}

/// Where a track's units come in from: a queue directory that a seat fills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitSourceDef {
    pub dir: String,
    pub track: String,
    /// The variable names the records of this source carry.
    pub vars: Vec<String>,
}

/// A seed unit repeats while its track finishes and the barrier file stays absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepeatDef {
    pub when_units_done: String,
    pub until_file: String,
    pub max: u32,
}

/// A seed unit that starts later: it is admitted once, on the first tick at which `file` exists
/// and every unit of the track `units_done` is done (and at least one such unit exists).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartWhen {
    /// A template over the run. The seed waits until the rendered path exists.
    pub file: String,
    pub units_done: String,
}

/// A unit the file declares, admitted at the start of a run with no queue record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedDef {
    pub id: String,
    pub group: String,
    pub order: u32,
    pub track: String,
    pub vars: BTreeMap<String, String>,
    pub repeat: Option<RepeatDef>,
    /// `Some`: the seed is admitted when its condition holds, not at the start of the run. A seed
    /// carries `repeat` or `start_when`, never both.
    pub start_when: Option<StartWhen>,
}

/// The check a `pr` condition applies to its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrTest {
    /// The command prints a PR URL.
    Url,
}

/// One condition of a stage's `done` list. All of them must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoneCond {
    File(String),
    Exit0(CommandSpec),
    Pr { command: CommandSpec, test: PrTest },
}

/// One entry of a stage's `outcomes`. It is tried after the stage's session ends and its `done`
/// conditions hold. When every `when` condition holds, the unit goes to the stage `to` of the same
/// track. The entry sends the unit at most `max` times; one more is a park.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutcomeSpec {
    pub when: Vec<DoneCond>,
    pub to: String,
    pub max: u32,
}

/// A file that parks the unit, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkOn {
    pub file: String,
    pub reason: String,
    /// Commands that run when an answer resumes a unit parked by this file, before the unit
    /// becomes `running`. Empty declares no hook.
    pub on_answer: Vec<CommandSpec>,
}

/// One stage of a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    pub name: String,
    pub role: String,
    /// Brief files, relative to the workflow file.
    pub brief: Vec<String>,
    pub message: String,
    pub cwd: String,
    pub slots: Vec<String>,
    pub before: Vec<CommandSpec>,
    pub after_first_turn: Vec<CommandSpec>,
    pub stop_file: String,
    /// Commands that run when an answer resumes a unit parked by the stop file, before the unit
    /// becomes `running`. Empty declares no hook.
    pub stop_on_answer: Vec<CommandSpec>,
    pub stall_minutes: u32,
    pub first_turn_seconds: u32,
    pub skills: Vec<String>,
    /// `env.<NAME>` and the keys of [`STAGE_SETTING_KEYS`].
    pub settings: BTreeMap<String, String>,
    pub done: Vec<DoneCond>,
    pub park_on: Vec<ParkOn>,
    /// Tried in order after the stage is done. The first entry that holds sends the unit on. An
    /// empty list is a stage that goes on to the next one.
    pub outcomes: Vec<OutcomeSpec>,
    /// `entered_by: outcome`: the stage runs only when an outcome sends the unit to it. A unit
    /// that reaches it in order goes past it.
    pub entered_by_outcome: bool,
}

/// The merge definition of a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeSpec {
    pub rule: MergeRule,
    pub repo: String,
    pub base: String,
    /// The branch a unit's PR must come from. Only the `claim_ack` rule takes it.
    pub head_branch: Option<String>,
    /// Prints the unit's PR URL.
    pub pr: CommandSpec,
    pub head_check: bool,
    pub after_merge: Vec<CommandSpec>,
}

/// When a run may end. The run ends with `reason` once its seeds are finished and every unit is
/// done, and every `when` condition holds. Until then each tick notes `waiting`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndWhen {
    /// `file` and `exit0` conditions, in the order the file lists them.
    pub when: Vec<DoneCond>,
    /// A template over the run: the note a tick keeps while a condition does not hold.
    pub waiting: String,
    /// A template over the run: the reason the run ends with.
    pub reason: String,
}

/// A named list of stages, with an optional merge definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackDef {
    pub name: String,
    pub stages: Vec<StageSpec>,
    pub merge: Option<MergeSpec>,
}

/// A parsed workflow file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowFile {
    pub version: u32,
    pub name: String,
    pub params: Vec<String>,
    pub plugins: Vec<PluginReq>,
    pub roles: Vec<RoleDef>,
    pub settings: Settings,
    pub plan_check: Option<CommandSpec>,
    pub roots: Vec<String>,
    pub unit_sources: Vec<UnitSourceDef>,
    pub seeds: Vec<SeedDef>,
    pub tracks: Vec<TrackDef>,
    /// `None`: the run ends as soon as its seeds are finished and every unit is done.
    pub end_when: Option<EndWhen>,
}

impl WorkflowFile {
    /// Read a workflow file from YAML text.
    pub fn parse(text: &str) -> Result<WorkflowFile, ParseError> {
        let root = marked::parse(text)?;
        read_file(&root)
    }

    pub fn role(&self, name: &str) -> Option<&RoleDef> {
        self.roles.iter().find(|r| r.name == name)
    }

    pub fn track(&self, name: &str) -> Option<&TrackDef> {
        self.tracks.iter().find(|t| t.name == name)
    }

    /// The unit variable names that a track's sources and seeds declare.
    pub fn declared_vars(&self, track: &str) -> BTreeSet<String> {
        declared_vars(&self.unit_sources, &self.seeds)
            .remove(track)
            .unwrap_or_default()
    }
}

fn declared_vars(
    sources: &[UnitSourceDef],
    seeds: &[SeedDef],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in sources {
        out.entry(s.track.clone())
            .or_default()
            .extend(s.vars.iter().cloned());
    }
    for s in seeds {
        out.entry(s.track.clone())
            .or_default()
            .extend(s.vars.keys().cloned());
    }
    out
}

// ------------------------------------------------------------------ typed reading

/// The keys of one mapping, checked against the fields the format knows.
struct Fields<'a> {
    node: &'a Node,
    entries: &'a [Entry],
    what: String,
}

impl<'a> Fields<'a> {
    fn new(node: &'a Node, what: &str, known: &[&str]) -> Result<Fields<'a>, ParseError> {
        let Value::Map(entries) = &node.value else {
            return Err(ParseError::new(
                node.pos,
                format!("{what} must be a mapping, not {}", node.kind()),
            ));
        };
        for e in entries {
            if !known.contains(&e.key.as_str()) {
                return Err(ParseError::new(
                    e.key_pos,
                    format!(
                        "unknown field `{}` in {what}; the known fields are: {}",
                        e.key,
                        known.join(", ")
                    ),
                ));
            }
        }
        Ok(Fields {
            node,
            entries,
            what: what.to_string(),
        })
    }

    fn get(&self, key: &str) -> Option<&'a Node> {
        self.entries.iter().find(|e| e.key == key).map(|e| &e.value)
    }

    fn req(&self, key: &str) -> Result<&'a Node, ParseError> {
        self.get(key).ok_or_else(|| {
            ParseError::new(
                self.node.pos,
                format!("missing required field `{key}` in {}", self.what),
            )
        })
    }
}

fn text_of(node: &Node, what: &str) -> Result<String, ParseError> {
    match &node.value {
        Value::Scalar { text, .. } => Ok(text.clone()),
        _ => Err(ParseError::new(
            node.pos,
            format!("{what} must be text, not {}", node.kind()),
        )),
    }
}

fn nonempty_text(node: &Node, what: &str) -> Result<String, ParseError> {
    let text = text_of(node, what)?;
    if text.trim().is_empty() {
        return Err(ParseError::new(
            node.pos,
            format!("{what} must not be empty"),
        ));
    }
    Ok(text)
}

fn uint_of(node: &Node, what: &str) -> Result<u64, ParseError> {
    if let Value::Scalar { text, plain: true } = &node.value
        && let Ok(n) = text.parse::<u64>()
    {
        return Ok(n);
    }
    Err(ParseError::new(
        node.pos,
        format!("{what} must be a whole number"),
    ))
}

fn small_uint(node: &Node, what: &str, min: u64) -> Result<u32, ParseError> {
    let n = uint_of(node, what)?;
    if n < min {
        return Err(ParseError::new(
            node.pos,
            format!("{what} must be at least {min}"),
        ));
    }
    u32::try_from(n).map_err(|_| ParseError::new(node.pos, format!("{what} is too large")))
}

fn flag_of(node: &Node, what: &str) -> Result<bool, ParseError> {
    match &node.value {
        Value::Scalar { text, plain: true } if text == "true" => Ok(true),
        Value::Scalar { text, plain: true } if text == "false" => Ok(false),
        _ => Err(ParseError::new(
            node.pos,
            format!("{what} must be true or false"),
        )),
    }
}

fn items<'a>(node: &'a Node, what: &str) -> Result<&'a [Node], ParseError> {
    match &node.value {
        Value::Seq(items) => Ok(items),
        _ => Err(ParseError::new(
            node.pos,
            format!("{what} must be a list, not {}", node.kind()),
        )),
    }
}

fn text_list(node: &Node, what: &str) -> Result<Vec<String>, ParseError> {
    items(node, what)?
        .iter()
        .map(|n| nonempty_text(n, what))
        .collect()
}

fn text_map(node: &Node, what: &str) -> Result<BTreeMap<String, String>, ParseError> {
    let Value::Map(entries) = &node.value else {
        return Err(ParseError::new(
            node.pos,
            format!("{what} must be a mapping, not {}", node.kind()),
        ));
    };
    let mut out = BTreeMap::new();
    for e in entries {
        out.insert(e.key.clone(), text_of(&e.value, what)?);
    }
    Ok(out)
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn name_of(node: &Node, what: &str) -> Result<String, ParseError> {
    let text = text_of(node, what)?;
    if !is_name(&text) {
        return Err(ParseError::new(
            node.pos,
            format!("{what} `{text}` must use only A-Z a-z 0-9 . _ -"),
        ));
    }
    Ok(text)
}

/// A dotted version such as `1.26.0`: whole numbers joined by dots.
pub(crate) fn is_version(text: &str) -> bool {
    !text.is_empty()
        && text
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

// ------------------------------------------------------------------ placeholders

/// Where a template is used, and so which placeholders it may hold.
#[derive(Clone, Copy)]
enum Scope<'a> {
    /// A unit on this track exists: every placeholder is allowed, except the answer ones.
    Unit(&'a str),
    /// An answer hook of a unit on this track: the unit placeholders and the answer ones.
    Answer(&'a str),
    /// A run exists, a unit does not.
    Run,
    /// Only the parameters exist.
    Params,
}

struct Checker<'a> {
    params: &'a [String],
    vars: &'a BTreeMap<String, BTreeSet<String>>,
}

impl Checker<'_> {
    fn template(&self, text: &str, pos: Pos, scope: Scope) -> Result<(), ParseError> {
        // The track of a unit placeholder, when a unit exists here.
        let track = match scope {
            Scope::Unit(t) | Scope::Answer(t) => Some(t),
            Scope::Run | Scope::Params => None,
        };
        for name in placeholder_names(text) {
            let hole = Placeholder::parse(&name).map_err(|m| ParseError::new(pos, m))?;
            let shown = format!("{{{}}}", hole.name());
            match &hole {
                Placeholder::Param(k) => {
                    if !self.params.iter().any(|p| p == k) {
                        return Err(ParseError::new(
                            pos,
                            format!(
                                "placeholder `{shown}`: parameter `{k}` is not declared in `params`"
                            ),
                        ));
                    }
                }
                Placeholder::RunId | Placeholder::RunDir | Placeholder::RunPackage => {
                    if matches!(scope, Scope::Params) {
                        return Err(ParseError::new(
                            pos,
                            format!("placeholder `{shown}` needs a run, and no run exists here"),
                        ));
                    }
                }
                Placeholder::UnitId => {
                    if track.is_none() {
                        return Err(ParseError::new(
                            pos,
                            format!("placeholder `{shown}` needs a unit, and no unit exists here"),
                        ));
                    }
                }
                Placeholder::UnitVar(k) => {
                    let Some(track) = track else {
                        return Err(ParseError::new(
                            pos,
                            format!("placeholder `{shown}` needs a unit, and no unit exists here"),
                        ));
                    };
                    let declared = self.vars.get(track).is_some_and(|v| v.contains(k));
                    if !declared {
                        return Err(ParseError::new(
                            pos,
                            format!(
                                "placeholder `{shown}`: variable `{k}` is not declared by a unit source or seed of track `{track}`"
                            ),
                        ));
                    }
                }
                Placeholder::Answer(_) => {
                    if !matches!(scope, Scope::Answer(_)) {
                        return Err(ParseError::new(
                            pos,
                            format!(
                                "placeholder `{shown}` is allowed only in an `on_answer` command"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn text_node(&self, node: &Node, what: &str, scope: Scope) -> Result<String, ParseError> {
        let text = nonempty_text(node, what)?;
        self.template(&text, node.pos, scope)?;
        Ok(text)
    }

    /// A command: an argv list, or a mapping `{argv, timeout_seconds}`.
    fn command(&self, node: &Node, what: &str, scope: Scope) -> Result<CommandSpec, ParseError> {
        let (argv_node, timeout) = match &node.value {
            Value::Seq(_) => (node, DEFAULT_TIMEOUT_SECONDS),
            Value::Map(_) => {
                let f = Fields::new(node, what, &["argv", "timeout_seconds"])?;
                let timeout = match f.get("timeout_seconds") {
                    None => DEFAULT_TIMEOUT_SECONDS,
                    Some(t) => {
                        let n = uint_of(t, "`timeout_seconds`")?;
                        if n == 0 || n > MAX_TIMEOUT_SECONDS {
                            return Err(ParseError::new(
                                t.pos,
                                format!(
                                    "`timeout_seconds` must be from 1 to {MAX_TIMEOUT_SECONDS}"
                                ),
                            ));
                        }
                        n
                    }
                };
                (f.req("argv")?, timeout)
            }
            _ => {
                return Err(ParseError::new(
                    node.pos,
                    format!(
                        "{what} must be an argv list, such as [program, arg]; a shell string is not accepted"
                    ),
                ));
            }
        };
        let argv_items = items(argv_node, "an argv")?;
        if argv_items.is_empty() {
            return Err(ParseError::new(argv_node.pos, "an argv must not be empty"));
        }
        let mut argv = Vec::new();
        for (i, item) in argv_items.iter().enumerate() {
            let text = text_of(item, "an argv item")?;
            self.template(&text, item.pos, scope)?;
            if i == 0 {
                if text.is_empty() {
                    return Err(ParseError::new(
                        item.pos,
                        "the program of a command must not be empty",
                    ));
                }
                let from_record = placeholder_names(&text)
                    .iter()
                    .filter_map(|n| Placeholder::parse(n).ok())
                    .any(|p| p.is_from_record());
                if from_record {
                    return Err(ParseError::new(
                        item.pos,
                        "the program of a command must not come from a unit record",
                    ));
                }
            }
            argv.push(text);
        }
        Ok(CommandSpec {
            argv,
            timeout_seconds: timeout,
        })
    }

    fn commands(
        &self,
        node: &Node,
        what: &str,
        scope: Scope,
    ) -> Result<Vec<CommandSpec>, ParseError> {
        items(node, what)?
            .iter()
            .map(|n| self.command(n, "a command", scope))
            .collect()
    }
}

// ------------------------------------------------------------------ the file

const TOP_FIELDS: &[&str] = &[
    "version",
    "name",
    "params",
    "plugins",
    "roles",
    "settings",
    "plan_check",
    "roots",
    "seeds",
    "unit_sources",
    "tracks",
    "end_when",
];

fn read_file(root: &Node) -> Result<WorkflowFile, ParseError> {
    let Value::Map(entries) = &root.value else {
        return Err(ParseError::new(
            root.pos,
            "the workflow file must be a mapping",
        ));
    };
    // The version comes first: a file of a future version gets the version error.
    let version_entry = entries.iter().find(|e| e.key == "version").ok_or_else(|| {
        ParseError::new(
            root.pos,
            "missing required field `version` in the workflow file",
        )
    })?;
    let version = match &version_entry.value.value {
        Value::Scalar { text, plain: true } => text.parse::<u64>().ok(),
        _ => None,
    };
    if version != Some(u64::from(SUPPORTED_VERSION)) {
        let found = match &version_entry.value.value {
            Value::Scalar { text, .. } => text.clone(),
            _ => "(not text)".to_string(),
        };
        return Err(ParseError::new(
            version_entry.value.pos,
            format!("unsupported version {found}; the supported version is {SUPPORTED_VERSION}"),
        ));
    }

    let top = Fields::new(root, "the workflow file", TOP_FIELDS)?;
    let name = name_of(top.req("name")?, "the workflow name")?;
    let params = match top.get("params") {
        None => Vec::new(),
        Some(n) => {
            let list = text_list(n, "a parameter name")?;
            if let Some(bad) = list.iter().find(|p| !is_name(p)) {
                return Err(ParseError::new(
                    n.pos,
                    format!("parameter name `{bad}` must use only A-Z a-z 0-9 . _ -"),
                ));
            }
            list
        }
    };

    let plugins = match top.get("plugins") {
        None => Vec::new(),
        Some(n) => items(n, "`plugins`")?
            .iter()
            .map(read_plugin)
            .collect::<Result<_, _>>()?,
    };

    let roles = read_roles(top.req("roles")?)?;

    let settings = match top.get("settings") {
        None => Settings::default(),
        Some(n) => {
            let f = Fields::new(n, "`settings`", &["model_sessions", "cold_builds"])?;
            let defaults = Settings::default();
            Settings {
                model_sessions: match f.get("model_sessions") {
                    Some(v) => small_uint(v, "`model_sessions`", 1)?,
                    None => defaults.model_sessions,
                },
                cold_builds: match f.get("cold_builds") {
                    Some(v) => small_uint(v, "`cold_builds`", 1)?,
                    None => defaults.cold_builds,
                },
            }
        }
    };

    let check = Checker {
        params: &params,
        vars: &BTreeMap::new(),
    };
    let plan_check = match top.get("plan_check") {
        None => None,
        Some(n) => Some(check.command(n, "`plan_check`", Scope::Params)?),
    };
    let roots = match top.get("roots") {
        None => Vec::new(),
        Some(n) => items(n, "`roots`")?
            .iter()
            .map(|r| check.text_node(r, "a root", Scope::Run))
            .collect::<Result<_, _>>()?,
    };

    // Track references are checked once the tracks are known.
    let mut track_refs: Vec<(String, Pos, &'static str)> = Vec::new();
    let unit_sources = match top.get("unit_sources") {
        None => Vec::new(),
        Some(n) => items(n, "`unit_sources`")?
            .iter()
            .map(|s| read_source(s, &check, &mut track_refs))
            .collect::<Result<_, _>>()?,
    };
    let seeds = match top.get("seeds") {
        None => Vec::new(),
        Some(n) => {
            let seeds: Vec<SeedDef> = items(n, "`seeds`")?
                .iter()
                .map(|s| read_seed(s, &check, &mut track_refs))
                .collect::<Result<_, _>>()?;
            let mut seen = BTreeSet::new();
            for (seed, node) in seeds.iter().zip(items(n, "`seeds`")?) {
                if !seen.insert(seed.id.clone()) {
                    return Err(ParseError::new(
                        node.pos,
                        format!("seed id `{}` is used twice", seed.id),
                    ));
                }
            }
            seeds
        }
    };

    let vars = declared_vars(&unit_sources, &seeds);
    let check = Checker {
        params: &params,
        vars: &vars,
    };
    let tracks = read_tracks(top.req("tracks")?, &check, &roles)?;

    for (track, pos, what) in track_refs {
        if !tracks.iter().any(|t| t.name == track) {
            return Err(ParseError::new(
                pos,
                format!("{what} names track `{track}`, which the file does not define"),
            ));
        }
    }

    let end_when = match top.get("end_when") {
        None => None,
        Some(n) => Some(read_end_when(n, &check)?),
    };

    Ok(WorkflowFile {
        version: SUPPORTED_VERSION,
        name,
        params,
        plugins,
        roles,
        settings,
        plan_check,
        roots,
        unit_sources,
        seeds,
        tracks,
        end_when,
    })
}

fn read_plugin(node: &Node) -> Result<PluginReq, ParseError> {
    let f = Fields::new(node, "a plugin", &["name", "min_version"])?;
    let name = name_of(f.req("name")?, "the plugin name")?;
    let v = f.req("min_version")?;
    let min_version = text_of(v, "`min_version`")?;
    if !is_version(&min_version) {
        return Err(ParseError::new(
            v.pos,
            format!(
                "`min_version` `{min_version}` must be whole numbers joined by dots, such as 1.26.0"
            ),
        ));
    }
    Ok(PluginReq { name, min_version })
}

fn read_roles(node: &Node) -> Result<Vec<RoleDef>, ParseError> {
    let Value::Map(entries) = &node.value else {
        return Err(ParseError::new(
            node.pos,
            format!("`roles` must be a mapping, not {}", node.kind()),
        ));
    };
    if entries.is_empty() {
        return Err(ParseError::new(
            node.pos,
            "`roles` must hold at least one role",
        ));
    }
    let mut roles = Vec::new();
    for e in entries {
        if !is_name(&e.key) {
            return Err(ParseError::new(
                e.key_pos,
                format!("role name `{}` must use only A-Z a-z 0-9 . _ -", e.key),
            ));
        }
        let f = Fields::new(
            &e.value,
            &format!("role `{}`", e.key),
            &["runtime", "model", "rules"],
        )?;
        roles.push(RoleDef {
            name: e.key.clone(),
            runtime: name_of(f.req("runtime")?, "`runtime`")?,
            model: nonempty_text(f.req("model")?, "`model`")?,
            rules: nonempty_text(f.req("rules")?, "`rules`")?,
        });
    }
    Ok(roles)
}

fn read_source(
    node: &Node,
    check: &Checker,
    refs: &mut Vec<(String, Pos, &'static str)>,
) -> Result<UnitSourceDef, ParseError> {
    let f = Fields::new(node, "a unit source", &["dir", "track", "vars"])?;
    let dir = check.text_node(f.req("dir")?, "`dir`", Scope::Run)?;
    let track_node = f.req("track")?;
    let track = name_of(track_node, "`track`")?;
    refs.push((track.clone(), track_node.pos, "a unit source"));
    let vars = match f.get("vars") {
        None => Vec::new(),
        Some(v) => text_list(v, "a variable name")?,
    };
    Ok(UnitSourceDef { dir, track, vars })
}

fn read_seed(
    node: &Node,
    check: &Checker,
    refs: &mut Vec<(String, Pos, &'static str)>,
) -> Result<SeedDef, ParseError> {
    let f = Fields::new(
        node,
        "a seed",
        &[
            "id",
            "group",
            "order",
            "track",
            "vars",
            "repeat",
            "start_when",
        ],
    )?;
    let id = name_of(f.req("id")?, "the seed id")?;
    let group = nonempty_text(f.req("group")?, "`group`")?;
    let order = match f.get("order") {
        Some(o) => u32::try_from(uint_of(o, "`order`")?)
            .map_err(|_| ParseError::new(o.pos, "`order` is too large"))?,
        None => 0,
    };
    let track_node = f.req("track")?;
    let track = name_of(track_node, "`track`")?;
    refs.push((track.clone(), track_node.pos, "a seed"));
    let vars = match f.get("vars") {
        None => BTreeMap::new(),
        Some(v) => text_map(v, "a seed variable")?,
    };
    let repeat = match f.get("repeat") {
        None => None,
        Some(r) => {
            let rf = Fields::new(r, "`repeat`", &["when_units_done", "until_file", "max"])?;
            let when = rf.req("when_units_done")?;
            let when_units_done = name_of(when, "`when_units_done`")?;
            refs.push((when_units_done.clone(), when.pos, "`repeat`"));
            Some(RepeatDef {
                when_units_done,
                until_file: check.text_node(rf.req("until_file")?, "`until_file`", Scope::Run)?,
                max: small_uint(rf.req("max")?, "`max`", 1)?,
            })
        }
    };
    let start_when = match f.get("start_when") {
        None => None,
        Some(w) => {
            if repeat.is_some() {
                return Err(ParseError::new(
                    w.pos,
                    format!(
                        "seed `{id}` has both `start_when` and `repeat`; a seed has one or the other"
                    ),
                ));
            }
            let wf = Fields::new(w, "`start_when`", &["file", "units_done"])?;
            let done = wf.req("units_done")?;
            let units_done = name_of(done, "`units_done`")?;
            refs.push((units_done.clone(), done.pos, "`start_when`"));
            Some(StartWhen {
                file: check.text_node(wf.req("file")?, "`file`", Scope::Run)?,
                units_done,
            })
        }
    };
    Ok(SeedDef {
        id,
        group,
        order,
        track,
        vars,
        repeat,
        start_when,
    })
}

fn read_tracks(
    node: &Node,
    check: &Checker,
    roles: &[RoleDef],
) -> Result<Vec<TrackDef>, ParseError> {
    let Value::Map(entries) = &node.value else {
        return Err(ParseError::new(
            node.pos,
            format!("`tracks` must be a mapping, not {}", node.kind()),
        ));
    };
    if entries.is_empty() {
        return Err(ParseError::new(
            node.pos,
            "`tracks` must hold at least one track",
        ));
    }
    let mut tracks = Vec::new();
    for e in entries {
        if !is_name(&e.key) {
            return Err(ParseError::new(
                e.key_pos,
                format!("track name `{}` must use only A-Z a-z 0-9 . _ -", e.key),
            ));
        }
        let what = format!("track `{}`", e.key);
        let f = Fields::new(&e.value, &what, &["stages", "merge"])?;
        let stages_node = f.req("stages")?;
        let stage_nodes = items(stages_node, "`stages`")?;
        if stage_nodes.is_empty() {
            return Err(ParseError::new(
                stages_node.pos,
                format!("{what} must hold at least one stage"),
            ));
        }
        let scope = Scope::Unit(&e.key);
        let mut stages: Vec<StageSpec> = Vec::new();
        let mut targets: Vec<(String, String, Pos)> = Vec::new();
        for s in stage_nodes {
            let (stage, to_refs) = read_stage(s, check, scope, roles)?;
            if stages.iter().any(|x| x.name == stage.name) {
                return Err(ParseError::new(
                    s.pos,
                    format!("stage `{}` is defined twice in {what}", stage.name),
                ));
            }
            targets.extend(
                to_refs
                    .into_iter()
                    .map(|(to, pos)| (stage.name.clone(), to, pos)),
            );
            stages.push(stage);
        }
        // An outcome sends the unit to a stage of its own track, earlier or later.
        for (from, to, pos) in targets {
            if !stages.iter().any(|x| x.name == to) {
                return Err(ParseError::new(
                    pos,
                    format!(
                        "an outcome of stage `{from}` sends the unit to `{to}`, which {what} does not define"
                    ),
                ));
            }
        }
        let merge = match f.get("merge") {
            None => None,
            Some(m) => Some(read_merge(m, check, scope)?),
        };
        tracks.push(TrackDef {
            name: e.key.clone(),
            stages,
            merge,
        });
    }
    Ok(tracks)
}

const STAGE_FIELDS: &[&str] = &[
    "name",
    "role",
    "brief",
    "message",
    "cwd",
    "slots",
    "before",
    "after_first_turn",
    "stop_file",
    "stall_minutes",
    "first_turn_seconds",
    "skills",
    "settings",
    "done",
    "park_on",
    "outcomes",
    "entered_by",
];

/// A stage, and the `to` of each of its outcomes with the place it was written. The track checks
/// the targets once it has read every stage.
type OutcomeTargets = Vec<(String, Pos)>;
type StageRead = (StageSpec, OutcomeTargets);

fn read_stage(
    node: &Node,
    check: &Checker,
    scope: Scope,
    roles: &[RoleDef],
) -> Result<StageRead, ParseError> {
    let f = Fields::new(node, "a stage", STAGE_FIELDS)?;
    let name = name_of(f.req("name")?, "the stage name")?;
    let role_node = f.req("role")?;
    let role = name_of(role_node, "`role`")?;
    if !roles.iter().any(|r| r.name == role) {
        return Err(ParseError::new(
            role_node.pos,
            format!("stage `{name}` uses role `{role}`, which `roles` does not define"),
        ));
    }
    let brief_node = f.req("brief")?;
    let brief = match &brief_node.value {
        Value::Seq(_) => text_list(brief_node, "a brief path")?,
        _ => vec![nonempty_text(brief_node, "`brief`")?],
    };
    let message = check.text_node(f.req("message")?, "`message`", scope)?;
    let cwd = check.text_node(f.req("cwd")?, "`cwd`", scope)?;
    let slots = match f.get("slots") {
        None => vec!["session".to_string()],
        Some(n) => {
            let slots = text_list(n, "a slot")?;
            if let Some(bad) = slots
                .iter()
                .find(|s| !matches!(s.as_str(), "session" | "build"))
            {
                return Err(ParseError::new(
                    n.pos,
                    format!("slot `{bad}` is unknown; the slots are session and build"),
                ));
            }
            slots
        }
    };
    let before = match f.get("before") {
        None => Vec::new(),
        Some(n) => check.commands(n, "`before`", scope)?,
    };
    let after_first_turn = match f.get("after_first_turn") {
        None => Vec::new(),
        Some(n) => check.commands(n, "`after_first_turn`", scope)?,
    };
    // The stop file is a path, or a mapping `{file, on_answer}` that adds an answer hook.
    let stop_node = f.req("stop_file")?;
    let (stop_file, stop_on_answer) = match &stop_node.value {
        Value::Map(_) => {
            let sf = Fields::new(stop_node, "`stop_file`", &["file", "on_answer"])?;
            (
                check.text_node(sf.req("file")?, "`file`", scope)?,
                read_on_answer(&sf, check, scope)?,
            )
        }
        _ => (
            check.text_node(stop_node, "`stop_file`", scope)?,
            Vec::new(),
        ),
    };
    let stall_minutes = small_uint(f.req("stall_minutes")?, "`stall_minutes`", 1)?;
    let first_turn_seconds = small_uint(f.req("first_turn_seconds")?, "`first_turn_seconds`", 1)?;
    let skills = match f.get("skills") {
        None => Vec::new(),
        Some(n) => text_list(n, "a skill")?,
    };
    let settings = match f.get("settings") {
        None => BTreeMap::new(),
        Some(n) => read_stage_settings(n, check, scope)?,
    };
    let done_node = f.req("done")?;
    let done_items = items(done_node, "`done`")?;
    if done_items.is_empty() {
        return Err(ParseError::new(
            done_node.pos,
            format!("`done` of stage `{name}` must hold at least one condition"),
        ));
    }
    let done = done_items
        .iter()
        .map(|d| read_done(d, check, scope))
        .collect::<Result<_, _>>()?;
    let park_on = match f.get("park_on") {
        None => Vec::new(),
        Some(n) => items(n, "`park_on`")?
            .iter()
            .map(|p| {
                let pf = Fields::new(p, "a `park_on` entry", &["file", "reason", "on_answer"])?;
                Ok(ParkOn {
                    file: check.text_node(pf.req("file")?, "`file`", scope)?,
                    reason: check.text_node(pf.req("reason")?, "`reason`", scope)?,
                    on_answer: read_on_answer(&pf, check, scope)?,
                })
            })
            .collect::<Result<_, ParseError>>()?,
    };
    let (outcomes, targets) = match f.get("outcomes") {
        None => (Vec::new(), Vec::new()),
        Some(n) => read_outcomes(n, &name, check, scope)?,
    };
    let entered_by_outcome = match f.get("entered_by") {
        None => false,
        Some(n) => {
            let value = text_of(n, "`entered_by`")?;
            if value != "outcome" {
                return Err(ParseError::new(
                    n.pos,
                    format!("`entered_by` `{value}` is unknown; version 1 accepts: outcome"),
                ));
            }
            if outcomes.is_empty() {
                return Err(ParseError::new(
                    n.pos,
                    format!(
                        "stage `{name}` is `entered_by: outcome`, so it must hold at least one `outcomes` entry"
                    ),
                ));
            }
            true
        }
    };
    let spec = StageSpec {
        name,
        role,
        brief,
        message,
        cwd,
        slots,
        before,
        after_first_turn,
        stop_file,
        stop_on_answer,
        stall_minutes,
        first_turn_seconds,
        skills,
        settings,
        done,
        park_on,
        outcomes,
        entered_by_outcome,
    };
    Ok((spec, targets))
}

/// The `outcomes` list of a stage: each entry is `{when, to, max}`. `when` holds `done` entries
/// and all of them must hold. `max` is at least 1: an entry that may never fire has no use.
fn read_outcomes(
    node: &Node,
    stage: &str,
    check: &Checker,
    scope: Scope,
) -> Result<(Vec<OutcomeSpec>, OutcomeTargets), ParseError> {
    let list = items(node, "`outcomes`")?;
    if list.is_empty() {
        return Err(ParseError::new(
            node.pos,
            format!("`outcomes` of stage `{stage}` must hold at least one entry"),
        ));
    }
    let mut outcomes = Vec::new();
    let mut targets = Vec::new();
    for entry in list {
        let f = Fields::new(entry, "an outcome", &["when", "to", "max"])?;
        let when_node = f.req("when")?;
        let when_items = items(when_node, "`when`")?;
        if when_items.is_empty() {
            return Err(ParseError::new(
                when_node.pos,
                format!("`when` of an outcome of stage `{stage}` must hold at least one condition"),
            ));
        }
        let when = when_items
            .iter()
            .map(|d| read_done(d, check, scope))
            .collect::<Result<_, _>>()?;
        let to_node = f.req("to")?;
        let to = name_of(to_node, "`to`")?;
        let max = small_uint(f.req("max")?, "`max`", 1)?;
        targets.push((to.clone(), to_node.pos));
        outcomes.push(OutcomeSpec { when, to, max });
    }
    Ok((outcomes, targets))
}

/// The optional `on_answer` list of a `park_on` entry or a `stop_file`. Its commands may use the
/// answer placeholders, so they are checked in the answer scope.
fn read_on_answer(
    f: &Fields,
    check: &Checker,
    scope: Scope,
) -> Result<Vec<CommandSpec>, ParseError> {
    let Some(node) = f.get("on_answer") else {
        return Ok(Vec::new());
    };
    let Scope::Unit(track) = scope else {
        unreachable!("a stage is read in the scope of its track");
    };
    check.commands(node, "`on_answer`", Scope::Answer(track))
}

/// The keys other than `env.<NAME>` that a stage's `settings` admits. Each is a template that
/// `start_stage` renders into the launch plan (`docs/contracts/launch-settings.md`).
pub const STAGE_SETTING_KEYS: [&str; 6] = [
    "push_dir",
    "push_branch",
    "report_dir",
    "worktree",
    "own_branch",
    "decisions_file",
];

fn read_stage_settings(
    node: &Node,
    check: &Checker,
    scope: Scope,
) -> Result<BTreeMap<String, String>, ParseError> {
    let Value::Map(entries) = &node.value else {
        return Err(ParseError::new(
            node.pos,
            format!("`settings` must be a mapping, not {}", node.kind()),
        ));
    };
    let mut out = BTreeMap::new();
    for e in entries {
        let ok = match e.key.strip_prefix("env.") {
            Some(var) => {
                !var.is_empty()
                    && var
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            }
            None => STAGE_SETTING_KEYS.contains(&e.key.as_str()),
        };
        if !ok {
            return Err(ParseError::new(
                e.key_pos,
                format!(
                    "unknown field `{}` in a stage's `settings`; the known fields are: env.<NAME>, {}",
                    e.key,
                    STAGE_SETTING_KEYS.join(", ")
                ),
            ));
        }
        let value = text_of(&e.value, "a setting")?;
        check.template(&value, e.value.pos, scope)?;
        out.insert(e.key.clone(), value);
    }
    Ok(out)
}

fn read_done(node: &Node, check: &Checker, scope: Scope) -> Result<DoneCond, ParseError> {
    let f = Fields::new(node, "a `done` condition", &["file", "exit0", "pr"])?;
    let Value::Map(entries) = &node.value else {
        unreachable!("Fields::new checked the node is a mapping");
    };
    if entries.len() != 1 {
        return Err(ParseError::new(
            node.pos,
            "a `done` condition holds exactly one of `file`, `exit0` and `pr`",
        ));
    }
    if let Some(n) = f.get("file") {
        return Ok(DoneCond::File(check.text_node(n, "`file`", scope)?));
    }
    if let Some(n) = f.get("exit0") {
        return Ok(DoneCond::Exit0(check.command(n, "`exit0`", scope)?));
    }
    let pr = f.req("pr")?;
    let pf = Fields::new(pr, "`pr`", &["argv", "test"])?;
    let command = check.command(pf.req("argv")?, "`pr` argv", scope)?;
    let test_node = pf.req("test")?;
    let test = match text_of(test_node, "`test`")?.as_str() {
        "url" => PrTest::Url,
        other => {
            return Err(ParseError::new(
                test_node.pos,
                format!("`test` `{other}` is unknown; version 1 accepts: url"),
            ));
        }
    };
    Ok(DoneCond::Pr { command, test })
}

/// The `end_when` block: `when` lists `file` and `exit0` conditions, and `waiting` and `reason`
/// are templates over the run. No unit exists when the run ends, so none of them may name one.
fn read_end_when(node: &Node, check: &Checker) -> Result<EndWhen, ParseError> {
    let f = Fields::new(node, "`end_when`", &["when", "waiting", "reason"])?;
    let when_node = f.req("when")?;
    let entries = items(when_node, "`when`")?;
    if entries.is_empty() {
        return Err(ParseError::new(
            when_node.pos,
            "`when` of `end_when` must hold at least one condition",
        ));
    }
    let mut when = Vec::new();
    for entry in entries {
        let cond = read_done(entry, check, Scope::Run)?;
        if matches!(cond, DoneCond::Pr { .. }) {
            return Err(ParseError::new(
                entry.pos,
                "a `pr` condition cannot end a run; `end_when` accepts `file` and `exit0`",
            ));
        }
        when.push(cond);
    }
    Ok(EndWhen {
        when,
        waiting: check.text_node(f.req("waiting")?, "`waiting`", Scope::Run)?,
        reason: check.text_node(f.req("reason")?, "`reason`", Scope::Run)?,
    })
}

/// The merge rules version 1 accepts, as the file spells them.
const MERGE_RULES: &str = "green_checks_in_declared_order, claim_ack";

fn read_merge(node: &Node, check: &Checker, scope: Scope) -> Result<MergeSpec, ParseError> {
    let f = Fields::new(
        node,
        "`merge`",
        &[
            "rule",
            "repo",
            "base",
            "head_branch",
            "pr",
            "head_check",
            "after_merge",
        ],
    )?;
    let rule_node = f.req("rule")?;
    let rule = match text_of(rule_node, "`rule`")?.as_str() {
        "green_checks_in_declared_order" => MergeRule::GreenChecksInDeclaredOrder,
        "claim_ack" => MergeRule::ClaimAck,
        other => {
            return Err(ParseError::new(
                rule_node.pos,
                format!("merge rule `{other}` is unknown; version 1 accepts: {MERGE_RULES}"),
            ));
        }
    };
    let head_branch = match f.get("head_branch") {
        None => None,
        Some(n) => {
            if rule != MergeRule::ClaimAck {
                return Err(ParseError::new(
                    n.pos,
                    "`head_branch` belongs to the merge rule `claim_ack`",
                ));
            }
            Some(check.text_node(n, "`head_branch`", scope)?)
        }
    };
    Ok(MergeSpec {
        rule,
        repo: check.text_node(f.req("repo")?, "`repo`", scope)?,
        base: check.text_node(f.req("base")?, "`base`", scope)?,
        head_branch,
        pr: check.command(f.req("pr")?, "`pr`", scope)?,
        head_check: match f.get("head_check") {
            None => false,
            Some(h) => flag_of(h, "`head_check`")?,
        },
        after_merge: match f.get("after_merge") {
            None => Vec::new(),
            Some(n) => check.commands(n, "`after_merge`", scope)?,
        },
    })
}
