//! The wire types of the API and of the verbs that the controller shares with the CLI (ADR-0004,
//! ADR-0012). Each type derives `Serialize`, `Deserialize` and `ts_rs::TS`: the clients take their
//! types from the generated files in `apps/api-client/src/gen/`, and a test fails when those drift.
//!
//! The project shapes moved here from the controller with the same JSON. The controller imports them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The `schema_version` of every answer (ADR-0004).
pub const SCHEMA_VERSION: u32 = 1;

// ------------------------------------------------------------------ projects

/// The arguments of `project_add`. `root` is an absolute path. The controller makes it canonical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectAddArgs {
    pub root: String,
    /// The default is the last component of the canonical root.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub name: Option<String>,
    pub command_id: String,
}

/// The arguments of `project_remove`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectRemoveArgs {
    pub id: String,
    pub command_id: String,
}

/// What a project is (ADR-0015): a git repository's main worktree, or a plain folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum ProjectKind {
    Git,
    Folder,
}

/// One project, as every verb shows it.
///
/// `kind` is null only for a project that was added before the kind existed and that the
/// controller could not detect at its start. `kind_error` is then the cause, from a fresh detection
/// at list time, and it is null otherwise. `remote_key` is `host/path` of the git remote, for a
/// `git` project with one: never a URL, because a URL can hold a token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub root: String,
    pub created_at: String,
    pub removed_at: Option<String>,
    pub kind: Option<ProjectKind>,
    pub remote_key: Option<String>,
    pub kind_error: Option<String>,
}

/// The answer of `project_add` and `project_remove`. `replayed` is true when the command ID was
/// admitted before with the same content: the project is the first record's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectChanged {
    pub schema_version: u32,
    pub replayed: bool,
    pub project: ProjectRecord,
}

/// The answer of `project_list`: the live projects, in creation order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectList {
    pub schema_version: u32,
    pub projects: Vec<ProjectRecord>,
}

// ------------------------------------------------------------------ devices and pairing

/// One device, as `device list` and the API show it. The record never holds the token or its hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub paired_at: String,
    pub last_seen_at: Option<String>,
    pub last_peer: Option<String>,
    pub revoked_at: Option<String>,
}

/// The body of `POST /v1/pair`: the pairing code and a name for the device (1 to 64 characters).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
}

/// The answer of `POST /v1/pair`. The token is 64 hex digits, and it appears here once and nowhere
/// else. The server keeps only its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PairResult {
    pub schema_version: u32,
    pub device: DeviceRecord,
    pub token: String,
}

/// The server an answer comes from: the SHA-256 of the key pinned at pairing, and the server's
/// name. The id is public: any TLS peer sees the key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ServerRef {
    /// The SPKI SHA-256 as 64 hex digits. `None` with the API off.
    pub id: Option<String>,
    pub name: String,
}

/// The answer of `GET /v1/status`. `key_spki_sha256` is 64 hex digits. `device` is the calling
/// device. A host fills the first three fields and the router fills the device. `server` is absent
/// from an answer of an older server, so a client still parses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiStatus {
    pub schema_version: u32,
    pub version: String,
    pub key_spki_sha256: String,
    pub device: Option<DeviceRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server: Option<ServerRef>,
}

/// The body of every error answer of the API. It never holds a token, a code or a key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiError {
    pub error: String,
}

/// The arguments of `device_revoke`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRevokeArgs {
    pub id: String,
    pub command_id: String,
}

/// The answer of `device_revoke`. `replayed` is true when the command ID was admitted before with
/// the same content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRevoked {
    pub schema_version: u32,
    pub replayed: bool,
    pub device: DeviceRecord,
}

/// The answer of `device_list`: live devices first, each group in pairing order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceList {
    pub schema_version: u32,
    pub devices: Vec<DeviceRecord>,
}

/// The answer of `device_pair`: one copyable code and the time it stops working.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DevicePairing {
    pub schema_version: u32,
    pub code: String,
    pub expires_at: String,
}

// ------------------------------------------------------------------ the listener's state

/// Where the listener stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum ListenState {
    /// No address is set. No key was made.
    Off,
    /// The listener is bound and serves.
    Listening,
    /// The address is not on this host yet (Tailscale may start after the controller). The
    /// controller tries again every 5 seconds.
    Waiting,
    /// The bind failed for another reason, or the key could not be made. The controller does not
    /// try again, and it keeps serving the socket.
    Failed,
}

