//! `pf-engine`: the generic engine core.
//!
//! The engine holds units, stages, sessions, signals, admissions, merge by rule, the ledger,
//! capacity and start-up reconcile. It defines its ports in [`port`]: store, agent adapter, forge
//! and unit source. It depends on no other `pf-` crate and it knows no planning method and no
//! agent (ADR-0002).

pub mod admission;
pub mod capacity;
pub mod engine;
pub mod launch;
pub mod ledger;
pub mod merge;
pub mod port;
pub mod reconcile;
pub mod status;
pub mod unit;
pub mod workflow;

pub use admission::{Admission, Command, CommandBody, EngineError, RunView};
pub use engine::{Clock, Engine, EngineConfig, IngestReport, system_clock};
pub use status::{EndStatus, Status};
pub use workflow::{MergeRule, StageDef, Workflow};
