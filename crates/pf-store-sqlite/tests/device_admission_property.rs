//! AC-6 of `r1.s2.w2` (`run-state-admission`, property test, on the real store): over generated
//! sequences of device pair and revoke commands with command IDs drawn from a small pool,
//!
//! - the same ID with the same content returns `Replayed` with the first record;
//! - the same ID with other content returns `Conflict` and changes nothing;
//! - the live set equals a model's live set, and so does the revoked set;
//! - no device id is ever issued twice, and no token hash is held by two devices;
//! - the `@controller` outbox holds exactly one line for each new admitted or refused command, and
//!   none for a replay or a conflict.
//!
//! A command the store rejects (a token hash in use, an unknown or revoked id) is made a refused
//! admission in a second commit, as the controller does it.
//!
//! Failure persistence is off, as in `crates/pf-engine/tests/park_clear_property.rs:1-7`, so a run
//! from another directory leaves no `*.proptest-regressions` file.

use std::collections::BTreeMap;

use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry,
    NEW_ID, NewAdmission, NewDevice, Store,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const LEDGER: &str = "controller.ledger";

#[derive(Clone, Debug)]
enum Op {
    /// Pair a device: a command ID of the pool, a device name and a token of small pools.
    Pair { cmd: u8, name: u8, token: u8 },
    /// Revoke a device: a command ID of the pool and a device number of 1..=6.
    Revoke { cmd: u8, n: u8 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..5, 0u8..3, 0u8..6).prop_map(|(cmd, name, token)| Op::Pair { cmd, name, token }),
        (0u8..5, 1u8..7).prop_map(|(cmd, n)| Op::Revoke { cmd, n }),
    ]
}

/// What the model knows of one device.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Model {
    name: String,
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
    devices: BTreeMap<String, Model>,
    /// Every token hash a device holds, live or revoked, and the device: a revoked row keeps its
    /// hash.
    tokens: BTreeMap<String, String>,
    seen: BTreeMap<String, Seen>,
    /// Every id the store issued, in order.
    issued: Vec<String>,
    /// The number of new commands the model expects an outbox line for.
    new_commands: usize,
}

fn token_hash(token: u8) -> String {
    format!("{token:02x}").repeat(32)
}

fn content_pair(name: &str) -> String {
    serde_json::json!({ "device_name": name }).to_string()
}

fn content_revoke(id: &str) -> String {
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
            devices: BTreeMap::new(),
            tokens: BTreeMap::new(),
            seen: BTreeMap::new(),
            issued: Vec::new(),
            new_commands: 0,
        }
    }

    /// The model's devices, live or revoked, as (id, name) in pairing order.
    fn model(&self, live: bool) -> Vec<(String, String)> {
        let mut rows: Vec<(u64, String, String)> = self
            .devices
            .iter()
            .filter(|(_, m)| m.live == live)
            .map(|(id, m)| {
                let n: u64 = id.trim_start_matches("D-").parse().unwrap();
                (n, id.clone(), m.name.clone())
            })
            .collect();
        rows.sort();
        rows.into_iter().map(|(_, id, name)| (id, name)).collect()
    }

    /// Run one command through the store as the controller does, and check the answer against
    /// the model.
    fn run(&mut self, op: &Op) {
        let (kind, cmd, content) = match op {
            Op::Pair { cmd, name, .. } => (
                "device_pair",
                format!("cmd-{cmd}"),
                content_pair(&format!("name-{name}")),
            ),
            Op::Revoke { cmd, n } => (
                "device_revoke",
                format!("cmd-{cmd}"),
                content_revoke(&format!("D-{n}")),
            ),
        };

        // Does this command pass the model's own rules right now?
        let verdict: Result<(), String> = match op {
            Op::Pair { token, .. } => {
                if self.tokens.contains_key(&token_hash(*token)) {
                    Err("token".to_string())
                } else {
                    Ok(())
                }
            }
            Op::Revoke { n, .. } => match self.devices.get(&format!("D-{n}")) {
                Some(m) if m.live => Ok(()),
                _ => Err("revoke".to_string()),
            },
        };

        let before_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        let new_before = self.new_commands;
        let first = match op {
            Op::Pair { name, token, .. } => commit(
                kind,
                &cmd,
                &content,
                AdmissionOutcome::Admitted,
                None,
                Some(NEW_ID.to_string()),
                vec![Effect::AddDevice(NewDevice {
                    name: format!("name-{name}"),
                    token_sha256: token_hash(*token),
                    paired_at: "t".to_string(),
                })],
            ),
            Op::Revoke { n, .. } => commit(
                kind,
                &cmd,
                &content,
                AdmissionOutcome::Admitted,
                None,
                Some(format!("D-{n}")),
                vec![Effect::RevokeDevice {
                    id: format!("D-{n}"),
                    revoked_at: "t".to_string(),
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
                    Op::Pair { name, token, .. } => {
                        let id = admission.unit_id.clone().expect("the assigned id");
                        assert!(!self.issued.contains(&id), "{id} was issued twice");
                        self.issued.push(id.clone());
                        self.tokens.insert(token_hash(*token), id.clone());
                        self.devices.insert(
                            id,
                            Model {
                                name: format!("name-{name}"),
                                live: true,
                            },
                        );
                    }
                    Op::Revoke { n, .. } => {
                        self.devices.get_mut(&format!("D-{n}")).unwrap().live = false;
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

        // A replay or a conflict changes nothing. Every case: the live and the revoked set are the
        // model's, and a live token finds its device.
        let all = self.store.devices().unwrap();
        let split = |live: bool| -> Vec<(String, String)> {
            all.iter()
                .filter(|d| d.revoked_at.is_none() == live)
                .map(|d| (d.id.clone(), d.name.clone()))
                .collect()
        };
        assert_eq!(
            split(true),
            self.model(true),
            "the live set equals the model's"
        );
        assert_eq!(
            split(false),
            self.model(false),
            "the revoked set equals the model's"
        );
        assert_eq!(
            all.iter()
                .map(|d| d.revoked_at.is_some())
                .collect::<Vec<_>>(),
            {
                let mut flags = vec![false; split(true).len()];
                flags.extend(vec![true; split(false).len()]);
                flags
            },
            "live devices come first"
        );
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
    fn device_commands_admit_once_replay_by_id_and_keep_the_live_set(
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
        // A live token finds its device, and a revoked or unknown one finds nothing.
        for token in 0u8..6 {
            let found = rig.store.device_by_token(&token_hash(token)).unwrap().map(|d| d.id);
            let expected = rig
                .tokens
                .get(&token_hash(token))
                .filter(|id| rig.devices[*id].live)
                .cloned();
            prop_assert_eq!(found, expected);
        }
    }
}