/// The answer of `api_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiState {
    pub schema_version: u32,
    pub state: ListenState,
    pub listen: Option<String>,
    pub key_spki_sha256: Option<String>,
    /// Why the state is `failed`: one line that names no token, code or key.
    pub error: Option<String>,
}

// ------------------------------------------------------------------ chat sessions (r1.s3.w3)

/// What a live chat session is doing. The same three words as the store's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatState {
    Working,
    NeedsYou,
    Idle,
}

/// A session's summed usage: the exact sum of its turns' own usage (C8). The cost is in millionths
/// of a dollar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatUsage {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    #[ts(type = "number")]
    pub cache_read_tokens: u64,
    #[ts(type = "number")]
    pub cache_write_tokens: u64,
    #[ts(type = "number")]
    pub cost_micros: u64,
}

/// A background task a chat started and has not seen finish.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatTaskRecord {
    pub id: String,
    pub kind: String,
    pub description: String,
    /// When the task started, as UTC text.
    pub since: String,
}

/// The live process of a chat session, and why it is held. `releases_at` is `None` while a holder
/// (a turn, a task or a pending request) holds the process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatProcessRecord {
    pub since: String,
    pub runtime_version: String,
    /// False for an override binary: every view marks it `unqualified`.
    pub qualified: bool,
    pub turn_since: Option<String>,
    pub waiting_since: Option<String>,
    pub tasks: Vec<ChatTaskRecord>,
    pub releases_at: Option<String>,
}

/// The context gauge of a chat: tokens used, and the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatContext {
    #[ts(type = "number")]
    pub used_tokens: u64,
    #[ts(type = "number")]
    pub max_tokens: u64,
}

/// One chat session, as every verb and the stream show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatSessionRecord {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub worktree_id: String,
    pub worktree_dir: String,
    /// Read live. `None` for a folder.
    pub branch: Option<String>,
    pub detached: bool,
    pub agent_id: String,
    pub runtime: String,
    pub runtime_session_ref: Option<String>,
    pub state: ChatState,
    /// Why the session cannot resume, when it cannot (C15). The session is ended and started anew.
    pub context_lost: Option<String>,
    /// `None`: no live process.
    pub process: Option<ChatProcessRecord>,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    /// The session's own runtime options (they start as the agent's and change per chat).
    pub runtime_options: BTreeMap<String, String>,
    /// Options set and not yet in the live process: they apply at the next open.
    pub pending_options: BTreeMap<String, String>,
    pub usage: ChatUsage,
    pub last_turn_at: Option<String>,
    /// 300 or 3600: the cache-warm timer of the last turn.
    pub cache_ttl_seconds: Option<u32>,
    pub context: Option<ChatContext>,
    pub created_at: String,
    pub updated_at: String,
    pub ended_at: Option<String>,
}

/// One option a runtime declares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatOptionRecord {
    pub id: String,
    pub label: String,
    pub values: Vec<String>,
    pub default: String,
    pub free: bool,
    pub live: bool,
}

/// What a message sent during a turn does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MidTurnMode {
    Steer,
    NextTurn,
}

/// One chat runtime this server has: its declared options, what a mid-turn send does, and whether
/// it can open a chat now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatRuntimeRecord {
    pub id: String,
    pub options: Vec<ChatOptionRecord>,
    pub mid_turn: MidTurnMode,
    pub ready: bool,
    pub error: Option<String>,
}

/// One agent: a persona with its runtime settings. The record holds no environment value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub title: String,
    pub built_in: bool,
    pub colour: String,
    pub head: String,
    pub runtime: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    pub skills: Vec<String>,
    pub runtime_options: BTreeMap<String, String>,
}

/// The answer of `session_list`: the server, the runtimes, the agents and the sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionList {
    pub schema_version: u32,
    pub server: ServerRef,
    pub runtimes: Vec<ChatRuntimeRecord>,
    pub agents: Vec<AgentRecord>,
    pub sessions: Vec<ChatSessionRecord>,
}

