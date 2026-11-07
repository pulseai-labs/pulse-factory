//! AC-3 (`run-state-admission`, property test, on the real store): over generated sequences of
//! project add and remove commands with command IDs drawn from a small pool,
//!
//! - the same ID with the same content returns `Replayed` with the first record;
//! - the same ID with other content returns `Conflict` and changes nothing;
//! - the live set equals a model's live set;
//! - no project id is ever issued twice;
//! - the `@controller` outbox holds exactly one line for each new admitted or refused command, and
//!   none for a replay or a conflict.
//!
//! A command the store rejects (a duplicate live root or name, an unknown or removed id) is made
//! a refused admission in a second commit, as the controller does it.
//!
//! Failure persistence is off, as in `crates/pf-engine/tests/park_clear_property.rs:1-7`, so a run
//! from another directory leaves no `*.proptest-regressions` file.

use std::collections::BTreeMap;

use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry,
    NEW_ID, NewAdmission, NewProject, Store,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const LEDGER: &str = "controller.ledger";

#[derive(Clone, Debug)]
enum Op {
    /// Add a project: a command ID of the pool, a name and a root of small pools.
    Add { cmd: u8, name: u8, root: u8 },
    /// Remove a project: a command ID of the pool and a project number of 1..=6.
    Remove { cmd: u8, n: u8 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..5, 0u8..3, 0u8..3).prop_map(|(cmd, name, root)| Op::Add { cmd, name, root }),
        (0u8..5, 1u8..7).prop_map(|(cmd, n)| Op::Remove { cmd, n }),
    ]
}

/// What the model knows of one project.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Model {
    name: String,
    root: String,
    live: bool,
}

/// What the model remembers of a command it has seen.
#[derive(Clone, Debug)]
struct Seen {
    content: String,
    row: AdmissionRow,
}

struct Rig {
    store: SqliteStore,
    _dir: tempfile::TempDir,
    projects: BTreeMap<String, Model>,
    seen: BTreeMap<String, Seen>,
    /// Every id the store issued, in order.
    issued: Vec<String>,
    /// The number of new commands the model expects an outbox line for.
    new_commands: usize,
}

fn content_add(name: &str, root: &str) -> String {
    serde_json::json!({ "name": name, "root": root }).to_string()
}

fn content_remove(id: &str) -> String {
    serde_json::json!({ "id": id }).to_string()
}

