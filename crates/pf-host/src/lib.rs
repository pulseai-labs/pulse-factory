//! `pf-host`: the session host (ADR-0001, amendment of 2026-11-16; `docs/contracts/session-host.md`).
//!
//! A host is one small process per session. It starts the session's child on a PTY or on pipes,
//! keeps the output the controller has not acknowledged, relays input, resize and signals, holds
//! the child's exit status, and answers one attached controller on a Unix socket. This crate also
//! holds the client the controller uses and the two launchers that start a host.
//!
//! The host stays dumb: it depends on no engine, no planning crate and no adapter crate, and its
//! source names no runtime. What the child is, is the controller's business.

pub mod buffer;
pub mod client;
pub mod exitfile;
pub mod host;
pub mod launcher;
pub mod names;
pub mod peer;
pub mod protocol;
pub mod record;
pub mod sha256;
