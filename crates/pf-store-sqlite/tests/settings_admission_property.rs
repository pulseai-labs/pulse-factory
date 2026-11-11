//! `run-state-admission`'s property test for the scoped settings, on the real store (ADR-0016).
//!
//! Generated sequences of `Set { cmd, scope, key, value }` and `Unset { cmd, scope, key }` over small
//! pools: command IDs (5), scopes (the server, `P-1`, `orchestrator`, `P-1` with `orchestrator`, a
//! removed `P-2`, an unknown agent), two keys and three values. A command the store rejects becomes
//! a refused admission in a second commit, as the controller does it. After every step:
//!
//! - the same ID with the same content returns `Replayed` with the first record and changes nothing;
//!   with other content it returns `Conflict` and changes nothing;
//! - the table equals a model's map;
//! - for every context (each project or none, each agent or none) the first row of `setting_chain`
//!   equals the model's most specific value;
//! - the `@controller` outbox holds exactly one line for each new admitted or refused command, and
//!   none for a replay or a conflict.
//!
//! Failure persistence is off, as in `project_admission_property.rs`, so a run from another
//! directory leaves no `*.proptest-regressions` file.

mod common;

use std::collections::BTreeMap;

use common::{LEDGER, add_project, commit, open, remove_project};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Committed, Effect, ORCHESTRATOR_AGENT, ProjectKind,
    SettingRow, SettingScope, Store,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const KEYS: [&str; 2] = ["chat.self_turn_cap", "chat.deny_commands"];
const VALUES: [&str; 3] = ["1", "2", "[\"x\"]"];

/// The six scopes of the pool, by index.
fn pool_scope(n: u8) -> SettingScope {
    match n % 6 {
        0 => SettingScope::server(),
        1 => SettingScope::new(Some("P-1"), None),
        2 => SettingScope::new(None, Some(ORCHESTRATOR_AGENT)),
        3 => SettingScope::new(Some("P-1"), Some(ORCHESTRATOR_AGENT)),
        4 => SettingScope::new(Some("P-2"), None),
        _ => SettingScope::new(None, Some("ghost")),
    }
}

/// Does the scope name only a live project and a known agent?
fn scope_is_valid(n: u8) -> bool {
    n % 6 < 4
}

#[derive(Clone, Debug)]
enum Op {
    Set {
        cmd: u8,
        scope: u8,
        key: u8,
        value: u8,
    },
    Unset {
        cmd: u8,
        scope: u8,
        key: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..5, 0u8..6, 0u8..2, 0u8..3).prop_map(|(cmd, scope, key, value)| Op::Set {
            cmd,
            scope,
            key,
            value
        }),
        (0u8..5, 0u8..6, 0u8..2).prop_map(|(cmd, scope, key)| Op::Unset { cmd, scope, key }),
    ]
}

type ModelKey = (Option<String>, Option<String>, String);

struct Rig {
    _dir: tempfile::TempDir,
    store: SqliteStore,
    /// The stored map: scope and key to value.
    model: BTreeMap<ModelKey, String>,
    /// The first content of each command ID, and whether it was admitted.
    seen: BTreeMap<String, String>,
    new_commands: usize,
    base_lines: usize,
}

impl Rig {
    fn new() -> Rig {
        let (dir, store) = open();
        add_project(
            &store,
            "add-1",
            "alpha",
            "/work/alpha",
            ProjectKind::Folder,
            None,
        );
        add_project(
            &store,
            "add-2",
            "beta",
            "/work/beta",
            ProjectKind::Folder,
            None,
        );
        let removed = remove_project(&store, "rm-2", "P-2");
        assert!(matches!(removed, Committed::Applied { .. }));
        let base_lines = store.unwritten_ledger_lines(LEDGER).unwrap().len();
        Rig {
            _dir: dir,
            store,
            model: BTreeMap::new(),
            seen: BTreeMap::new(),
            new_commands: 0,
            base_lines,
        }
    }

