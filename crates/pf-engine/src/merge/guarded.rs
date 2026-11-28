//! The guarded compare of a conflict resolution (ADR-0005, amended; ADR-0004, amended).
//!
//! A track may declare a stage that a merge conflict sends the unit to (`merge.on_conflict`) and a
//! list of guarded globs (`merge.guarded`). When the conflict stage is done, the engine compares the
//! paths the resolution changed with the globs. A match, or a read that fails, parks the unit above
//! the line with the cause `resolution_changed_guarded`. Otherwise the unit goes on to the track's
//! `then` stage, so a resolution is reviewed before the merge rule runs.
//!
//! The compare is a pure function of three trees ([`resolution_changes`]) and a hand-written glob
//! grammar ([`glob_match`]): ADR-0008 allows no new dependency for it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::admission::{Admission, Command, CommandBody, EngineError, body_of_content};
use crate::claim::{Act, VERB_SETTLE};
use crate::engine::Engine;
use crate::port::forge::PrRef;
use crate::status::Status;
use crate::unit::Signal;

/// The most paths or globs a resolution keeps in its stored verdict. The count is kept.
pub const LISTED_MAX: usize = 20;

/// A list cut at [`LISTED_MAX`], with the count of the whole list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub items: Vec<String>,
    pub total: usize,
}

impl Listed {
    /// The first [`LISTED_MAX`] of `all`, and the count of all of them.
    pub fn cut(all: &[String]) -> Listed {
        Listed {
            items: all.iter().take(LISTED_MAX).cloned().collect(),
            total: all.len(),
        }
    }
}

/// What the guarded compare found. It rides in the `Signal` command that ends the conflict stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Resolution {
    /// The resolution changed no guarded path.
    Clean { paths: Listed },
    /// The resolution changed a path that a guarded glob matches.
    Guarded { paths: Listed, globs: Listed },
    /// A read failed, so nothing was compared. This parks the unit too.
    Unread { why: String },
}

impl Resolution {
    /// The facts of the park that a `Guarded` or an `Unread` verdict opens, or `None` for `Clean`.
    pub fn park_facts(&self) -> Option<Value> {
        match self {
            Resolution::Clean { .. } => None,
            Resolution::Guarded { paths, globs } => Some(json!({
                "paths": paths.items,
                "paths_total": paths.total,
                "globs": globs.items,
                "globs_total": globs.total,
            })),
            Resolution::Unread { why } => Some(json!({ "why": why })),
        }
    }

    /// One line for the ledger.
    pub fn describe(&self) -> String {
        match self {
            Resolution::Clean { paths } => {
                format!("resolution changed {} path(s), none guarded", paths.total)
            }
            Resolution::Guarded { paths, globs } => format!(
                "resolution changed {} guarded path(s): {} (globs: {})",
                paths.total,
                paths.items.join(", "),
                globs.items.join(", ")
            ),
            Resolution::Unread { why } => format!("resolution not compared: {why}"),
        }
    }
}

/// The paths that a resolution changed. `old` is the tree of the PR's head when the conflict was
/// settled, `new` the tree of its head when the conflict stage is done, `base` the tree of the tip
/// of the PR's base. A path is changed when its blob at `new` differs from its blob at `old` and from
/// its blob at `base`. A path that is missing counts as one more value. So a path that only took the
/// base's change is not changed, a path that both sides changed is, and a deleted path is.
///
/// Stated limit: a resolution that keeps the PR's old content for a path that the base changed reads
/// as not changed here. The `then` stage's review sees it in the PR's diff.
pub fn resolution_changes(
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
    base: &BTreeMap<String, String>,
) -> Vec<String> {
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).chain(base.keys()).collect();
    paths
        .into_iter()
        .filter(|p| {
            let at_new = new.get(*p);
            at_new != old.get(*p) && at_new != base.get(*p)
        })
        .cloned()
        .collect()
}

/// Does `pattern` match `path`? The grammar: `*` matches within one path segment, `**` as a whole
/// segment matches any number of whole segments (none too), `?` matches one character, and
/// everything else is literal. A path is relative and uses `/`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    segments_match(&pat, &segs)
}

fn segments_match(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|skip| segments_match(rest, &segs[skip..])),
        Some((first, rest)) => match segs.split_first() {
            Some((seg, tail)) => segment_match(first, seg) && segments_match(rest, tail),
            None => false,
        },
    }
}

/// `*` and `?` inside one segment.
fn segment_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Row by row: `reach[j]` is true when the first `i` pattern characters match the first `j` text
    // characters.
    let mut reach = vec![false; t.len() + 1];
    reach[0] = true;
    for pc in &p {
        let mut next = vec![false; t.len() + 1];
        if *pc == '*' {
            let mut open = false;
            for j in 0..=t.len() {
                open = open || reach[j];
                next[j] = open;
            }
        } else {
            for j in 0..t.len() {
                next[j + 1] = reach[j] && (*pc == '?' || *pc == t[j]);
            }
        }
        reach = next;
    }
    reach[t.len()]
}

