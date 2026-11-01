//! From the parsed file to the value the engine runs.
//!
//! The file holds more than the engine needs, such as the plugin list, the unit sources and the
//! seeds. The engine gets the tracks, the roots and the settings. Each stage becomes a
//! [`StageDef`] whose `settings` map holds the templates that `start_stage` renders into the
//! launch plan (`docs/contracts/launch-settings.md`).

use std::collections::BTreeMap;

use pf_engine::port::agent::RuleSet;
use pf_engine::{MergeDef, StageDef, Track, Workflow};

use crate::file::{StageSpec, TrackDef, WorkflowFile};

impl WorkflowFile {
    /// Build the engine's workflow from the file. The file was checked when it was parsed, so
    /// this cannot fail.
    pub fn to_workflow(&self) -> Workflow {
        Workflow {
            name: self.name.clone(),
            tracks: self.tracks.iter().map(|t| self.convert_track(t)).collect(),
            settings: self.settings,
            roots: self.roots.clone(),
        }
    }

    fn convert_track(&self, track: &TrackDef) -> Track {
        Track {
            name: track.name.clone(),
            stages: track.stages.iter().map(|s| self.convert_stage(s)).collect(),
            merge: track.merge.as_ref().map(|m| MergeDef {
                rule: m.rule,
                repo: m.repo.clone(),
                base: m.base.clone(),
                head_check: m.head_check,
            }),
        }
    }

    fn convert_stage(&self, stage: &StageSpec) -> StageDef {
        // The parser checked that the role exists.
        let role = self.role(&stage.role);
        let mut def = StageDef::new(
            &stage.name,
            &stage.role,
            role.map_or("", |r| r.model.as_str()),
        );
        if let Some(role) = role {
            def.runtime = role.runtime.clone();
        }
        def.rules = RuleSet {
            name: stage.role.clone(),
            prompts_off: true,
            deny: Vec::new(),
        };
        def.settings = stage_settings(stage);
        def
    }
}

/// The launch settings of a stage, as templates.
fn stage_settings(stage: &StageSpec) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("cwd".to_string(), stage.cwd.clone());
    out.insert("message".to_string(), stage.message.clone());
    if !stage.brief.is_empty() {
        out.insert("brief".to_string(), stage.brief.join(","));
    }
    if !stage.skills.is_empty() {
        out.insert("skills".to_string(), stage.skills.join(","));
    }
    out.insert(
        "first_turn_seconds".to_string(),
        stage.first_turn_seconds.to_string(),
    );
    out.insert("stall_minutes".to_string(), stage.stall_minutes.to_string());
    // `env.<NAME>` and the keys of `STAGE_SETTING_KEYS`. The parser admits no other key, so these
    // never replace a key above.
    out.extend(stage.settings.clone());
    out
}
