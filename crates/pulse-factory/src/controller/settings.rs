//! Scoped settings: the registry, the resolver and the verbs `settings_get`, `settings_set` and
//! `settings_unset` (ADR-0001, ADR-0002, ADR-0003, ADR-0004, ADR-0016).
//!
//! One store table holds one value of one key at one scope: the server default, one project, one
//! agent, or one agent in one project. The most specific scope wins, and the binary's own default
//! comes last. Each key in the registry declares how it merges (`override` now), so a more specific
//! row masks the rest and `settings get` shows what it masked. A row changes only through an
//! admitted command with a command ID, from the CLI operator or a paired device. A chat or a seat is
//! refused, and the refusal is audited in coalesced lines, so a caller gets no unbounded durable
//! writes. The engine never reads a setting: the store port carries opaque keys and JSON values.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime};

use pf_api::types::{
    SCHEMA_VERSION, SettingApplies, SettingChanged, SettingLayer, SettingMerge, SettingRecord,
    SettingScope as WireScope, SettingScopeKind as WireKind, SettingValue, SettingsGetArgs,
    SettingsView,
};
use pf_engine::claim::Act;
use pf_engine::port::agent::SessionRef;
use pf_engine::port::chat::{DENY_COMMANDS_MAX, check_deny_commands};
use pf_engine::port::store::{Effect, SettingRow, SettingScope, SettingScopeKind, Store};
use pf_ipc::{Peer, Response};
use serde::Deserialize;
use serde_json::Value;

use super::chats::utc;
use super::commands::{Plan, check_command_id, field, now};
use super::seat::{hex, random_bytes};
use super::{Controller, lock};

/// The key of the cap on self-started turns.
pub const SELF_TURN_CAP: &str = "chat.self_turn_cap";

/// The key of the deny list of a chat's launch.
pub const DENY_COMMANDS: &str = "chat.deny_commands";

/// The key of the bound on `propose` builds, per chat session and minute (`r1.s4.w2`).
pub const PROPOSE_PER_MIN: &str = "chat.propose_per_min";

/// The bound's built-in value: four builds a minute. A legitimate Orchestrator proposes once and
/// re-proposes after a correction. Each build runs `plan_check`, `git` and maybe `gh`, so four a
/// minute leaves room for corrections and bounds a looping chat to 240 builds an hour.
pub const DEFAULT_PROPOSE_PER_MIN: u64 = 4;

/// The most the bound may be set to.
pub const PROPOSE_PER_MIN_MAX: u64 = 60;

/// The cap's built-in value: three self-started turns run whole.
pub const DEFAULT_SELF_TURN_CAP: u64 = 3;

/// The most the cap may be set to.
pub const SELF_TURN_CAP_MAX: u64 = 100;

/// The key of the replay buffer a session host keeps, in bytes (`r2.s2.w3`).
pub const SESSION_REPLAY_BYTES: &str = "session.replay_bytes";

/// The key of how long a host holds an exit nobody consumed, in minutes.
pub const SESSION_EXIT_HOLD_MINUTES: &str = "session.exit_hold_minutes";

/// The key of how long a host waits for its first attach, and the controller waits for a `hello`,
/// in milliseconds.
pub const SESSION_ATTACH_WAIT_MS: &str = "session.attach_wait_ms";

/// The key of how long the `hook` verb waits for a controller, in seconds.
pub const SESSION_HOOK_WAIT_SECONDS: &str = "session.hook_wait_seconds";

/// The four `session.*` keys, with their range and built-in value, in the order `settings get`
/// lists them: (key, lowest, highest, built-in).
pub const SESSION_KEYS: [(&str, u64, u64, u64); 4] = [
    (SESSION_REPLAY_BYTES, 65_536, 67_108_864, 4_194_304),
    (SESSION_EXIT_HOLD_MINUTES, 1, 10_080, 1_440),
    (SESSION_ATTACH_WAIT_MS, 100, 30_000, 3_000),
    (SESSION_HOOK_WAIT_SECONDS, 1, 120, 15),
];

/// The key that turns the app channel of the notices on (1) or off (0) (`r2.s11.w3`).
pub const NOTIFY_APP_ON: &str = "notify.app.on";

/// The key that turns the orchestrator channel of the notices on (1) or off (0).
pub const NOTIFY_ORCHESTRATOR_ON: &str = "notify.orchestrator.on";

/// The key of the most words in the text of a notice on the app channel.
pub const NOTIFY_APP_MAX_WORDS: &str = "notify.app.max_words";

/// The key of the most words in the text of a notice on the orchestrator channel.
pub const NOTIFY_ORCHESTRATOR_MAX_WORDS: &str = "notify.orchestrator.max_words";

/// The key of the wait before a notice is sent again, in seconds. It doubles with each send, up to
/// 900 seconds.
pub const NOTIFY_RESEND_SECONDS: &str = "notify.resend_seconds";

/// The five `notify.*` keys, with their range and built-in value, in the order `settings get` lists
/// them: (key, lowest, highest, built-in). The two `on` keys are 0 or 1.
pub const NOTIFY_KEYS: [(&str, u64, u64, u64); 5] = [
    (NOTIFY_APP_ON, 0, 1, 1),
    (NOTIFY_ORCHESTRATOR_ON, 0, 1, 1),
    (NOTIFY_APP_MAX_WORDS, 5, 200, 30),
    (NOTIFY_ORCHESTRATOR_MAX_WORDS, 10, 500, 100),
    (NOTIFY_RESEND_SECONDS, 10, 3600, 60),
];

/// The key of the acts of the clear grant that a project's Orchestrator may use (`r2.s3.w2`). Its
/// merge mode is `intersect`: every stored value on the chain cuts the built-in list of six acts. It
/// is set at a project, an agent, or a project and agent, never at the server, and `settle` reads it
/// fresh at each act.
pub const AUTHORITY_CLEAR_ACTS: &str = "authority.clear_acts";

