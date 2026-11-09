//! What a chat process is started with: the command line, the environment and the options. Every
//! function here is pure, so a test calls it without a process.

use std::collections::BTreeMap;
use std::io::Read;

use pf_engine::port::chat::{ChatError, ChatOpen, ChatOption, check_deny_commands};

use crate::decode::TIMER_TOOLS;
use crate::version::{ARTIFACTS_LIVE, ARTIFACTS_SUPPORTED};

pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The permission modes of the pinned CLI (`claude --help`).
pub const MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// The model values the CLI listed in its `initialize` response (qualification case 6), and the
/// full ids they resolve to. The model option is free: any id of the grammar passes.
pub const MODELS: [&str; 17] = [
    "default",
    "opus",
    "fable",
    "sonnet",
    "haiku",
    "claude-opus-5-5",
    "claude-fable-5-1",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-haiku-4-5-20251001",
    "claude-sonnet-5",
    "claude-opus-5",
    "claude-fable-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
];

/// The names the chat's environment takes from the inherited one, when set.
const ALLOWED_EXACT: [&str; 16] = [
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TERM",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "SSH_AUTH_SOCK",
];

/// A fresh version-4 UUID from the random device.
pub fn uuid_v4() -> Result<String, ChatError> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| ChatError::Failed(format!("cannot read /dev/urandom: {e}")))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// The model grammar: `[A-Za-z0-9._:\[\]-]{1,64}`.
pub fn model_ok(model: &str) -> bool {
    (1..=64).contains(&model.len())
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '[' | ']' | '-'))
}

/// The options this adapter declares, fixed for the qualified version.
pub fn options() -> Vec<ChatOption> {
    let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    vec![
        ChatOption {
            id: "model".into(),
            label: "Model".into(),
            values: strings(&MODELS),
            default: "claude-opus-5-5".into(),
            free: true,
            live: true,
        },
        ChatOption {
            id: "effort".into(),
            label: "Effort".into(),
            values: strings(&EFFORTS),
            default: "high".into(),
            free: false,
            live: true,
        },
        ChatOption {
            id: "permission_mode".into(),
            label: "Permission mode".into(),
            values: strings(&MODES),
            default: "bypassPermissions".into(),
            free: false,
            live: true,
        },
        ChatOption {
            id: "artifacts".into(),
            label: "Artifacts".into(),
            values: if ARTIFACTS_SUPPORTED {
                strings(&["on", "off"])
            } else {
                strings(&["off"])
            },
            default: if ARTIFACTS_SUPPORTED { "on" } else { "off" }.into(),
            free: false,
            live: ARTIFACTS_SUPPORTED && ARTIFACTS_LIVE,
        },
    ]
}

/// What `validate` found out beyond "this open is allowed".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validated {
    /// The artifacts option, after the degrade: `false` whenever the version cannot publish.
    pub artifacts: bool,
    /// The agent stored `on` but the degrade turned it off: the adapter logs one line.
    pub degraded: bool,
}

/// Check an open before any binary is touched. Every failure is `Refused` and names what is wrong.
pub fn validate(spec: &ChatOpen) -> Result<Validated, ChatError> {
    let refuse = |why: String| Err(ChatError::Refused(why));
    if !std::path::Path::new(&spec.cwd).is_dir() {
        return refuse(format!("cwd {:?} is not a directory", spec.cwd));
    }
    if !model_ok(&spec.model) {
        return refuse(format!(
            "model {:?} does not match [A-Za-z0-9._:\\[\\]-]{{1,64}}",
            spec.model
        ));
    }
    if !EFFORTS.contains(&spec.effort.as_str()) {
        return refuse(format!(
            "effort {:?} is not one of {}",
            spec.effort,
            EFFORTS.join(", ")
        ));
    }
    if !MODES.contains(&spec.permission_mode.as_str()) {
        return refuse(format!(
            "permission mode {:?} is not one of {}",
            spec.permission_mode,
            MODES.join(", ")
        ));
    }
    check_deny_commands(&spec.deny_commands).map_err(ChatError::Refused)?;
    let mut stored = true;
    for (key, value) in &spec.options {
        match key.as_str() {
            "artifacts" => match value.as_str() {
                "on" => stored = true,
                "off" => stored = false,
                other => {
                    return refuse(format!("option artifacts is {other:?}, not on or off"));
                }
            },
            "model" | "effort" | "permission_mode" => {
                return refuse(format!(
                    "option {key} belongs in its own field of the open, not in the options map"
                ));
            }
            other => return refuse(format!("unknown option {other}")),
        }
    }
    let artifacts = stored && ARTIFACTS_SUPPORTED;
    Ok(Validated {
        artifacts,
        degraded: stored && !ARTIFACTS_SUPPORTED,
    })
}

