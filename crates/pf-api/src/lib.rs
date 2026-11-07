//! `pf-api`: the controller's network API (ADR-0012, `docs/contracts/api.md`).
//!
//! - [`types`]: the wire types, which also derive `ts_rs::TS` for the clients.
//! - [`code`]: the pairing code.
//! - [`host`]: the port the controller implements, and the allowlist of verbs.
//! - [`router`]: the routes of version 1 over a host.
//! - [`tls`]: the server key, its certificate and the SPKI hash that clients pin.
//! - [`listener`]: the TLS listener.
//!
//! The crate never depends on the controller. The controller depends on it.

pub mod code;
pub mod host;
pub mod listener;
pub mod router;
pub mod tls;
pub mod types;

pub use host::{ALLOWLIST, ApiHost, Refusal};
pub use router::{MAX_BODY_BYTES, PeerAddr, router};
