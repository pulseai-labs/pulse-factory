//! The workflow data the engine runs. The workflow file format is not here: a workflow crate
//! reads its file and builds this value.

use crate::capacity::Settings;
use crate::port::agent::RuleSet;

/// One stage of a workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageDef {
    pub name: String,
    pub role: String,
    pub model: String,
    pub rules: RuleSet,
}

impl StageDef {
    pub fn new(name: &str, role: &str, model: &str) -> StageDef {
        StageDef {
            name: name.to_string(),
            role: role.to_string(),
            model: model.to_string(),
            rules: RuleSet::default(),
        }
    }
}

/// The rule that merges a unit's PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeRule {
    /// Merge when the checks are green, one unit at a time, in the declared order.
    GreenChecksInDeclaredOrder,
}

/// A workflow as the engine sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    pub stages: Vec<StageDef>,
    /// `None`: a unit is done when its last stage is done.
    pub merge: Option<MergeRule>,
    pub settings: Settings,
}