/// The answer of `session_send`, `session_answer`, `session_interrupt` and `session_set`. `note`
/// says why an admitted command could not act (an open that failed after the admission).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionChanged {
    pub schema_version: u32,
    pub replayed: bool,
    pub session: ChatSessionRecord,
    pub note: Option<String>,
}

/// A tool call's input or output: the text as the adapter capped it, the size before the cut, and
/// whether it was cut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolTextRecord {
    pub text: String,
    #[ts(type = "number")]
    pub bytes: u64,
    pub truncated: bool,
}

/// How a request ended: the operator answered it, the process that asked is gone, or the runtime
/// withdrew it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AnswerOutcome {
    Answered,
    Lost,
    Withdrawn,
}

/// One choice of a question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionChoiceRecord {
    pub label: String,
    pub description: String,
}

/// One question the agent asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionRecord {
    pub text: String,
    pub header: String,
    pub options: Vec<QuestionChoiceRecord>,
    pub multi_select: bool,
}

/// The body of one chat item, by its kind. A redacted row shows the marks only, and a kind this
/// binary does not know shows as `unknown` with its own kind text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatItemBody {
    OperatorMessage {
        text: String,
    },
    AssistantText {
        text: String,
    },
    ToolCall {
        call_id: String,
        tool: String,
        input: Option<ToolTextRecord>,
        output: Option<ToolTextRecord>,
        is_error: bool,
        finished: bool,
    },
    ApprovalRequest {
        request_id: String,
        tool: String,
        input: Option<ToolTextRecord>,
        reason: Option<String>,
    },
    ApprovalAnswer {
        outcome: AnswerOutcome,
        reason: Option<String>,
        /// `allow` or `deny`, for an answer that the operator gave.
        decision: Option<String>,
        message: Option<String>,
    },
    QuestionRequest {
        request_id: String,
        items: Vec<QuestionRecord>,
    },
    QuestionAnswer {
        outcome: AnswerOutcome,
        reason: Option<String>,
        answers: Option<BTreeMap<String, String>>,
    },
    BackgroundTask {
        /// `started` or `finished`.
        event: String,
        task_id: String,
        task_kind: Option<String>,
        description: Option<String>,
        end: Option<String>,
        summary: Option<String>,
    },
    /// A line the controller adds: `idle_release`, `context_lost`, `exited`, `halted`,
    /// `open_failed` or `option_restart`.
    Notice {
        code: String,
        text: String,
    },
    TurnResult {
        cause: String,
        usage: ChatUsage,
        is_error: bool,
        stop: String,
        #[ts(type = "number")]
        duration_ms: u64,
        cache_ttl_seconds: Option<u32>,
    },
    /// A redacted row: the marks only.
    Redacted {
        item_kind: String,
    },
    /// A kind this binary does not know.
    Unknown {
        item_kind: String,
    },
}

/// One item of a chat's display log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChatItemRecord {
    pub session_id: String,
    #[ts(type = "number")]
    pub seq: u64,
    /// An answer item: the `seq` of the request it answers.
    #[ts(type = "number | null")]
    pub answers_seq: Option<u64>,
    pub created_at: String,
    pub redacted_at: Option<String>,
    pub body: ChatItemBody,
}

/// The answer of `session_items`: stored items after a `seq`, at most 500.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionItems {
    pub schema_version: u32,
    pub session_id: String,
    pub items: Vec<ChatItemRecord>,
}

/// One frame of the chat stream (`GET /v1/stream`), as a JSON text message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
// A frame lives for one send: it is built, serialised and dropped. Boxing the record would change
// every construction site and match, for a size that no one keeps.
#[allow(clippy::large_enum_variant)]
pub enum ChatFrame {
    /// The first frame: the server, every session, the session followed and the `seq` the stored
    /// items start after.
    Hello {
        server: ServerRef,
        sessions: Vec<ChatSessionRecord>,
        following: Option<String>,
        #[ts(type = "number")]
        after_seq: u64,
    },
    /// A session's record changed: its state, cost, holders or options.
    Session { session: ChatSessionRecord },
    /// One stored item, in `seq` order.
    Item { item: ChatItemRecord },
    /// The stored items are sent. What follows is live.
    Caught {
        session_id: String,
        #[ts(type = "number")]
        last_seq: u64,
    },
    /// Text as it streams. A delta is never stored and is not replayed after a reconnect.
    Delta { session_id: String, text: String },
    /// A tool call started and has not finished.
    ToolRunning {
        session_id: String,
        call_id: String,
        tool: String,
    },
}

