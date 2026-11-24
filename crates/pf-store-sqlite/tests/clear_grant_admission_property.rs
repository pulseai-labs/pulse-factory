//! (`run-state-admission`, property test, on the real store; `r2.s3.w1`): over generated orders of
//! grants, revokes and guarded commits over two projects, with command IDs drawn from a small pool,
//!
//! - the same ID with the same content returns `Replayed` with the first record, and changes
//!   nothing, whatever the grants did in between;
//! - the same ID with other content returns `Conflict` with the first record, and changes nothing;
//! - each project's generations are 1 to n with no gap, and at most one row of a project is live;
//! - a commit that carries `RequireLiveGrant` (audit C6) is `Applied` exactly when, at its place in
//!   the order, its generation is the project's live one, and never after that generation's
//!   revoke; a rejected one writes no admission, so its ID is free again.
//!
//! A rejected commit is not stored, so the model records nothing for it. Failure persistence is
//! off, as in `project_admission_property.rs`, so a run from another directory leaves no
//! `*.proptest-regressions` file.

use std::collections::BTreeMap;

use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry,
    NewAdmission, NewClearGrant, NewProject, ORCHESTRATOR_AGENT, ProjectKind, Store,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const LEDGER: &str = "controller.ledger";
const RUN: &str = "run-1";

#[derive(Clone, Debug)]
enum Op {
    /// `grant clear` of project `p` (1 or 2) under command `cmd` of the pool.
    Grant { cmd: u8, p: u8 },
    /// `grant revoke` of project `p`'s open row, or of generation 1 when it has none.
    Revoke { cmd: u8, p: u8 },
    /// A commit in a run's scope that requires generation `g` of project `p`.
    Guard { cmd: u8, p: u8, g: u8 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..6, 1u8..3).prop_map(|(cmd, p)| Op::Grant { cmd, p }),
        (0u8..6, 1u8..3).prop_map(|(cmd, p)| Op::Revoke { cmd, p }),
        (0u8..6, 1u8..3, 1u8..5).prop_map(|(cmd, p, g)| Op::Guard { cmd, p, g }),
    ]
}

/// What the model remembers of a command it stored.
#[derive(Clone, Debug)]
struct Seen {
    content: String,
    row: AdmissionRow,
}

struct Rig {
    store: SqliteStore,
    _dir: tempfile::TempDir,
    /// For each project, one flag for each generation, in order: is it revoked?
    grants: BTreeMap<String, Vec<bool>>,
    /// The commands the store stored, by scope and command ID.
    seen: BTreeMap<(String, String), Seen>,
}

fn project(p: u8) -> String {
    format!("P-{p}")
}

