//! `run-state-admission`'s property test for a run's project, on the real store (ADR-0003,
//! ADR-0015; `r1.s4.w2`).
//!
//! Generated sequences of `Link { cmd, run, project }` (a `SetRunProject` admission under
//! `@controller`), `Put { run, halted }` (the engine's own `PutRun` in the run's scope, which carries
//! no project) and `Remove` (project `P-3` is removed once, mid-sequence), over small pools: command
//! IDs (4), runs (3) and projects (3). A link the store rejects becomes a refused admission in a
//! second commit, as the controller does it. After every step:
//!
//! - a run's project, once set, never changes, and it equals a model's value;
//! - a project that a run holds was live when it was set;
//! - the same ID with the same content returns `Replayed` with the first record and changes nothing;
//!   with other content it returns `Conflict` and changes nothing;
//! - every admitted `run_project` transition has its command ID, and the admitted count equals the
//!   model's.
//!
//! Failure persistence is off, as in `project_admission_property.rs`, so a run from another
//! directory leaves no `*.proptest-regressions` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{add_project, commit, open, remove_project};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Committed, Effect, ProjectKind, RunRow, RunState, Store,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const RUNS: [&str; 3] = ["run-1", "run-2", "run-3"];
const PROJECTS: [&str; 3] = ["P-1", "P-2", "P-3"];

#[derive(Clone, Debug)]
enum Op {
    Link { cmd: u8, run: u8, project: u8 },
    Put { run: u8, halted: bool },
    Remove,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u8..4, 0u8..3, 0u8..3).prop_map(|(cmd, run, project)| Op::Link { cmd, run, project }),
        3 => (0u8..3, any::<bool>()).prop_map(|(run, halted)| Op::Put { run, halted }),
        1 => Just(Op::Remove),
    ]
}

fn row(id: &str, halted: bool) -> RunRow {
    RunRow {
        id: id.to_string(),
        workflow: "wf".to_string(),
        ledger_file: "/l.md".to_string(),
        state: if halted {
            RunState::Ended
        } else {
            RunState::Active
        },
        halted_reason: halted.then(|| "stop".to_string()),
        project_id: None,
        created_at: "t1".to_string(),
        updated_at: "t2".to_string(),
    }
}

fn run_commit(run: &str, command_id: &str, effect: Effect) -> pf_engine::port::store::Commit {
    let mut c = commit(
        "put_run",
        command_id,
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![effect],
        None,
    );
    c.run_id = run.to_string();
    c
}

fn link_commit(
    cmd: &str,
    run: &str,
    project: &str,
    outcome: AdmissionOutcome,
) -> pf_engine::port::store::Commit {
    let content = format!("{{\"project\":\"{project}\",\"run\":\"{run}\"}}");
    match outcome {
        AdmissionOutcome::Admitted => commit(
            "run_project",
            cmd,
            &content,
            outcome,
            None,
            Some(run),
            vec![Effect::SetRunProject {
                run_id: run.to_string(),
                project_id: project.to_string(),
            }],
            Some(format!(
                "run_project {run} project={project} admitted command={cmd}"
            )),
        ),
        AdmissionOutcome::Refused => commit(
            "run_project",
            cmd,
            &content,
            outcome,
            Some("refused"),
            None,
            vec![],
            Some(format!("run_project refused command={cmd}")),
        ),
    }
}

struct Model {
    projects: BTreeMap<&'static str, Option<&'static str>>,
    removed: BTreeSet<&'static str>,
    /// The projects each run held when it was linked, and whether the project was live then.
    set_live: Vec<(&'static str, &'static str, bool)>,
    cmds: BTreeMap<String, (String, AdmissionOutcome)>,
    admitted: usize,
    put_n: usize,
}

fn project_of(store: &SqliteStore, run: &str) -> Option<String> {
    store.run(run).unwrap().unwrap().project_id
}

fn snapshot(store: &SqliteStore) -> (Vec<Option<String>>, usize) {
    (
        RUNS.iter().map(|r| project_of(store, r)).collect(),
        store.admissions(CONTROLLER_SCOPE).unwrap().len(),
    )
}

fn setup() -> (tempfile::TempDir, SqliteStore) {
    let (d, store) = open();
    for (i, p) in PROJECTS.iter().enumerate() {
        add_project(
            &store,
            &format!("add-{i}"),
            &format!("p{i}"),
            &format!("/r/p{i}"),
            ProjectKind::Folder,
            None,
        );
        assert_eq!(*p, format!("P-{}", i + 1));
    }
    for r in RUNS {
        let c = run_commit(r, &format!("open-{r}"), Effect::PutRun(row(r, false)));
        assert!(matches!(
            store.commit(c).unwrap(),
            Committed::Applied { .. }
        ));
    }
    (d, store)
}