/// The key of how many times `settle` may run a PR's failed checks again for one head SHA
/// (`r2.s3.w3`): a whole number from 0 to 5, built-in 1, merge mode `override`, set at the server, a
/// project, an agent, or a project and agent. `settle` reads it fresh at each `rerun_check`.
pub const AUTHORITY_RERUN_CHECKS_PER_HEAD: &str = "authority.rerun_checks_per_head";

/// The `authority.*` keys that hold a whole number, with their range and built-in value:
/// (key, lowest, highest, built-in).
pub const AUTHORITY_NUMBER_KEYS: [(&str, u64, u64, u64); 1] =
    [(AUTHORITY_RERUN_CHECKS_PER_HEAD, 0, 5, 1)];

/// The refusal of a write of [`AUTHORITY_CLEAR_ACTS`] at the server scope.
const CLEAR_ACTS_SCOPE: &str =
    "authority.clear_acts is set at a project, an agent, or a project and agent";

/// The key that turns the wake of a project's Orchestrator off or on (`r2.s3.w4`, ADR-0016's
/// amendment of 2026-11-24): a list of exactly one word, `["off"]` or `["below"]`. There is no `all`:
/// the relay is deferred.
pub const AUTOPILOT_WAKE: &str = "autopilot.wake";

/// The key that bounds the wakes of a project in an hour: a whole number, 0 to 60. `0` is off.
pub const AUTOPILOT_WAKES_PER_HOUR: &str = "autopilot.wakes_per_hour";

/// The built-in of [`AUTOPILOT_WAKES_PER_HOUR`].
pub const DEFAULT_WAKES_PER_HOUR: u64 = 6;

/// The top of the range of [`AUTOPILOT_WAKES_PER_HOUR`].
pub const WAKES_PER_HOUR_MAX: u64 = 60;

/// The one word of [`AUTOPILOT_WAKE`] that lets a halt below the line wake the Orchestrator.
pub const WAKE_BELOW: &str = "below";

/// The word of [`AUTOPILOT_WAKE`] that turns the wake off.
pub const WAKE_OFF: &str = "off";

/// What a resume does with a key's value (ADR-0016's amendment of 2026-11-16, which calls the flag
/// `applies`; `SettingApplies` of `pf-api` already names when a change takes effect, so the code
/// names this flag `at_resume` and leaves the wire type alone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtResume {
    /// A resume uses the value the launch record holds.
    Record,
    /// A resume reads the current value.
    Current,
}

impl AtResume {
    pub fn as_str(self) -> &'static str {
        match self {
            AtResume::Record => "record",
            AtResume::Current => "current",
        }
    }
}

/// The resume flag of a known key: `record` for the four `session.*` keys, `current` for the
/// three `chat.*` keys. An unknown key has none.
pub fn at_resume_of(key: &str) -> Option<AtResume> {
    if SESSION_KEYS.iter().any(|(k, ..)| *k == key) {
        Some(AtResume::Record)
    } else if KEYS.contains(&key) {
        Some(AtResume::Current)
    } else {
        None
    }
}

/// The built-in deny patterns, in order (ruling 9 of `r1.s4.w4`): the common spellings of a forced
/// push, a remote branch delete, a local branch delete, and a merge by `gh pr merge`. A deny rule
/// matches the command text the model writes: a speed bump, not a boundary (ADR-0014).
pub const BUILT_IN_DENY: [&str; 15] = [
    "git push -f*",
    "git push * -f*",
    "git push --force*",
    "git push * --force*",
    "git push * +*",
    "git push --delete",
    "git push * --delete",
    "git push -d",
    "git push * -d",
    "git branch -D",
    "git branch * -D",
    "gh pr merge",
    "gh -R * pr merge",
    "gh --repo * pr merge",
    "gh --repo=* pr merge",
];

/// The keys of this release, in the order `settings get` lists them.
const KEYS: [&str; 16] = [
    DENY_COMMANDS,
    SELF_TURN_CAP,
    PROPOSE_PER_MIN,
    SESSION_REPLAY_BYTES,
    SESSION_EXIT_HOLD_MINUTES,
    SESSION_ATTACH_WAIT_MS,
    SESSION_HOOK_WAIT_SECONDS,
    NOTIFY_APP_ON,
    NOTIFY_ORCHESTRATOR_ON,
    NOTIFY_APP_MAX_WORDS,
    NOTIFY_ORCHESTRATOR_MAX_WORDS,
    NOTIFY_RESEND_SECONDS,
    AUTHORITY_CLEAR_ACTS,
    // `r2.s3.w3`'s `authority.rerun_checks_per_head` follows `authority.clear_acts`, and the two
    // `autopilot.*` keys (`r2.s3.w4`) are last.
    AUTHORITY_RERUN_CHECKS_PER_HEAD,
    AUTOPILOT_WAKE,
    AUTOPILOT_WAKES_PER_HOUR,
];

/// The range and the built-in value of a `session.*`, a `notify.*` or an `authority.*` number key:
/// a whole number.
fn session_key(key: &str) -> Option<(u64, u64, u64)> {
    SESSION_KEYS
        .iter()
        .chain(NOTIFY_KEYS.iter())
        .chain(AUTHORITY_NUMBER_KEYS.iter())
        .find(|(k, ..)| *k == key)
        .map(|(_, lo, hi, d)| (*lo, *hi, *d))
}

/// The setting names that a refusal bucket can take. Any other name is bucket `other`.
const OTHER_BUCKET: &str = "other";

fn unknown_key(key: &str) -> String {
    format!(
        "unknown setting {key}; the settings are {}",
        KEYS.join(", ")
    )
}