fn orchestrator_agent() -> String {
    "orchestrator".to_string()
}

/// The arguments of `session_start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionStartArgs {
    pub project_id: String,
    /// The default is `orchestrator`.
    #[serde(default = "orchestrator_agent")]
    #[ts(optional, as = "Option<String>")]
    pub agent_id: String,
    pub command_id: String,
}

/// The arguments of `session_send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionSendArgs {
    pub session_id: String,
    pub text: String,
    pub command_id: String,
}

/// The answer to an approval or a question, as the chat port's `ChatAnswer` writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ChatAnswerArg {
    Allow,
    Deny {
        message: String,
    },
    /// Question text to the chosen answer.
    Answers(BTreeMap<String, String>),
}

/// The arguments of `session_answer`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionAnswerArgs {
    pub session_id: String,
    /// The `seq` of the request item.
    #[ts(type = "number")]
    pub seq: u64,
    pub answer: ChatAnswerArg,
    pub command_id: String,
}

/// The arguments of `session_interrupt`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionInterruptArgs {
    pub session_id: String,
    pub command_id: String,
}

/// The arguments of `session_set`: at least one of the four fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionSetArgs {
    pub session_id: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub model: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub effort: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub options: Option<BTreeMap<String, String>>,
    pub command_id: String,
}

/// The arguments of `session_end`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionEndArgs {
    pub session_id: String,
    pub command_id: String,
}

/// The arguments of `session_list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionListArgs {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub project_id: Option<String>,
    /// Also the ended sessions.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub all: Option<bool>,
}

/// The arguments of `session_redact`. The verb is the CLI's only, so this type has no TypeScript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRedactArgs {
    pub session_id: String,
    pub seq: u64,
    pub command_id: String,
}

// ------------------------------------------------------------------ settings (ADR-0016)

/// The value of a setting: a JSON number or a list of strings. The controller's registry decides
/// which keys exist and what each value may be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum SettingValue {
    Number(#[ts(type = "number")] u64),
    List(Vec<String>),
}

/// Where a setting row sits: the server default, one project, one agent, or one agent in one
/// project. `built_in` is the scope of a value that no row set (the binary's own default).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingScope {
    #[ts(inline)]
    pub kind: SettingScopeKind,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
}

/// The kinds of a [`SettingScope`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SettingScopeKind {
    Server,
    Project,
    Agent,
    ProjectAgent,
    BuiltIn,
}

/// How a key combines the rows of its scope chain. `override` is the one mode built: the most
/// specific row wins whole, and every other stored row on the chain is masked. Later modes (`union`,
/// `min`) add variants, with no migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SettingMerge {
    Override,
}

/// When a change of a setting takes effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SettingApplies {
    /// At the next self-started turn of a live process.
    NextTurn,
    /// At the chat's next launch.
    NextLaunch,
}

/// One stored row on the scope chain of a context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingLayer {
    pub scope: SettingScope,
    pub value: SettingValue,
    /// `cli`, or the ID of the device that set it.
    pub set_by: String,
    pub set_at: String,
}

/// One key as a context reads it: the resolved value and the scope it came from, the binary's
/// default, how the key merges, when a change applies, every stored row on the chain (`layers`,
/// most specific first) and the rows the resolved value hides (`masked`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingRecord {
    pub key: String,
    pub value: SettingValue,
    pub from: SettingScope,
    pub built_in: SettingValue,
    pub merge: SettingMerge,
    #[ts(inline)]
    pub applies: SettingApplies,
    #[ts(inline)]
    pub layers: Vec<SettingLayer>,
    #[ts(inline)]
    pub masked: Vec<SettingLayer>,
}

/// The arguments of `settings_get`. No key asks for every key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingsGetArgs {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub key: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub agent_id: Option<String>,
}

/// The arguments of `settings_set`. The scope is the project and the agent named: none is the server
/// default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingsSetArgs {
    pub key: String,
    pub value: SettingValue,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub agent_id: Option<String>,
    pub command_id: String,
}

/// The arguments of `settings_unset`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingsUnsetArgs {
    pub key: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub agent_id: Option<String>,
    pub command_id: String,
}