fn check_after(store: &SqliteStore, m: &Model) {
    for (i, r) in RUNS.iter().enumerate() {
        let want = m.projects[r].map(str::to_string);
        assert_eq!(project_of(store, r), want, "the project of {r}");
        let _ = i;
    }
    for (run, project, live) in &m.set_live {
        assert!(*live, "{project} was set on {run} while it was not live");
    }
    let admitted: Vec<_> = store
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "run_project" && a.outcome == AdmissionOutcome::Admitted)
        .collect();
    assert_eq!(admitted.len(), m.admitted, "the admitted transitions");
    assert!(admitted.iter().all(|a| !a.command_id.is_empty()));
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn run_project_links_admit_once_replay_by_id_and_never_change_a_set_project(
        ops in prop::collection::vec(op(), 1..40)
    ) {
        let (_d, store) = setup();
        let mut m = Model {
            projects: RUNS.iter().map(|r| (*r, None)).collect(),
            removed: BTreeSet::new(),
            set_live: Vec::new(),
            cmds: BTreeMap::new(),
            admitted: 0,
            put_n: 0,
        };
        for op in ops {
            match op {
                Op::Remove => {
                    if m.removed.insert("P-3") {
                        let c = remove_project(&store, "rm-3", "P-3");
                        assert!(matches!(c, Committed::Applied { .. }));
                    }
                }
                Op::Put { run, halted } => {
                    let run = RUNS[usize::from(run % 3)];
                    m.put_n += 1;
                    let c = run_commit(run, &format!("put-{}", m.put_n), Effect::PutRun(row(run, halted)));
                    assert!(matches!(store.commit(c).unwrap(), Committed::Applied { .. }));
                }
                Op::Link { cmd, run, project } => {
                    let cmd_id = format!("link-{}", cmd % 4);
                    let run = RUNS[usize::from(run % 3)];
                    let project = PROJECTS[usize::from(project % 3)];
                    let content = format!("{{\"project\":\"{project}\",\"run\":\"{run}\"}}");
                    let before = snapshot(&store);
                    if let Some((first, outcome)) = m.cmds.get(&cmd_id).cloned() {
                        // The command was seen: the store answers its record, and changes nothing.
                        let c = store
                            .commit(link_commit(&cmd_id, run, project, AdmissionOutcome::Admitted))
                            .unwrap();
                        if first == content {
                            match c {
                                Committed::Replayed(a) => {
                                    assert_eq!(a.command_id, cmd_id);
                                    assert_eq!(a.outcome, outcome);
                                    assert_eq!(a.content, first);
                                }
                                other => panic!("a replay answered {other:?}"),
                            }
                        } else {
                            assert!(matches!(c, Committed::Conflict(_)), "a conflict answered {c:?}");
                        }
                        assert_eq!(snapshot(&store), before, "a replay or a conflict changed the store");
                    } else {
                        let live = !m.removed.contains(project);
                        let held = m.projects[run];
                        // The same pair again is a no-op the store admits, even after the project was removed (the
                        // project was live when it was set). Only a first link needs a live project.
                        let ok = held == Some(project) || (live && held.is_none());
                        let c = store
                            .commit(link_commit(&cmd_id, run, project, AdmissionOutcome::Admitted))
                            .unwrap();
                        match (ok, c) {
                            (true, Committed::Applied { .. }) => {
                                if held.is_none() {
                                    m.set_live.push((run, project, live));
                                }
                                m.projects.insert(run, Some(project));
                                m.admitted += 1;
                                m.cmds.insert(cmd_id, (content, AdmissionOutcome::Admitted));
                            }
                            (false, Committed::Rejected(_)) => {
                                assert_eq!(snapshot(&store), before, "a rejected link wrote something");
                                let refused = store
                                    .commit(link_commit(&cmd_id, run, project, AdmissionOutcome::Refused))
                                    .unwrap();
                                assert!(matches!(refused, Committed::Applied { .. }));
                                m.cmds.insert(cmd_id, (content, AdmissionOutcome::Refused));
                            }
                            (want, got) => panic!("a link that should be {want} answered {got:?}"),
                        }
                    }
                }
            }
            check_after(&store, &m);
        }
    }
}