fn commit(
    scope: &str,
    kind: &str,
    command_id: &str,
    content: &str,
    unit_id: Option<String>,
    effects: Vec<Effect>,
) -> Commit {
    Commit {
        run_id: scope.to_string(),
        command_id: command_id.to_string(),
        content: content.to_string(),
        admission: NewAdmission {
            kind: kind.to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t1".to_string(),
        },
        effects,
        ledger: Some(LedgerEntry {
            ledger_file: LEDGER.to_string(),
            body: format!("{kind} command={command_id}"),
        }),
    }
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("store.sqlite")).unwrap();
        for (i, name) in ["one", "two"].into_iter().enumerate() {
            let c = commit(
                CONTROLLER_SCOPE,
                "project_add",
                &format!("add-{i}"),
                name,
                None,
                vec![Effect::AddProject(NewProject {
                    name: name.to_string(),
                    root: format!("/work/{name}"),
                    created_at: "t0".to_string(),
                    kind: ProjectKind::Folder,
                    remote_key: None,
                })],
            );
            assert!(matches!(
                store.commit(c).unwrap(),
                Committed::Applied { .. }
            ));
        }
        Rig {
            store,
            _dir: dir,
            grants: BTreeMap::new(),
            seen: BTreeMap::new(),
        }
    }

    /// The live generation of a project, by the model.
    fn live(&self, project: &str) -> Option<u64> {
        let rows = self.grants.get(project)?;
        rows.iter()
            .position(|revoked| !revoked)
            .map(|i| u64::try_from(i).unwrap() + 1)
    }

    /// The command of an op, and what the model expects of it when it is new.
    fn build(&self, op: &Op) -> (Commit, bool) {
        match op {
            Op::Grant { cmd, p } => {
                let project = project(*p);
                let generation = self
                    .grants
                    .get(&project)
                    .map_or(1, |rows| u64::try_from(rows.len()).unwrap() + 1);
                let content = serde_json::json!({ "verb": "grant", "project_id": project });
                let c = commit(
                    CONTROLLER_SCOPE,
                    "grant_clear",
                    &format!("c{cmd}"),
                    &content.to_string(),
                    Some(format!("{project}:{generation}")),
                    vec![Effect::GrantClear(NewClearGrant {
                        project_id: project.clone(),
                        generation,
                        agent_id: ORCHESTRATOR_AGENT.to_string(),
                        granted_by: "cli".to_string(),
                        granted_at: "t2".to_string(),
                    })],
                );
                (c, self.live(&project).is_none())
            }
            Op::Revoke { cmd, p } => {
                let project = project(*p);
                let open = self.live(&project);
                let content = serde_json::json!({ "verb": "revoke", "project_id": project });
                let c = commit(
                    CONTROLLER_SCOPE,
                    "grant_revoke",
                    &format!("c{cmd}"),
                    &content.to_string(),
                    Some(project.clone()),
                    vec![Effect::RevokeClearGrant {
                        project_id: project.clone(),
                        generation: open.unwrap_or(1),
                        revoked_by: "cli".to_string(),
                        revoked_at: "t3".to_string(),
                    }],
                );
                (c, open.is_some())
            }
            Op::Guard { cmd, p, g } => {
                let project = project(*p);
                let content = serde_json::json!({
                    "verb": "guard", "project_id": project, "generation": g
                });
                let c = commit(
                    RUN,
                    "settle_probe",
                    &format!("c{cmd}"),
                    &content.to_string(),
                    None,
                    vec![Effect::RequireLiveGrant {
                        project_id: project.clone(),
                        generation: u64::from(*g),
                    }],
                );
                (c, self.live(&project) == Some(u64::from(*g)))
            }
        }
    }

    fn run(&mut self, op: &Op) {
        let (c, applies) = self.build(op);
        let key = (c.run_id.clone(), c.command_id.clone());
        let content = c.content.clone();
        let got = self.store.commit(c).unwrap();
        if let Some(first) = self.seen.get(&key) {
            // A command the store stored: the first record answers, and nothing changes.
            match got {
                Committed::Replayed(row) => {
                    assert_eq!(first.content, content, "{op:?}");
                    assert_eq!(row, first.row, "{op:?}");
                }
                Committed::Conflict(row) => {
                    assert_ne!(first.content, content, "{op:?}");
                    assert_eq!(row, first.row, "{op:?}");
                }
                other => {
                    panic!("a stored command is replayed or in conflict, got {other:?}: {op:?}")
                }
            }
            return;
        }
        match (got, applies) {
            (Committed::Applied { admission, .. }, true) => {
                self.seen.insert(
                    key,
                    Seen {
                        content,
                        row: admission,
                    },
                );
                match op {
                    Op::Grant { p, .. } => {
                        self.grants.entry(project(*p)).or_default().push(false);
                    }
                    Op::Revoke { p, .. } => {
                        let project = project(*p);
                        let at = self.live(&project).unwrap();
                        self.grants.get_mut(&project).unwrap()[usize::try_from(at).unwrap() - 1] =
                            true;
                    }
                    Op::Guard { .. } => {}
                }
            }
            (Committed::Rejected(why), false) => {
                assert!(!why.is_empty() && !why.contains('\n'), "{op:?}: {why}");
                // Nothing was stored: the ID is free, and the model keeps nothing.
                assert_eq!(
                    self.store.admission(&key.0, &key.1).unwrap(),
                    None,
                    "a rejected commit leaves no admission: {op:?}"
                );
            }
            (other, expected) => {
                panic!(
                    "{op:?}: the model expected applied={expected}, the store answered {other:?}"
                )
            }
        }
        self.check();
    }

    /// The store's rows equal the model's, and the invariants hold.
    fn check(&self) {
        let rows = self.store.clear_grants().unwrap();
        for (project, flags) in &self.grants {
            let mine: Vec<_> = rows.iter().filter(|r| &r.project_id == project).collect();
            assert_eq!(mine.len(), flags.len(), "{project}");
            for (i, row) in mine.iter().enumerate() {
                assert_eq!(row.generation, u64::try_from(i).unwrap() + 1, "no gap");
                assert_eq!(row.revoked_at.is_some(), flags[i], "{project} #{}", i + 1);
                assert_eq!(row.revoked_at.is_some(), row.revoked_by.is_some());
            }
            let live = mine.iter().filter(|r| r.revoked_at.is_none()).count();
            assert!(live <= 1, "at most one live row of {project}");
            assert_eq!(
                mine.iter()
                    .find(|r| r.revoked_at.is_none())
                    .map(|r| r.generation),
                self.live(project)
            );
        }
        assert_eq!(
            rows.len(),
            self.grants.values().map(Vec::len).sum::<usize>()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn grants_and_revokes_admit_once_replay_by_id_and_fence_a_guarded_commit(
        ops in prop::collection::vec(op(), 1..40)
    ) {
        let mut rig = Rig::new();
        for op in &ops {
            rig.run(op);
        }
        rig.check();
    }
}