/// The answer of `settings_get`: one record for each known key, or the one key asked for, as the
/// context (`project_id`, `agent_id`) reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingsView {
    pub schema_version: u32,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    #[ts(inline)]
    pub settings: Vec<SettingRecord>,
}

/// The answer of `settings_set` and `settings_unset`. `replayed` is true when the command ID was
/// admitted before with the same content. `value` is null after an unset. `resolved` is the key as the
/// scope of the change reads it now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingChanged {
    pub schema_version: u32,
    pub replayed: bool,
    pub key: String,
    pub scope: SettingScope,
    pub value: Option<SettingValue>,
    pub resolved: SettingRecord,
}

/// Write the TypeScript file of every wire type, with its dependencies, as `cfg` says. The
/// generated files in `apps/api-client/src/gen/` are exactly this output.
pub fn export_all(cfg: &ts_rs::Config) -> Result<(), ts_rs::ExportError> {
    ProjectAddArgs::export_all(cfg)?;
    ProjectRemoveArgs::export_all(cfg)?;
    ProjectRecord::export_all(cfg)?;
    ProjectKind::export_all(cfg)?;
    ProjectChanged::export_all(cfg)?;
    ProjectList::export_all(cfg)?;
    DeviceRecord::export_all(cfg)?;
    PairRequest::export_all(cfg)?;
    PairResult::export_all(cfg)?;
    ServerRef::export_all(cfg)?;
    ApiStatus::export_all(cfg)?;
    ChatState::export_all(cfg)?;
    ChatUsage::export_all(cfg)?;
    ChatTaskRecord::export_all(cfg)?;
    ChatProcessRecord::export_all(cfg)?;
    ChatContext::export_all(cfg)?;
    ChatSessionRecord::export_all(cfg)?;
    ChatOptionRecord::export_all(cfg)?;
    MidTurnMode::export_all(cfg)?;
    ChatRuntimeRecord::export_all(cfg)?;
    AgentRecord::export_all(cfg)?;
    SessionList::export_all(cfg)?;
    SessionChanged::export_all(cfg)?;
    SessionItems::export_all(cfg)?;
    ToolTextRecord::export_all(cfg)?;
    AnswerOutcome::export_all(cfg)?;
    QuestionChoiceRecord::export_all(cfg)?;
    QuestionRecord::export_all(cfg)?;
    ChatItemBody::export_all(cfg)?;
    ChatItemRecord::export_all(cfg)?;
    ChatFrame::export_all(cfg)?;
    ChatAnswerArg::export_all(cfg)?;
    SessionStartArgs::export_all(cfg)?;
    SessionSendArgs::export_all(cfg)?;
    SessionAnswerArgs::export_all(cfg)?;
    SessionInterruptArgs::export_all(cfg)?;
    SessionSetArgs::export_all(cfg)?;
    SessionEndArgs::export_all(cfg)?;
    SessionListArgs::export_all(cfg)?;
    SettingValue::export_all(cfg)?;
    SettingScope::export_all(cfg)?;
    SettingMerge::export_all(cfg)?;
    SettingRecord::export_all(cfg)?;
    SettingsGetArgs::export_all(cfg)?;
    SettingsSetArgs::export_all(cfg)?;
    SettingsUnsetArgs::export_all(cfg)?;
    SettingsView::export_all(cfg)?;
    SettingChanged::export_all(cfg)?;
    ApiError::export_all(cfg)?;
    DeviceRevokeArgs::export_all(cfg)?;
    DeviceRevoked::export_all(cfg)?;
    DeviceList::export_all(cfg)?;
    DevicePairing::export_all(cfg)?;
    ListenState::export_all(cfg)?;
    ApiState::export_all(cfg)?;
    Ok(())
}

/// The rules for a device name: 1 to 64 characters, with no control character and no line break.
pub fn check_device_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n == 0 {
        return Err("the device name is empty".to_string());
    }
    if n > DEVICE_NAME_MAX_CHARS {
        return Err(format!(
            "the device name has {n} characters, and the limit is {DEVICE_NAME_MAX_CHARS}"
        ));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err("the device name holds a control character or a line break".to_string());
    }
    Ok(())
}

/// A device name is 1 to 64 characters.
pub const DEVICE_NAME_MAX_CHARS: usize = 64;