/// The compare's verdict for the changed `paths` against the track's `globs`.
pub fn verdict(paths: &[String], globs: &[String]) -> Resolution {
    let hit_paths: Vec<String> = paths
        .iter()
        .filter(|p| globs.iter().any(|g| glob_match(g, p)))
        .cloned()
        .collect();
    if hit_paths.is_empty() {
        return Resolution::Clean {
            paths: Listed::cut(paths),
        };
    }
    let hit_globs: Vec<String> = globs
        .iter()
        .filter(|g| hit_paths.iter().any(|p| glob_match(g, p)))
        .cloned()
        .collect();
    Resolution::Guarded {
        paths: Listed::cut(&hit_paths),
        globs: Listed::cut(&hit_globs),
    }
}

impl Engine {
    /// The head SHA that the unit's newest `resolve_conflict` settle stored in its answer.
    fn stored_conflict_head(&self, unit_id: &str) -> Result<Option<String>, EngineError> {
        let view = self.view()?;
        Ok(view
            .requests
            .iter()
            .rev()
            .filter(|r| r.unit_id.as_deref() == Some(unit_id))
            .filter_map(|r| serde_json::from_str::<Value>(r.answer.as_deref()?).ok())
            .find(|a| {
                a.get("verb").and_then(Value::as_str) == Some(VERB_SETTLE)
                    && a.get("act").and_then(Value::as_str) == Some(Act::ResolveConflict.as_str())
            })
            .and_then(|a| {
                a.get("head_sha")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }))
    }

    /// The compare of the conflict stage's end, as far as a read allows.
    fn compare_resolution(
        &self,
        unit_id: &str,
        pr: Option<&PrRef>,
        globs: &[String],
    ) -> Resolution {
        let unread = |why: String| Resolution::Unread { why };
        let old_sha = match self.stored_conflict_head(unit_id) {
            Ok(Some(sha)) => sha,
            Ok(None) => return unread("no recorded head".to_string()),
            Err(e) => return unread(format!("the stored head cannot be read: {e}")),
        };
        let Some(pr) = pr else {
            return unread("no PR is recorded for the unit".to_string());
        };
        let status = match self.forge.pr_status(pr) {
            Ok(s) => s,
            Err(e) => return unread(format!("the PR's head cannot be read: {e}")),
        };
        let tip = match self.forge.branch_tip(&pr.repo, &status.base) {
            Ok(t) => t,
            Err(e) => return unread(format!("the tip of the base cannot be read: {e}")),
        };
        let read = |sha: &str| {
            self.forge
                .tree(&pr.repo, sha)
                .map_err(|e| format!("the files of {sha} cannot be read: {e}"))
        };
        let (old, new, base) = match (read(&old_sha), read(&status.sha), read(&tip)) {
            (Ok(old), Ok(new), Ok(base)) => (old, new, base),
            (Err(why), _, _) | (_, Err(why), _) | (_, _, Err(why)) => return unread(why),
        };
        verdict(&resolution_changes(&old, &new, &base), globs)
    }

    /// End the unit's conflict stage `stage` with `signal`. A signal that succeeded carries the
    /// verdict of the guarded compare, read outside the lock; a read that fails is an `Unread`
    /// verdict, which parks the unit. A failed signal fails the unit, as on every stage.
    pub fn conflict_done(
        &self,
        unit_id: &str,
        stage: &str,
        signal: Signal,
        pr: Option<PrRef>,
    ) -> Result<Admission, EngineError> {
        if !signal.is_success() {
            return self.signal(unit_id, stage, signal, pr);
        }
        let view = self.view()?;
        let unit = view
            .node(unit_id)
            .filter(|n| n.kind == crate::port::store::NodeKind::Unit)
            .ok_or_else(|| EngineError::Rejected(format!("unit {unit_id} is unknown")))?;
        if unit.status != Status::Running {
            return Err(EngineError::Rejected(format!(
                "unit {unit_id} is {}, not running",
                unit.status.as_str()
            )));
        }
        let detail = view.unit_detail(unit).map_err(EngineError::Rejected)?;
        let track = self.cfg.workflow.track(&detail.track).ok_or_else(|| {
            EngineError::Rejected(crate::admission::unknown_track(unit_id, &detail.track))
        })?;
        let globs = track
            .merge
            .as_ref()
            .map(|m| m.guarded.clone())
            .unwrap_or_default();
        let node = view
            .stages_of(unit_id)
            .into_iter()
            .rfind(|n| n.detail == stage)
            .ok_or_else(|| {
                EngineError::Rejected(format!("unit {unit_id} has no stage {stage} yet"))
            })?;
        let known = pr.clone().or_else(|| detail.pr.clone());
        let resolution = self.compare_resolution(unit_id, known.as_ref(), &globs);
        self.submit(Command {
            id: format!("signal:{}", node.id),
            body: CommandBody::Signal {
                unit_id: unit_id.to_string(),
                stage: stage.to_string(),
                signal,
                pr,
                resolution: Some(resolution),
            },
        })
    }
}

/// The verdict a stored `Signal` carries, read back from its admission.
pub fn stored_resolution(content: &str) -> Option<Resolution> {
    match body_of_content(content)? {
        CommandBody::Signal { resolution, .. } => resolution,
        _ => None,
    }
}
