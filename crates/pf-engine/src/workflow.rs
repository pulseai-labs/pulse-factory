//! The workflow data the engine runs. The workflow file format is not here: a workflow crate
//! reads its file and builds this value.

use std::collections::BTreeMap;

use crate::capacity::Settings;
use crate::port::agent::RuleSet;

/// The runtime name that [`StageDef::new`] gives a stage.
pub const DEFAULT_RUNTIME: &str = "default";

/// One stage of a workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageDef {
    pub name: String,
    pub role: String,
    pub model: String,
    /// The agent runtime of the stage's role, a free name such as `default`. The engine knows no
    /// runtime: it hands the name to the launch plan, and the adapter router reads it.
    pub runtime: String,
    pub rules: RuleSet,
    /// Launch settings, as templates. `start_stage` renders them into the launch plan.
    /// The keys are documented in `docs/contracts/launch-settings.md`.
    pub settings: BTreeMap<String, String>,
}

impl StageDef {
    pub fn new(name: &str, role: &str, model: &str) -> StageDef {
        StageDef {
            name: name.to_string(),
            role: role.to_string(),
            model: model.to_string(),
            runtime: DEFAULT_RUNTIME.to_string(),
            rules: RuleSet::default(),
            settings: BTreeMap::new(),
        }
    }
}

/// The rule that merges a unit's PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeRule {
    /// Merge when the checks are green, one unit at a time, in the declared order.
    GreenChecksInDeclaredOrder,
}

/// The merge definition of a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeDef {
    pub rule: MergeRule,
    /// The repository a unit's PR must be in, with placeholders. The engine renders it for each
    /// unit and parks a unit whose PR names another repo. Empty declares nothing.
    pub repo: String,
    /// The branch a unit's PR must merge into, with placeholders. The engine renders it for each
    /// unit and parks a unit whose PR has another base. Empty declares nothing.
    pub base: String,
    /// Compare the PR head with the last recorded push before the merge ([`crate::merge::head_check`]).
    pub head_check: bool,
}

impl MergeDef {
    /// A merge definition with no repo, no base and no head check.
    pub fn new(rule: MergeRule) -> MergeDef {
        MergeDef {
            rule,
            repo: String::new(),
            base: String::new(),
            head_check: false,
        }
    }
}

/// A named list of stages, with an optional merge definition. A unit moves through the stages of
/// its own track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub name: String,
    pub stages: Vec<StageDef>,
    /// `None`: a unit is done when its last stage is done.
    pub merge: Option<MergeDef>,
}

/// The name of the only track of a workflow built by [`Workflow::single_track`].
pub const SINGLE_TRACK: &str = "main";

/// A workflow as the engine sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    pub tracks: Vec<Track>,
    pub settings: Settings,
    /// The directories that a working directory from a unit record may lie under. They may hold
    /// `{run.*}` and `{param.*}` placeholders.
    pub roots: Vec<String>,
}

impl Workflow {
    /// A workflow with one track, named `main`.
    pub fn single_track(
        name: &str,
        stages: Vec<StageDef>,
        merge: Option<MergeRule>,
        settings: Settings,
    ) -> Workflow {
        Workflow {
            name: name.to_string(),
            tracks: vec![Track {
                name: SINGLE_TRACK.to_string(),
                stages,
                merge: merge.map(MergeDef::new),
            }],
            settings,
            roots: Vec::new(),
        }
    }

    pub fn track(&self, name: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.name == name)
    }

    /// The track that a source with no track joins: the only track, when there is one.
    pub fn sole_track(&self) -> Option<&Track> {
        match self.tracks.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Does any track merge by rule?
    pub fn has_merge(&self) -> bool {
        self.tracks.iter().any(|t| t.merge.is_some())
    }
}
