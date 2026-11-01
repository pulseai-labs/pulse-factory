//! `pulse-factory`: the CLI verbs, the controller and the hook (ADR-0001, ADR-0004).
//!
//! The binary composes the libraries of Release 0. It knows no planning method (ADR-0002): a
//! workflow package names the tools, and `arm` takes the values it needs as `--param`.

pub mod arm;
pub mod cli;
pub mod controller;
pub mod forge;
pub mod format;
pub mod hook;

pub use cli::run;