/// When a change of `key` applies.
fn applies_of(key: &str) -> SettingApplies {
    // A session key reaches a host when it starts, so a change applies at the next launch.
    // A `notify.*` key reads `next_turn` too: a third wire value would break the UI's typecheck, and
    // `docs/contracts/cli.md` says that for these keys it means the next notice (`r2.s11.w3`).
    // `authority.clear_acts` reads `next_turn` too, and `docs/contracts/cli.md` says that for this key it
    // means the next `settle` (`r2.s3.w2`).
    // `authority.rerun_checks_per_head` reads `next_turn` too: for this key it means the next
    // `settle` (`r2.s3.w3`). The two `autopilot.*` keys read `next_turn` too, and for them it means
    // the next wake (`r2.s3.w4`).
    if key == SELF_TURN_CAP
        || key == PROPOSE_PER_MIN
        || key == AUTHORITY_CLEAR_ACTS
        || key == AUTHORITY_RERUN_CHECKS_PER_HEAD
        || key.starts_with("notify.")
        || key.starts_with("autopilot.")
    {
        SettingApplies::NextTurn
    } else {
        SettingApplies::NextLaunch
    }
}

/// The merge mode of `key`: `override` (the first row on the chain wins whole) for every key but
/// `authority.clear_acts`, which is `intersect`.
fn merge_of(key: &str) -> SettingMerge {
    if key == AUTHORITY_CLEAR_ACTS {
        SettingMerge::Intersect
    } else {
        SettingMerge::Override
    }
}

/// The names of the six acts, in the built-in order.
fn all_act_names() -> Vec<String> {
    Act::ALL.iter().map(|a| a.as_str().to_string()).collect()
}

/// The built-in value of a known key.
fn built_in(key: &str) -> SettingValue {
    if let Some((_, _, default)) = session_key(key) {
        SettingValue::Number(default)
    } else if key == SELF_TURN_CAP {
        SettingValue::Number(DEFAULT_SELF_TURN_CAP)
    } else if key == PROPOSE_PER_MIN {
        SettingValue::Number(DEFAULT_PROPOSE_PER_MIN)
    } else if key == AUTHORITY_CLEAR_ACTS {
        SettingValue::List(all_act_names())
    } else if key == AUTOPILOT_WAKE {
        SettingValue::List(vec![WAKE_BELOW.to_string()])
    } else if key == AUTOPILOT_WAKES_PER_HOUR {
        SettingValue::Number(DEFAULT_WAKES_PER_HOUR)
    } else {
        SettingValue::List(BUILT_IN_DENY.iter().map(ToString::to_string).collect())
    }
}

/// Check a value against the rule of its key, and give it back as a typed value. A value that
/// fails is refused with the rule.
fn check_value(key: &str, value: &Value) -> Result<SettingValue, String> {
    if let Some((lo, hi, _)) = session_key(key) {
        return match value.as_u64() {
            Some(n) if (lo..=hi).contains(&n) => Ok(SettingValue::Number(n)),
            _ => Err(format!(
                "{key} is a whole number from {lo} to {hi}, not {value}"
            )),
        };
    }
    match key {
        SELF_TURN_CAP => match value.as_u64() {
            Some(n) if n <= SELF_TURN_CAP_MAX => Ok(SettingValue::Number(n)),
            _ => Err(format!(
                "{SELF_TURN_CAP} is a whole number from 0 to {SELF_TURN_CAP_MAX} (0 is off), not {value}"
            )),
        },
        PROPOSE_PER_MIN => match value.as_u64() {
            Some(n) if n <= PROPOSE_PER_MIN_MAX => Ok(SettingValue::Number(n)),
            _ => Err(format!(
                "{PROPOSE_PER_MIN} is a whole number from 0 to {PROPOSE_PER_MIN_MAX} (0 is off), not {value}"
            )),
        },
        AUTOPILOT_WAKE => match value.as_array().map(Vec::as_slice) {
            Some([word]) if matches!(word.as_str(), Some(WAKE_OFF | WAKE_BELOW)) => {
                Ok(SettingValue::List(vec![
                    word.as_str().unwrap_or(WAKE_BELOW).to_string(),
                ]))
            }
            _ => Err(format!(
                "{AUTOPILOT_WAKE} is a list of one word, [\"{WAKE_OFF}\"] or [\"{WAKE_BELOW}\"], not {value}"
            )),
        },
        AUTOPILOT_WAKES_PER_HOUR => match value.as_u64() {
            Some(n) if n <= WAKES_PER_HOUR_MAX => Ok(SettingValue::Number(n)),
            _ => Err(format!(
                "{AUTOPILOT_WAKES_PER_HOUR} is a whole number from 0 to {WAKES_PER_HOUR_MAX} (0 is off), not {value}"
            )),
        },
        AUTHORITY_CLEAR_ACTS => {
            let names = all_act_names().join(", ");
            let Some(items) = value.as_array() else {
                return Err(format!(
                    "{AUTHORITY_CLEAR_ACTS} is a list of act names ({names}), not {value}"
                ));
            };
            let mut list = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str().and_then(Act::parse) {
                    Some(act) => list.push(act.as_str().to_string()),
                    None => {
                        return Err(format!(
                            "{AUTHORITY_CLEAR_ACTS} holds act names only ({names}), and {item} is not one"
                        ));
                    }
                }
            }
            Ok(SettingValue::List(list))
        }
        DENY_COMMANDS => {
            let Some(items) = value.as_array() else {
                return Err(format!(
                    "{DENY_COMMANDS} is a list of 0 to {DENY_COMMANDS_MAX} command patterns, not {value}"
                ));
            };
            let mut list = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err(format!(
                        "{DENY_COMMANDS} holds text patterns only, and {item} is not text"
                    ));
                };
                list.push(text.to_string());
            }
            check_deny_commands(&list)?;
            Ok(SettingValue::List(list))
        }
        other => Err(unknown_key(other)),
    }
}

/// A typed value as canonical JSON text.
fn value_json(v: &SettingValue) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn wire_scope(scope: &SettingScope) -> WireScope {
    WireScope {
        kind: match scope.kind() {
            SettingScopeKind::Server => WireKind::Server,
            SettingScopeKind::Project => WireKind::Project,
            SettingScopeKind::Agent => WireKind::Agent,
            SettingScopeKind::ProjectAgent => WireKind::ProjectAgent,
        },
        project_id: scope.project_id.clone(),
        agent_id: scope.agent_id.clone(),
    }
}