/// The value of `--disallowedTools`: the five timer tools, then, for each deny pattern `c` in order,
/// `Bash(c)` and `Bash(c *)`, or `Bash(c)` alone when `c` ends in `*`, all joined by `,`. `Bash(c)`
/// matches the bare command, and `Bash(c *)` matches it with arguments (the pinned help's
/// `Bash(git *)` form). With an empty list it is the timer tools alone.
///
/// A pattern passed [`check_deny_commands`], so no `(`, `)`, `,` or `:*` reaches a rule. A deny rule
/// matches the command text the model writes: it is a speed bump, not a boundary (ADR-0014).
pub fn disallowed_tools(deny: &[String]) -> String {
    let mut rules: Vec<String> = TIMER_TOOLS.iter().map(ToString::to_string).collect();
    for c in deny {
        rules.push(format!("Bash({c})"));
        if !c.ends_with('*') {
            rules.push(format!("Bash({c} *)"));
        }
    }
    rules.join(",")
}

/// The command line, the same for every process of one session (a new session and a resume differ
/// only in the last pair). The program runs with no shell, and the instructions are one argument.
/// `session_id` is the fresh UUID of a new session, or the stored id of a resume.
pub fn argv(spec: &ChatOpen, session_id: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
        "--allow-dangerously-skip-permissions",
        "--permission-mode",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(spec.permission_mode.clone());
    a.push("--model".into());
    a.push(spec.model.clone());
    a.push("--effort".into());
    a.push(spec.effort.clone());
    a.push("--disallowedTools".into());
    a.push(disallowed_tools(&spec.deny_commands));
    if !spec.instructions.is_empty() {
        a.push("--append-system-prompt".into());
        a.push(spec.instructions.clone());
    }
    a.push(
        if spec.resume.is_some() {
            "--resume"
        } else {
            "--session-id"
        }
        .into(),
    );
    a.push(session_id.to_string());
    a
}

/// The names an agent's environment map may not set: the chat runtime's own, Pulse Factory's, and
/// the Orca and herdr tokens.
fn reserved(name: &str) -> bool {
    ["ANTHROPIC_", "CLAUDE_CODE_", "PF_", "ORCA_", "HERDR_"]
        .iter()
        .any(|p| name.starts_with(p))
        || ["CLAUDE_CONFIG_DIR", "CLAUDECODE", "DISABLE_AUTOUPDATER"].contains(&name)
}

/// The environment of the chat process: an allowlist, never the whole inherited one.
///
/// - from `inherited`, when set: the names of [`ALLOWED_EXACT`] and every `LC_*`;
/// - the agent's `env` map, each name matching `[A-Z0-9_]+` and not reserved (a refusal names the
///   variable);
/// - set by the adapter: `DISABLE_AUTOUPDATER=1`, and `CLAUDE_CODE_ARTIFACT` to `1` or `0` by the
///   option. These are the only `CLAUDE_CODE_*` names the child gets: an inherited
///   `CLAUDE_CODE_ARTIFACT` or `CLAUDE_CODE_ENTRYPOINT` is dropped, and `CLAUDE_CODE_ENTRYPOINT`
///   is never set.
pub fn build_env(
    inherited: &BTreeMap<String, String>,
    env: &BTreeMap<String, String>,
    artifacts: bool,
) -> Result<BTreeMap<String, String>, ChatError> {
    let mut out: BTreeMap<String, String> = inherited
        .iter()
        .filter(|(k, _)| ALLOWED_EXACT.contains(&k.as_str()) || k.starts_with("LC_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (name, value) in env {
        let grammar = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !grammar {
            return Err(ChatError::Refused(format!(
                "environment name {name:?} does not match [A-Z0-9_]+"
            )));
        }
        if reserved(name) {
            return Err(ChatError::Refused(format!(
                "environment name {name} is reserved: the adapter sets or forbids it"
            )));
        }
        out.insert(name.clone(), value.clone());
    }
    out.insert("DISABLE_AUTOUPDATER".into(), "1".into());
    out.insert(
        "CLAUDE_CODE_ARTIFACT".into(),
        if artifacts { "1" } else { "0" }.into(),
    );
    Ok(out)
}