fn commit(
    kind: &str,
    command_id: &str,
    content: &str,
    outcome: AdmissionOutcome,
    reason: Option<String>,
    unit_id: Option<String>,
    effects: Vec<Effect>,
) -> Commit {
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: content.to_string(),
        admission: NewAdmission {
            kind: kind.to_string(),
            outcome,
            reason: reason.clone(),
            unit_id,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t".to_string(),
        },
        effects,
        ledger: Some(LedgerEntry {
            ledger_file: LEDGER.to_string(),
            body: format!(
                "{kind} {} command={command_id}",
                reason.unwrap_or_else(|| NEW_ID.to_string())
            ),
        }),
    }
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        Rig {
            store: SqliteStore::open(dir.path().join("store.sqlite")).unwrap(),
            _dir: dir,
            projects: BTreeMap::new(),
            seen: BTreeMap::new(),
            issued: Vec::new(),
            new_commands: 0,
        }
    }

    fn live_model(&self) -> Vec<(String, String, String)> {
        self.projects
            .iter()
            .filter(|(_, m)| m.live)
            .map(|(id, m)| (id.clone(), m.name.clone(), m.root.clone()))
            .collect()
    }

    /// Run one command through the store as the controller does, and check the answer against
    /// the model.
    fn run(&mut self, op: &Op) {
        let (kind, cmd, content) = match op {
            Op::Add { cmd, name, root } => (
                "project_add",
                format!("cmd-{cmd}"),
                content_add(&format!("name-{name}"), &format!("/root/{root}")),
            ),
            Op::Remove { cmd, n } => (
                "project_remove",
                format!("cmd-{cmd}"),
                content_remove(&format!("P-{n}")),
            ),
        };

        // Does this command pass the model's own rules right now?
        let verdict: Result<(), String> = match op {
            Op::Add { name, root, .. } => {
                let (name, root) = (format!("name-{name}"), format!("/root/{root}"));
                if self.projects.values().any(|m| m.live && m.root == root) {
                    Err("root".to_string())
                } else if self.projects.values().any(|m| m.live && m.name == name) {
                    Err("name".to_string())
                } else {
                    Ok(())
                }
            }
            Op::Remove { n, .. } => match self.projects.get(&format!("P-{n}")) {
                Some(m) if m.live => Ok(()),
                _ => Err("remove".to_string()),
            },
        };

        let before_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        let new_before = self.new_commands;
        let first = match op {
            Op::Add { name, root, .. } => commit(
                kind,
                &cmd,
                &content,
                AdmissionOutcome::Admitted,
                None,
                Some(NEW_ID.to_string()),
                vec![Effect::AddProject(NewProject {
                    name: format!("name-{name}"),
                    root: format!("/root/{root}"),
                    created_at: "t".to_string(),
                })],
            ),
            Op::Remove { n, .. } => commit(
                kind,
                &cmd,
                &content,
                AdmissionOutcome::Admitted,
                None,
                Some(format!("P-{n}")),
                vec![Effect::RemoveProject {
                    id: format!("P-{n}"),
                    removed_at: "t".to_string(),
                }],
            ),
        };
        let answer = self.store.commit(first).unwrap();

        let known = self.seen.get(&cmd).cloned();
        match (known, answer) {
            // The ID was seen: a replay with the same content, a conflict with other content.
            (Some(seen), Committed::Replayed(row)) => {
                assert_eq!(seen.content, content, "a replay needs the same content");
                assert_eq!(row, seen.row, "a replay returns the first record");
            }
            (Some(seen), Committed::Conflict(row)) => {
                assert_ne!(seen.content, content, "a conflict needs other content");
                assert_eq!(row, seen.row, "a conflict names the first record");
            }
            (Some(seen), other) => panic!("a seen ID {cmd} gave {other:?} (first: {seen:?})"),
            (None, Committed::Replayed(_) | Committed::Conflict(_)) => {
                panic!("an unseen ID {cmd} cannot be a replay or a conflict")
            }
            // A new command that passes: applied.
            (None, Committed::Applied { admission, ledger }) => {
                assert!(
                    verdict.is_ok(),
                    "the store admitted what the model refuses: {op:?}"
                );
                assert_eq!(admission.outcome, AdmissionOutcome::Admitted);
                assert!(ledger.is_some());
                match op {
                    Op::Add { name, root, .. } => {
                        let id = admission.unit_id.clone().expect("the assigned id");
                        assert!(!self.issued.contains(&id), "{id} was issued twice");
                        self.issued.push(id.clone());
                        self.projects.insert(
                            id,
                            Model {
                                name: format!("name-{name}"),
                                root: format!("/root/{root}"),
                                live: true,
                            },
                        );
                    }
                    Op::Remove { n, .. } => {
                        self.projects.get_mut(&format!("P-{n}")).unwrap().live = false;
                    }
                }
                self.new_commands += 1;
                self.seen.insert(
                    cmd,
                    Seen {
                        content,
                        row: admission,
                    },
                );
            }
            // A new command the store rejects: the controller records a refusal.
            (None, Committed::Rejected(why)) => {
                assert!(
                    verdict.is_err(),
                    "the store refused what the model admits: {op:?} {why}"
                );
                assert_eq!(
                    self.store.unwritten_ledger_lines(LEDGER).unwrap().len(),
                    before_lines,
                    "a rejection writes no outbox line"
                );
                assert_eq!(self.store.admission(CONTROLLER_SCOPE, &cmd).unwrap(), None);
                let refusal = commit(
                    kind,
                    &cmd,
                    &content,
                    AdmissionOutcome::Refused,
                    Some(format!("refused: {why}")),
                    None,
                    vec![],
                );
                match self.store.commit(refusal).unwrap() {
                    Committed::Applied { admission, .. } => {
                        assert_eq!(admission.outcome, AdmissionOutcome::Refused);
                        self.new_commands += 1;
                        self.seen.insert(
                            cmd,
                            Seen {
                                content,
                                row: admission,
                            },
                        );
                    }
                    other => panic!("the refusal was not applied: {other:?}"),
                }
            }
        }

        // A replay or a conflict changes nothing. Every case: the live set is the model's.
        let live: Vec<(String, String, String)> = self
            .store
            .projects()
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.name, p.root))
            .collect();
        assert_eq!(live, self.live_model(), "the live set equals the model's");
        let after_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        assert_eq!(
            after_lines - before_lines,
            self.new_commands - new_before,
            "one outbox line per new command, none for a replay or a conflict"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn project_commands_admit_once_replay_by_id_and_keep_the_live_set(
        ops in prop::collection::vec(op(), 1..40)
    ) {
        let mut rig = Rig::new();
        for op in &ops {
            rig.run(op);
        }
        // The outbox holds one line for each new command, in all.
        let lines = rig.store.unwritten_ledger_lines(LEDGER).unwrap();
        prop_assert_eq!(lines.len(), rig.new_commands);
        prop_assert!(lines.iter().all(|l| l.run_id == CONTROLLER_SCOPE));
        // The line numbers run 1, 2, 3 with no gap.
        let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
        let want: Vec<u64> = (1..=lines.len() as u64).collect();
        prop_assert_eq!(seqs, want);
        // The admissions of the scope are the new commands, and each ID is unique.
        let admissions = rig.store.admissions(CONTROLLER_SCOPE).unwrap();
        prop_assert_eq!(admissions.len(), rig.new_commands);
        let mut ids: Vec<&str> = admissions.iter().map(|a| a.command_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        prop_assert_eq!(ids.len(), admissions.len());
        // Every id the store issued is unique.
        let mut issued = rig.issued.clone();
        issued.sort();
        issued.dedup();
        prop_assert_eq!(issued.len(), rig.issued.len());
    }
}