fn built_in_scope() -> WireScope {
    WireScope {
        kind: WireKind::BuiltIn,
        project_id: None,
        agent_id: None,
    }
}

/// What the resolver found for one key in one context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub value: SettingValue,
    /// The scope of the winning row, or `built_in`.
    pub from: WireScope,
    /// The text of `from`: a scope text, or `built_in`.
    pub from_text: String,
    /// Every readable stored row on the chain, most specific first.
    pub layers: Vec<SettingLayer>,
    /// The rows the resolved value hides.
    pub masked: Vec<SettingLayer>,
}

/// The four `session.*` values a host's launch resolved, each with the scope it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSettings {
    pub replay_bytes: (u64, String),
    pub exit_hold_minutes: (u64, String),
    pub attach_wait_ms: (u64, String),
    pub hook_wait_seconds: (u64, String),
}

impl SessionSettings {
    /// Take each `session.*` value of a launch record (JSON text) in place of the resolved one:
    /// the value and the scope it was recorded with. A key the record lacks keeps its value.
    pub fn take_recorded(&mut self, record: &str) {
        let Ok(body) = serde_json::from_str::<Value>(record) else {
            return;
        };
        let settings = &body["settings"];
        for (key, slot) in [
            (SESSION_REPLAY_BYTES, &mut self.replay_bytes),
            (SESSION_EXIT_HOLD_MINUTES, &mut self.exit_hold_minutes),
            (SESSION_ATTACH_WAIT_MS, &mut self.attach_wait_ms),
            (SESSION_HOOK_WAIT_SECONDS, &mut self.hook_wait_seconds),
        ] {
            if let (Some(value), Some(scope)) = (
                settings[key]["value"].as_u64(),
                settings[key]["scope"].as_str(),
            ) {
                *slot = (value, scope.to_string());
            }
        }
    }

    /// The record's `settings` entries: each key with its value and scope.
    pub fn as_record(&self) -> Vec<(&'static str, Value)> {
        [
            (SESSION_REPLAY_BYTES, &self.replay_bytes),
            (SESSION_EXIT_HOLD_MINUTES, &self.exit_hold_minutes),
            (SESSION_ATTACH_WAIT_MS, &self.attach_wait_ms),
            (SESSION_HOOK_WAIT_SECONDS, &self.hook_wait_seconds),
        ]
        .into_iter()
        .map(|(k, (v, scope))| (k, serde_json::json!({ "value": v, "scope": scope })))
        .collect()
    }
}

/// Who called a settings write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum By {
    /// The operator at the CLI, over the socket.
    Cli,
    /// A paired device, over the API.
    Device(String),
}

impl By {
    fn text(&self) -> String {
        match self {
            By::Cli => "cli".to_string(),
            By::Device(id) => id.clone(),
        }
    }
}

/// The arguments of a write, read leniently so that a bad value is a refusal with a reason and an
/// admission of its own, not a parse error with none.
#[derive(Deserialize)]
struct RawWrite {
    key: String,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    command_id: String,
}

/// The content of a settings admission, in canonical order. Who called is not content, so the same
/// command ID from the CLI and from a device with the same content replays. The kind of the verb
/// is content: a command ID is for one verb, so a set under the ID of an unset is a conflict. An
/// unset has no value, so a `value` in its arguments is not content.
#[derive(serde::Serialize)]
struct Content<'a> {
    agent_id: Option<&'a str>,
    key: &'a str,
    kind: &'a str,
    project_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a Value>,
}

fn content_of(kind: &str, raw: &RawWrite) -> String {
    serde_json::to_string(&Content {
        agent_id: raw.agent_id.as_deref(),
        key: &raw.key,
        kind,
        project_id: raw.project_id.as_deref(),
        value: if kind == "setting_set" {
            raw.value.as_ref()
        } else {
            None
        },
    })
    .unwrap_or_default()
}

// ------------------------------------------------------------------ the refusal windows

/// One open window of coalesced refusals, for one caller and one bucket.
struct Window {
    start: SystemTime,
    /// The refusals after the first one, which wrote its line at once.
    counted: u64,
    kind: &'static str,
    content: String,
    last_reason: String,
    /// True once the one stderr line of this window (a stopped ledger) was printed.
    noted: bool,
}

/// The windows of the refused callers, by (caller key, bucket). In memory: a crash or a restart
/// loses the count of an open window, which is at most one minute.
#[derive(Default)]
pub struct Refusals {
    windows: std::sync::Mutex<BTreeMap<(String, String), Window>>,
}

fn kind_of(verb: &str) -> &'static str {
    if verb == "settings_unset" {
        "setting_unset"
    } else {
        "setting_set"
    }
}

/// `<proc root>/<pid>/environ` holds `PF_SEAT_TOKEN`?
fn has_seat_token(environ: &[u8]) -> bool {
    environ
        .split(|b| *b == 0)
        .any(|entry| entry.starts_with(b"PF_SEAT_TOKEN="))
}

/// The parent of a process, from `<proc>/<pid>/stat` (cut after the last `)`).
fn parent_of(proc_root: &Path, pid: u32) -> Option<u32> {
    let line = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let after = &line[line.rfind(')')? + 1..];
    after.split_whitespace().nth(1)?.parse().ok()
}

/// The most parents the walk follows.
const MAX_ANCESTORS: usize = 64;

impl Controller {
    // -------------------------------------------------------------- the resolver

    /// Whether a project is live, or the reason it is not.
    fn check_project(&self, id: &str) -> Result<(), String> {
        match self.store.project(id).map_err(|e| e.to_string())? {
            None => Err(format!("no project {id}")),
            Some(p) if p.removed_at.is_some() => Err(format!("project {id} is removed")),
            Some(_) => Ok(()),
        }
    }

    fn check_agent(&self, id: &str) -> Result<(), String> {
        match self.store.agent(id).map_err(|e| e.to_string())? {
            None => Err(format!("no agent {id}")),
            Some(_) => Ok(()),
        }
    }

