//! `pf-adapter-claude`: the chat adapter for Claude Code. It drives a managed copy of Anthropic's
//! official CLI binary, pinned by version and SHA-256, headless over `stream-json`, on the
//! operator's subscription login (ADR-0014).
//!
//! - [`ClaudeChat`] implements the chat port of `pf-engine`.
//! - `pf-claude-replay` (a binary of this crate) stands in for the CLI and plays a recorded fixture
//!   back, so tests and demos run the real adapter with no tokens and no network.
//! - [`probe`] is the budget of real processes, for the marked qualification tests.
//!
//! The contract: `docs/contracts/chat-adapter.md`.

mod chat;
pub mod decode;
pub mod launch;
pub mod pinned;
pub mod probe;
pub mod runtime;
pub mod sha256;
pub mod version;

pub use chat::{CLAUDE_CODE_RUNTIME, ClaudeChat, ClaudeConfig};
