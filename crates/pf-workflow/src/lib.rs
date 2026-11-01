//! `pf-workflow`: the strict workflow file and what the engine runs from it.
//!
//! This crate reads the versioned workflow file, the queue a seat fills, and the role rule
//! files. It runs the commands the file names under fixed bounds, decides from a stage's signals
//! whether the stage is done, pins plugin directories, and re-admits seed units. It names no
//! planning method (ADR-0002). It depends on `pf-engine` and on no other `pf-` crate.

pub mod commands;
mod convert;
pub mod file;
mod marked;
pub mod package;
pub mod pin;
pub mod queue;
pub mod rules;
pub mod seeds;
pub mod signals;

pub use commands::CommandSpec;
pub use file::{
    DoneCond, MergeSpec, ParkOn, PluginReq, PrTest, RepeatDef, RoleDef, STAGE_SETTING_KEYS,
    SUPPORTED_VERSION, SeedDef, StageSpec, TrackDef, UnitSourceDef, WorkflowFile,
};
pub use marked::ParseError;