    /// Resolve a key for a context: the first readable row of the scope chain wins whole
    /// (`override`), and the others are masked. With no row, the built-in. There is no cache, so a
    /// change applies at the next read. A stored row that this binary cannot read (a hand edit) is
    /// skipped with one stderr line, and the next scope decides. A stored key this binary does not
    /// know is never asked for here.
    pub fn resolve_setting(
        &self,
        key: &str,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Resolved, String> {
        self.resolve_inner(key, project, agent, false)
    }

    /// [`Controller::resolve_setting`] for a read that must fail closed (the wake, `r2.s3.w4`): a
    /// stored row that this binary cannot read is an error, where the plain read skips it and lets
    /// the next scope or the built-in decide. A wake is spend, so a value it cannot read gives no
    /// wake and never the built-in.
    pub(super) fn resolve_setting_strict(
        &self,
        key: &str,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Resolved, String> {
        self.resolve_inner(key, project, agent, true)
    }

    fn resolve_inner(
        &self,
        key: &str,
        project: Option<&str>,
        agent: Option<&str>,
        strict: bool,
    ) -> Result<Resolved, String> {
        if !KEYS.contains(&key) {
            return Err(unknown_key(key));
        }
        let rows = self
            .store
            .setting_chain(key, project, agent)
            .map_err(|e| e.to_string())?;
        let mut layers = Vec::new();
        for row in rows {
            let typed = serde_json::from_str::<Value>(&row.value)
                .map_err(|e| e.to_string())
                .and_then(|v| check_value(key, &v));
            match typed {
                Ok(value) => layers.push(SettingLayer {
                    scope: wire_scope(&row.scope),
                    value,
                    set_by: row.set_by,
                    set_at: row.set_at,
                }),
                Err(why) if strict => {
                    return Err(format!(
                        "the stored value of {key} at {} cannot be read: {why}",
                        row.scope.text()
                    ));
                }
                Err(why) => eprintln!(
                    "pulse-factory: the stored value of {key} at {} is skipped: {why}",
                    row.scope.text()
                ),
            }
        }
        if merge_of(key) == SettingMerge::Intersect {
            return Ok(resolve_intersect(key, layers));
        }
        match layers.split_first() {
            Some((winner, rest)) => Ok(Resolved {
                value: winner.value.clone(),
                from: winner.scope.clone(),
                from_text: scope_text(&winner.scope),
                masked: rest.to_vec(),
                layers,
            }),
            None => Ok(Resolved {
                value: built_in(key),
                from: built_in_scope(),
                from_text: "built_in".to_string(),
                layers: Vec::new(),
                masked: Vec::new(),
            }),
        }
    }

    /// The acts that `authority.clear_acts` leaves for a project and an agent, and the scope the
    /// value came from. A read that fails refuses the act, so the error comes back.
    pub(super) fn resolved_clear_acts(
        &self,
        project: &str,
        agent: &str,
    ) -> Result<(Vec<String>, String), String> {
        Self::clear_acts_of(self.resolve_setting(
            AUTHORITY_CLEAR_ACTS,
            Some(project),
            Some(agent),
        )?)
    }

    /// [`Controller::resolved_clear_acts`] for the wake (`r2.s3.w4`): a stored row that cannot be
    /// read is an error, where the plain read skips it and lets a wider scope decide. A wake that
    /// names acts the narrowing row may have cut gives no wake instead.
    pub(super) fn resolved_clear_acts_strict(
        &self,
        project: &str,
        agent: &str,
    ) -> Result<(Vec<String>, String), String> {
        Self::clear_acts_of(self.resolve_setting_strict(
            AUTHORITY_CLEAR_ACTS,
            Some(project),
            Some(agent),
        )?)
    }

    fn clear_acts_of(resolved: Resolved) -> Result<(Vec<String>, String), String> {
        match resolved {
            Resolved {
                value: SettingValue::List(list),
                from_text,
                ..
            } => Ok((list, from_text)),
            _ => Err(format!("{AUTHORITY_CLEAR_ACTS} resolved to no list")),
        }
    }

    /// The number of times `settle` may run the failed checks of a PR again for one head SHA, for a
    /// project and an agent, read fresh. A read that fails refuses the act, so the error comes back.
    pub(super) fn resolved_rerun_checks_per_head(
        &self,
        project: &str,
        agent: &str,
    ) -> Result<u32, String> {
        match self.resolve_setting(AUTHORITY_RERUN_CHECKS_PER_HEAD, Some(project), Some(agent))? {
            Resolved {
                value: SettingValue::Number(n),
                ..
            } => u32::try_from(n)
                .map_err(|_| format!("{AUTHORITY_RERUN_CHECKS_PER_HEAD} resolved to {n}")),
            _ => Err(format!(
                "{AUTHORITY_RERUN_CHECKS_PER_HEAD} resolved to no number"
            )),
        }
    }

    /// The resolved cap for a context, as a number. A resolver failure is the built-in, with one
    /// stderr line: a chat is never held by a settings read that fails.
    pub(super) fn self_turn_cap(&self, project: &str, agent: &str) -> (u64, String) {
        match self.resolve_setting(SELF_TURN_CAP, Some(project), Some(agent)) {
            Ok(Resolved {
                value: SettingValue::Number(n),
                from_text,
                ..
            }) => (n, from_text),
            Ok(_) => (DEFAULT_SELF_TURN_CAP, "built_in".to_string()),
            Err(why) => {
                eprintln!("pulse-factory: {SELF_TURN_CAP} is not read: {why}");
                (DEFAULT_SELF_TURN_CAP, "built_in".to_string())
            }
        }
    }

    /// The resolved `chat.propose_per_min` for a context, and the scope it came from. A resolver
    /// failure is the built-in, with one stderr line.
    pub(super) fn propose_per_min(&self, project: &str, agent: &str) -> (u64, String) {
        match self.resolve_setting(PROPOSE_PER_MIN, Some(project), Some(agent)) {
            Ok(Resolved {
                value: SettingValue::Number(n),
                from_text,
                ..
            }) => (n, from_text),
            Ok(_) => (DEFAULT_PROPOSE_PER_MIN, "built_in".to_string()),
            Err(why) => {
                eprintln!("pulse-factory: {PROPOSE_PER_MIN} is not read: {why}");
                (DEFAULT_PROPOSE_PER_MIN, "built_in".to_string())
            }
        }
    }

    /// The resolved value of a `notify.*` key for a context. A resolver failure is the built-in, with
    /// one stderr line: a notice is never held back by a settings read that fails.
    pub(super) fn notify_setting(
        &self,
        key: &str,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> u64 {
        let default = NOTIFY_KEYS
            .iter()
            .find(|(k, ..)| *k == key)
            .map_or(0, |(_, _, _, d)| *d);
        match self.resolve_setting(key, project, agent) {
            Ok(Resolved {
                value: SettingValue::Number(n),
                ..
            }) => n,
            Ok(_) => default,
            Err(why) => {
                eprintln!("pulse-factory: {key} is not read: {why}");
                default
            }
        }
    }

    /// [`Controller::notify_setting`] for the wake (`r2.s3.w4`): a stored row that cannot be read
    /// is an error, and never the built-in.
    pub(super) fn notify_setting_strict(
        &self,
        key: &str,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Result<u64, String> {
        match self.resolve_setting_strict(key, project, agent)?.value {
            SettingValue::Number(n) => Ok(n),
            SettingValue::List(_) => Err(format!("{key} resolved to a list, not a number")),
        }
    }

    /// The resolved deny list for a context, and the scope it came from.
    pub(super) fn deny_commands(&self, project: &str, agent: &str) -> (Vec<String>, String) {
        match self.resolve_setting(DENY_COMMANDS, Some(project), Some(agent)) {
            Ok(Resolved {
                value: SettingValue::List(list),
                from_text,
                ..
            }) => (list, from_text),
            Ok(_) => (
                BUILT_IN_DENY.iter().map(ToString::to_string).collect(),
                "built_in".to_string(),
            ),
            Err(why) => {
                eprintln!("pulse-factory: {DENY_COMMANDS} is not read: {why}");
                (
                    BUILT_IN_DENY.iter().map(ToString::to_string).collect(),
                    "built_in".to_string(),
                )
            }
        }
    }

    /// The four `session.*` keys resolved for a project: each with its value and the scope it came
    /// from. A seat has no agent, so the chain is the project's, then the server's. A resolver
    /// failure is the built-in, with one stderr line: a launch is never held by a settings read.
    pub(super) fn session_settings(&self, project: Option<&str>) -> SessionSettings {
        let one = |key: &str, default: u64| -> (u64, String) {
            match self.resolve_setting(key, project, None) {
                Ok(Resolved {
                    value: SettingValue::Number(n),
                    from_text,
                    ..
                }) => (n, from_text),
                Ok(_) => (default, "built_in".to_string()),
                Err(why) => {
                    eprintln!("pulse-factory: {key} is not read: {why}");
                    (default, "built_in".to_string())
                }
            }
        };
        let [replay, hold, wait, hook] = SESSION_KEYS.map(|(k, _, _, d)| one(k, d));
        SessionSettings {
            replay_bytes: replay,
            exit_hold_minutes: hold,
            attach_wait_ms: wait,
            hook_wait_seconds: hook,
        }
    }

    fn record(
        &self,
        key: &str,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Result<SettingRecord, String> {
        let r = self.resolve_setting(key, project, agent)?;
        Ok(SettingRecord {
            key: key.to_string(),
            value: r.value,
            from: r.from,
            built_in: built_in(key),
            merge: merge_of(key),
            applies: applies_of(key),
            layers: r.layers,
            masked: r.masked,
        })
    }

    // -------------------------------------------------------------- settings_get

    pub(super) fn verb_settings_get(&self, args: &Value) -> Response {
        let args: SettingsGetArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
        };
        let build = || -> Result<SettingsView, String> {
            if let Some(p) = &args.project_id {
                self.check_project(p)?;
            }
            if let Some(a) = &args.agent_id {
                self.check_agent(a)?;
            }
            let keys: Vec<&str> = match &args.key {
                Some(k) if KEYS.contains(&k.as_str()) => vec![k.as_str()],
                Some(k) => return Err(unknown_key(k)),
                None => KEYS.to_vec(),
            };
            let mut settings = Vec::new();
            for key in keys {
                settings.push(self.record(
                    key,
                    args.project_id.as_deref(),
                    args.agent_id.as_deref(),
                )?);
            }
            Ok(SettingsView {
                schema_version: SCHEMA_VERSION,
                project_id: args.project_id.clone(),
                agent_id: args.agent_id.clone(),
                settings,
            })
        };
        match build() {
            Ok(view) => Response::ok(serde_json::to_value(view).unwrap_or(Value::Null)),
            Err(why) => Response::err(why),
        }
    }

    // -------------------------------------------------------------- settings_set and settings_unset

    /// The write of a setting, for the socket (`By::Cli`, after the caller check) and for the API
    /// bridge (`By::Device`). `verb` is `settings_set` or `settings_unset`.
    pub(super) fn settings_write(&self, verb: &str, args: &Value, by: By) -> Response {
        let set = verb == "settings_set";
        let raw: RawWrite = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
        };
        if let Err(why) = check_command_id(&raw.command_id) {
            return Response::err(why);
        }
        let scope = SettingScope::new(raw.project_id.as_deref(), raw.agent_id.as_deref());
        let kind = kind_of(verb);
        let content = content_of(kind, &raw);
        let plan = match self.plan_write(set, &raw, &scope, &by) {
            Ok(plan) => plan,
            Err(why) => Plan::Refuse(why),
        };
        let value = plan_value(set, &raw);
        match self.run_command(kind, &raw.command_id, content, plan) {
            Ok((_, replayed)) => self.setting_changed(&raw.key, &scope, value, replayed),
            Err(why) => Response::err(why),
        }
    }

    fn plan_write(
        &self,
        set: bool,
        raw: &RawWrite,
        scope: &SettingScope,
        by: &By,
    ) -> Result<Plan, String> {
        if !KEYS.contains(&raw.key.as_str()) {
            return Err(unknown_key(&raw.key));
        }
        let typed = if set {
            let Some(value) = &raw.value else {
                return Err("settings set needs a value".to_string());
            };
            Some(check_value(&raw.key, value)?)
        } else {
            None
        };
        if let Some(p) = &scope.project_id {
            self.check_project(p)?;
        }
        if let Some(a) = &scope.agent_id {
            self.check_agent(a)?;
        }
        if raw.key == AUTHORITY_CLEAR_ACTS && scope.kind() == SettingScopeKind::Server {
            return Err(CLEAR_ACTS_SCOPE.to_string());
        }
        let subject = format!("{}@{}", raw.key, scope.text());
        let who = by.text();
        match typed {
            Some(value) => {
                let json = value_json(&value);
                Ok(Plan::Admit {
                    effects: vec![Effect::SetSetting(SettingRow {
                        scope: scope.clone(),
                        key: raw.key.clone(),
                        value: json.clone(),
                        set_by: who.clone(),
                        set_at: now(),
                    })],
                    subject,
                    line: format!(
                        "setting_set {} admitted scope={} value={} by={who} command={}",
                        raw.key,
                        scope.text(),
                        field(&json),
                        raw.command_id
                    ),
                })
            }
            None => Ok(Plan::Admit {
                effects: vec![Effect::UnsetSetting {
                    scope: scope.clone(),
                    key: raw.key.clone(),
                }],
                subject,
                line: format!(
                    "setting_unset {} admitted scope={} by={who} command={}",
                    raw.key,
                    scope.text(),
                    raw.command_id
                ),
            }),
        }
    }

    /// The answer of an admitted (or replayed) write.
    fn setting_changed(
        &self,
        key: &str,
        scope: &SettingScope,
        value: Option<SettingValue>,
        replayed: bool,
    ) -> Response {
        match self.record(key, scope.project_id.as_deref(), scope.agent_id.as_deref()) {
            Ok(resolved) => Response::ok(
                serde_json::to_value(SettingChanged {
                    schema_version: SCHEMA_VERSION,
                    replayed,
                    key: key.to_string(),
                    scope: wire_scope(scope),
                    value,
                    resolved,
                })
                .unwrap_or(Value::Null),
            ),
            Err(why) => Response::err(why),
        }
    }

    // -------------------------------------------------------------- the caller check

    /// The write over the socket: refused for a caller that is, or descends from, a seat or a live
    /// chat process, and audited (rulings 6 and 8). Anything else is the operator.
    pub(super) fn settings_write_from(&self, verb: &str, args: &Value, peer: Peer) -> Response {
        match self.check_chat_caller(verb, peer) {
            Ok(()) => self.settings_write(verb, args, By::Cli),
            Err(why) => {
                let caller = self.settings_caller_key(peer);
                self.refuse_settings_caller(verb, args, &caller, &why);
                Response::err(why)
            }
        }
    }

    /// The seat processes the controller launched and still tracks.
    fn seat_pids(&self) -> HashSet<u32> {
        let mut pids = HashSet::new();
        for handle in self.handles() {
            let Ok(sessions) = self.store.sessions(&handle.info.run_id) else {
                continue;
            };
            for session in sessions.iter().filter(|s| !s.ended) {
                if let Some(pid) = session
                    .process_ref
                    .as_ref()
                    .and_then(|r| handle.agent.process_id(&SessionRef(r.clone())))
                {
                    pids.insert(pid);
                }
            }
        }
        pids
    }

    /// Name a refused caller: `seat-token` for any caller with `PF_SEAT_TOKEN` in its environment,
    /// `chat:<S-n>` for a caller whose `PULSE_FACTORY_CHAT` is `S-<digits>` (`chat:invalid` for any
    /// other value), and with no marker, by its own walk of the peer's ancestors: `chat:<S-n>` for a
    /// process that is, or descends from, a live chat process, `seat:<pid>` for one that descends from a tracked seat, and
    /// `unknown` when the walk cannot finish. A chat that forks many processes is one key.
    pub(super) fn settings_caller_key(&self, peer: Peer) -> String {
        let Some(pid) = peer.pid else {
            return "unknown".to_string();
        };
        let root = &self.cfg.proc_root;
        match std::fs::read(root.join(pid.to_string()).join("environ")) {
            Ok(environ) if has_seat_token(&environ) => return "seat-token".to_string(),
            // A detached child keeps the chat's marker where the walk finds no chat. The value is
            // cleaned first, so a caller cannot write text of its own choice into the ledger.
            Ok(environ) => {
                if let Some(value) = super::requests::environment_chat_marker(&environ) {
                    return super::requests::chat_marker_key(&value);
                }
            }
            Err(_) => return "unknown".to_string(),
        }
        let seats = self.seat_pids();
        let chats: Vec<(u32, String)> = self
            .live_chats()
            .iter()
            .filter_map(|l| l.port.process_id(&l.chat).map(|p| (p, l.session.clone())))
            .collect();
        let mut current = pid;
        for _ in 0..MAX_ANCESTORS {
            if seats.contains(&current) {
                return format!("seat:{current}");
            }
            if let Some((_, session)) = chats.iter().find(|(p, _)| *p == current) {
                return format!("chat:{session}");
            }
            match parent_of(root, current) {
                Some(parent) if parent > 1 => current = parent,
                Some(_) => return "unknown".to_string(),
                None => return "unknown".to_string(),
            }
        }
        "unknown".to_string()
    }

    // -------------------------------------------------------------- the audit of a refused caller

    /// Audit a refused settings write, coalesced per caller, bucket and minute (ruling 8). The
    /// window's first refusal commits one refused admission at once. Later ones are counted. The
    /// close, at the tick after the window or at the caller's next refusal after it, commits one
    /// more with the count. So a caller gets at most two admissions and two lines per bucket per
    /// minute, whatever it sends.
    fn refuse_settings_caller(&self, verb: &str, args: &Value, caller: &str, why: &str) {
        let bucket = args
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| KEYS.contains(k))
            .unwrap_or(OTHER_BUCKET)
            .to_string();
        let kind = kind_of(verb);
        let content = serde_json::from_value::<RawWrite>(args.clone())
            .map(|raw| content_of(kind, &raw))
            .unwrap_or_else(|_| "{}".to_string());
        self.refuse_coalesced(kind, bucket, content, caller, why);
    }

    /// The window of one caller and one bucket: the first refusal commits one refused admission at
    /// once, and later ones are counted for the closing line. `kind` is the admission kind, and
    /// `content` its content. `propose` shares this with the settings writes (`r1.s4.w2`).
    pub(super) fn refuse_coalesced(
        &self,
        kind: &'static str,
        bucket: String,
        content: String,
        caller: &str,
        why: &str,
    ) {
        let end = self.cfg.refusal_window;
        let mut windows = lock(&self.settings_refusals.windows);
        let key = (caller.to_string(), bucket.clone());
        let now_t = SystemTime::now();
        if let Some(window) = windows.get_mut(&key) {
            let age = now_t.duration_since(window.start).unwrap_or_default();
            if age < end {
                window.counted += 1;
                window.kind = kind;
                window.content = content;
                window.last_reason = why.to_string();
                return;
            }
            // The window ended: close it, then this refusal opens the next one.
            let closed = windows.remove(&key);
            if let Some(closed) = closed {
                self.close_window(caller, &bucket, closed, now_t);
            }
        }
        let mut window = Window {
            start: now_t,
            counted: 0,
            kind,
            content: content.clone(),
            last_reason: why.to_string(),
            noted: false,
        };
        let detail = format!(" caller={caller} bucket={bucket} refused=1");
        if let Err(note) = self.commit_caller_refusal(kind, &content, why, &detail) {
            window.noted = true;
            eprintln!("pulse-factory: a refused call is not audited: {note}");
        }
        windows.insert(key, window);
    }

    /// Close one window: when it counted any refusal after the first, commit one refused admission
    /// with the count and the window's bounds.
    fn close_window(&self, caller: &str, bucket: &str, mut window: Window, end: SystemTime) {
        if window.counted == 0 {
            return;
        }
        let detail = format!(
            " caller={caller} bucket={bucket} refused={} window={}..{}",
            window.counted,
            utc(window.start),
            utc(end)
        );
        let content = std::mem::take(&mut window.content);
        if let Err(note) =
            self.commit_caller_refusal(window.kind, &content, &window.last_reason, &detail)
            && !window.noted
        {
            eprintln!("pulse-factory: a refused call is not audited: {note}");
        }
    }

    /// The tick: close each window that has ended.
    pub(super) fn close_refusal_windows(&self) {
        let end = self.cfg.refusal_window;
        let now_t = SystemTime::now();
        let mut windows = lock(&self.settings_refusals.windows);
        let due: Vec<(String, String)> = windows
            .iter()
            .filter(|(_, w)| now_t.duration_since(w.start).unwrap_or_default() >= end)
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            if let Some(window) = windows.remove(&key) {
                self.close_window(&key.0, &key.1, window, now_t);
            }
        }
    }

    /// One refused admission under a command ID the controller makes, so the caller's own ID is
    /// never used up and a guessed ID cannot poison a later operator command.
    fn commit_caller_refusal(
        &self,
        kind: &'static str,
        content: &str,
        why: &str,
        detail: &str,
    ) -> Result<(), String> {
        let id = format!(
            "caller-refused-{}",
            hex(&random_bytes(&self.cfg.random_device, 16)?)
        );
        // A refused admission answers `Err(reason)`: that is the success of this call.
        match self.run_command(
            kind,
            &id,
            content.to_string(),
            Plan::RefuseDetailed {
                why: why.to_string(),
                detail: detail.to_string(),
            },
        ) {
            Ok(_) => Ok(()),
            Err(text) if text == pf_engine::status::one_line_reason(why) => Ok(()),
            Err(text) => Err(text),
        }
    }
}

/// The resolve of an `intersect` key: the built-in list cut by every stored value on the chain, in
/// the built-in order. `from` names the most specific stored row, or `built_in`; `layers` lists every
/// stored row; `masked` is empty, because no row hides another.
fn resolve_intersect(key: &str, layers: Vec<SettingLayer>) -> Resolved {
    let SettingValue::List(built) = built_in(key) else {
        unreachable!("an intersect key has a list for its built-in value");
    };
    let value: Vec<String> = built
        .into_iter()
        .filter(|name| {
            layers.iter().all(|l| match &l.value {
                SettingValue::List(list) => list.contains(name),
                SettingValue::Number(_) => true,
            })
        })
        .collect();
    let (from, from_text) = match layers.first() {
        Some(top) => (top.scope.clone(), scope_text(&top.scope)),
        None => (built_in_scope(), "built_in".to_string()),
    };
    Resolved {
        value: SettingValue::List(value),
        from,
        from_text,
        layers,
        masked: Vec::new(),
    }
}

/// The typed value of a set, for the answer. `None` for an unset or a value that does not pass.
fn plan_value(set: bool, raw: &RawWrite) -> Option<SettingValue> {
    if !set {
        return None;
    }
    raw.value
        .as_ref()
        .and_then(|v| check_value(&raw.key, v).ok())
}

fn scope_text(scope: &WireScope) -> String {
    match scope.kind {
        WireKind::BuiltIn => "built_in".to_string(),
        _ => SettingScope::new(scope.project_id.as_deref(), scope.agent_id.as_deref()).text(),
    }
}

/// The refusal window of the default config: one minute.
pub const DEFAULT_REFUSAL_WINDOW: Duration = Duration::from_secs(60);
