//! What the qualification of CLI 2.1.319 selected (ADR-0014, "Verified claims"), one constant per
//! optional capability. The adapter builds the selected path only, and tests it on the recordings.
//! A new CLI version needs a new qualification run, and these constants follow its results
//! (`docs/contracts/chat-adapter.md`, "Bump the CLI pin").

use pf_engine::port::chat::MidTurn;

/// The version the chat qualifies: the pin's CLI version.
pub const QUALIFIED_VERSION: &str = crate::pinned::CLI_VERSION;

/// What a `send` during a turn does. Case 10: the CLI folds the message into the running turn
/// (one `init`, one `result` that lists both message ids). `FakeChat` holds the same value.
pub const MID_TURN: MidTurn = MidTurn::Steer;

/// What starts a turn when a background task finishes. Case 11: the CLI does (`Cli`); the adapter
/// sends no notice.
pub const SELF_WAKE: SelfWake = SelfWake::Cli;

/// How a task finish reaches the chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfWake {
    /// The CLI starts a turn itself.
    Cli,
}

/// Case 12: `set_model`, `apply_flag_settings` (`effortLevel`) and `set_permission_mode` change a
/// running process.
pub const LIVE_CHANGES: bool = true;

/// Case 13: `get_context_usage` with `detail: "summary"` answers `totalTokens` and `maxTokens`.
pub const CONTEXT_USAGE: bool = true;

/// Case 11: `system/background_tasks_changed` gives the task set (replace semantics).
pub const TASK_SIGNAL: &str = "background_tasks_changed";

/// Cases 17 and 18: with `CLAUDE_CODE_ARTIFACT=1` the CLI lists the three Artifact tools, and a
/// headless publish works. When this is false, `options()` offers `artifacts` only as `off`, and the
/// adapter launches with `CLAUDE_CODE_ARTIFACT=0` whatever an agent stores.
pub const ARTIFACTS_SUPPORTED: bool = true;

/// Case 17: `apply_flag_settings {enableArtifact:false}` removes the tools from a process started
/// with `CLAUDE_CODE_ARTIFACT=1`, but `{enableArtifact:true}` does not add them to a process started
/// with 0. A one-way toggle makes `set_option` lie, so `artifacts` is not live.
pub const ARTIFACTS_LIVE: bool = false;