    fn run(&mut self, op: &Op) {
        let (cmd, scope_n, key_n, value) = match op {
            Op::Set {
                cmd,
                scope,
                key,
                value,
            } => (*cmd, *scope, *key, Some(*value)),
            Op::Unset { cmd, scope, key } => (*cmd, *scope, *key, None),
        };
        let id = format!("cmd-{cmd}");
        let scope = pool_scope(scope_n);
        let key = KEYS[usize::from(key_n % 2)];
        let kind = if value.is_some() {
            "setting_set"
        } else {
            "setting_unset"
        };
        let content = match value {
            Some(v) => format!(
                "{{\"key\":\"{key}\",\"scope\":\"{}\",\"value\":{}}}",
                scope.text(),
                VALUES[usize::from(v % 3)]
            ),
            None => format!("{{\"key\":\"{key}\",\"scope\":\"{}\"}}", scope.text()),
        };
        let model_key: ModelKey = (
            scope.project_id.clone(),
            scope.agent_id.clone(),
            key.to_string(),
        );
        let valid = match value {
            Some(_) => scope_is_valid(scope_n),
            None => self.model.contains_key(&model_key),
        };
        let effect = match value {
            Some(v) => Effect::SetSetting(SettingRow {
                scope: scope.clone(),
                key: key.to_string(),
                value: VALUES[usize::from(v % 3)].to_string(),
                set_by: "cli".to_string(),
                set_at: "t1".to_string(),
            }),
            None => Effect::UnsetSetting {
                scope: scope.clone(),
                key: key.to_string(),
            },
        };
        let before_rows = self.store.setting_rows().unwrap();
        let before_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        let got = self
            .store
            .commit(commit(
                kind,
                &id,
                &content,
                AdmissionOutcome::Admitted,
                None,
                Some(&format!("{key}@{}", scope.text())),
                vec![effect],
                Some(format!("{kind} {key} admitted command={id}")),
            ))
            .unwrap();
        match self.seen.get(&id).cloned() {
            Some(first) => {
                // An ID the store knows: a replay of the same content, a conflict of other content.
                if first == content {
                    assert!(matches!(got, Committed::Replayed(_)), "{got:?}");
                } else {
                    assert!(matches!(got, Committed::Conflict(_)), "{got:?}");
                }
                assert_eq!(self.store.setting_rows().unwrap(), before_rows);
            }
            None => {
                self.seen.insert(id.clone(), content.clone());
                if valid {
                    assert!(matches!(got, Committed::Applied { .. }), "{got:?}");
                    match value {
                        Some(v) => {
                            self.model
                                .insert(model_key, VALUES[usize::from(v % 3)].to_string());
                        }
                        None => {
                            self.model.remove(&model_key);
                        }
                    }
                    self.new_commands += 1;
                } else {
                    assert!(matches!(got, Committed::Rejected(_)), "{got:?}");
                    // Nothing was written. The controller commits a refused admission for it.
                    assert_eq!(self.store.setting_rows().unwrap(), before_rows);
                    let refused = self
                        .store
                        .commit(commit(
                            kind,
                            &id,
                            &content,
                            AdmissionOutcome::Refused,
                            Some("refused"),
                            None,
                            Vec::new(),
                            Some(format!("{kind} refused reason=\"refused\" command={id}")),
                        ))
                        .unwrap();
                    assert!(matches!(refused, Committed::Applied { .. }), "{refused:?}");
                    self.new_commands += 1;
                }
            }
        }
        let after_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        let was_new = !matches!(got, Committed::Replayed(_) | Committed::Conflict(_));
        assert_eq!(after_lines, before_lines + usize::from(was_new));
        self.check_table();
    }

    /// The table equals the model, and every context reads the model's most specific value.
    fn check_table(&self) {
        let rows = self.store.setting_rows().unwrap();
        let table: BTreeMap<ModelKey, String> = rows
            .iter()
            .map(|r| {
                (
                    (
                        r.scope.project_id.clone(),
                        r.scope.agent_id.clone(),
                        r.key.clone(),
                    ),
                    r.value.clone(),
                )
            })
            .collect();
        assert_eq!(table, self.model);
        assert_eq!(rows.len(), table.len(), "a scope and key holds one row");
        for project in [None, Some("P-1")] {
            for agent in [None, Some(ORCHESTRATOR_AGENT)] {
                for key in KEYS {
                    let chain = self.store.setting_chain(key, project, agent).unwrap();
                    let expect = SettingScope::chain(project, agent)
                        .into_iter()
                        .find_map(|s| {
                            self.model
                                .get(&(s.project_id.clone(), s.agent_id.clone(), key.to_string()))
                                .map(|v| (s, v.clone()))
                        });
                    match (chain.first(), expect) {
                        (None, None) => {}
                        (Some(first), Some((scope, value))) => {
                            assert_eq!(first.scope, scope);
                            assert_eq!(first.value, value);
                        }
                        (got, want) => panic!("chain {got:?} but the model says {want:?}"),
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn setting_commands_admit_once_replay_by_id_and_keep_the_table_equal_to_the_model(
        ops in prop::collection::vec(op(), 1..40)
    ) {
        let mut rig = Rig::new();
        for op in &ops {
            rig.run(op);
        }
        // The outbox holds one line for each new command, and one for each of the three set-up
        // commands; the lines number 1, 2, 3 with no gap.
        let lines = rig.store.unwritten_ledger_lines(LEDGER).unwrap();
        prop_assert_eq!(lines.len(), rig.base_lines + rig.new_commands);
        prop_assert!(lines.iter().all(|l| l.run_id == CONTROLLER_SCOPE));
        let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
        let want: Vec<u64> = (1..=lines.len() as u64).collect();
        prop_assert_eq!(seqs, want);
        let admissions = rig.store.admissions(CONTROLLER_SCOPE).unwrap();
        prop_assert_eq!(admissions.len(), rig.base_lines + rig.new_commands);
    }
}
