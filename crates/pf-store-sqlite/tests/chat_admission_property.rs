//! AC-4 (`run-state-admission`, property test, on the real store): over generated sequences of
//! project, worktree, chat session and chat item commands, with command IDs drawn from a small
//! pool, after every step
//!
//! - there is at most one live Orchestrator per project, and a session never starts in a worktree
//!   that was gone or removed (the model refuses what the store must refuse, and the reverse);
//! - at most one live worktree row holds a path, and every session's `project_id` is its worktree's
//!   project, and a live session's project is live;
//! - the same command ID with the same content returns `Replayed` with the first record, and with
//!   other content `Conflict`, changing nothing;
//! - no `P-`, `W-` or `S-` ID is issued twice (each new ID is the next number);
//! - the live sets equal the model's;
//! - the outbox holds exactly one line for each new admitted or refused command, and none for a
//!   replay, a conflict, a gone mark or a chat item;
//! - the `seq` of a session's items runs 1, 2, 3 with no gap, every request has at most one answer
//!   of the matching kind, and the store's pending set equals the model's;
//! - an appended item never changes, except by one redaction, which keeps its kind, `seq`,
//!   `answers_seq`, sizes and truncation marks and leaves only the marks in its text columns. A
//!   second redaction is refused and changes nothing.
//!
//! A command the store rejects becomes a refused admission in a second commit, as the controller
//! does it. Failure persistence is off, as in `project_admission_property.rs`, so a run from another
//! directory leaves no `*.proptest-regressions` file.

mod common;

use std::collections::BTreeMap;

use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, ChatItemKind, ChatItemRow, Committed, Effect,
    NEW_ID, NewChatItem, NewChatSession, NewProject, ORCHESTRATOR_AGENT, ProjectKind,
    REDACTED_PAYLOAD, REDACTED_TEXT, SessionWorktree, Store, ToolText, WORKTREE_ID,
    WorktreeCreatedBy,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Wt {
    Existing(u8),
    Record(u8),
}

#[derive(Clone, Debug)]
enum What {
    Request {
        approval: bool,
    },
    /// `outcome` 0, 1, 2 are the three good outcomes, 3 is a bad one.
    Answer {
        approval: bool,
        target: u8,
        outcome: u8,
    },
    Tool,
    Text,
    /// An assistant text that names a request it does not answer.
    Stray {
        target: u8,
    },
}

#[derive(Clone, Debug)]
enum Op {
    AddProject { cmd: u8, git: bool, root: u8 },
    RemoveProject { cmd: u8, n: u8 },
    Start { cmd: u8, n: u8, wt: Wt },
    End { cmd: u8, n: u8 },
    MarkGone { n: u8 },
    Append { n: u8, what: What },
    Redact { cmd: u8, n: u8, seq: u8 },
}

fn wt() -> impl Strategy<Value = Wt> {
    prop_oneof![
        (1u8..6).prop_map(Wt::Existing),
        (0u8..4).prop_map(Wt::Record),
    ]
}

fn what() -> impl Strategy<Value = What> {
    prop_oneof![
        any::<bool>().prop_map(|approval| What::Request { approval }),
        (any::<bool>(), 1u8..6, 0u8..4).prop_map(|(approval, target, outcome)| What::Answer {
            approval,
            target,
            outcome
        }),
        Just(What::Tool),
        Just(What::Text),
        (1u8..4).prop_map(|target| What::Stray { target }),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (0u8..12, any::<bool>(), 0u8..3)
            .prop_map(|(cmd, git, root)| Op::AddProject { cmd, git, root }),
        1 => (0u8..12, 1u8..5).prop_map(|(cmd, n)| Op::RemoveProject { cmd, n }),
        5 => (0u8..12, 1u8..5, wt()).prop_map(|(cmd, n, wt)| Op::Start { cmd, n, wt }),
        2 => (0u8..12, 1u8..6).prop_map(|(cmd, n)| Op::End { cmd, n }),
        1 => (1u8..6).prop_map(|n| Op::MarkGone { n }),
        6 => (1u8..6, what()).prop_map(|(n, what)| Op::Append { n, what }),
        1 => (0u8..12, 1u8..6, 1u8..6).prop_map(|(cmd, n, seq)| Op::Redact { cmd, n, seq }),
    ]
}

struct MProject {
    live: bool,
    git: bool,
    name: String,
    root: String,
}

struct MWorktree {
    project: u32,
    path: String,
    gone: bool,
    removed: bool,
}

impl MWorktree {
    fn live(&self) -> bool {
        !self.gone && !self.removed
    }
}

#[derive(Clone)]
struct MItem {
    kind: ChatItemKind,
    answers: Option<u64>,
    redacted: bool,
    payload: String,
    tool_input: Option<String>,
    tool_output: Option<String>,
}

struct MSession {
    project: u32,
    worktree: u32,
    live: bool,
    items: Vec<MItem>,
}

impl MSession {
    fn answered(&self, seq: u64) -> bool {
        self.items.iter().any(|i| i.answers == Some(seq))
    }

    fn pending(&self) -> Vec<u64> {
        self.items
            .iter()
            .enumerate()
            .filter(|(n, i)| i.kind.is_request() && !self.answered(*n as u64 + 1))
            .map(|(n, _)| n as u64 + 1)
            .collect()
    }
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
    projects: BTreeMap<u32, MProject>,
    worktrees: BTreeMap<u32, MWorktree>,
    sessions: BTreeMap<u32, MSession>,
    seen: BTreeMap<String, Seen>,
    new_commands: usize,
    /// A counter that makes each appended payload its own.
    appended: u32,
}

fn pid(n: u32) -> String {
    format!("P-{n}")
}
fn wid(n: u32) -> String {
    format!("W-{n}")
}
fn sid(n: u32) -> String {
    format!("S-{n}")
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        Rig {
            store: SqliteStore::open(dir.path().join("store.sqlite")).unwrap(),
            _dir: dir,
            projects: BTreeMap::new(),
            worktrees: BTreeMap::new(),
            sessions: BTreeMap::new(),
            seen: BTreeMap::new(),
            new_commands: 0,
            appended: 0,
        }
    }

    fn has_live_session(&self, project: u32) -> bool {
        self.sessions
            .values()
            .any(|s| s.project == project && s.live)
    }

    // ------------------------------------------------------------ the model's verdicts

    fn verdict(&self, op: &Op) -> Result<(), String> {
        match op {
            Op::AddProject { git: _, root, .. } => {
                let (name, root) = (format!("name-{root}"), format!("/p/{root}"));
                if self.projects.values().any(|p| p.live && p.root == root) {
                    Err("root".to_string())
                } else if self.projects.values().any(|p| p.live && p.name == name) {
                    Err("name".to_string())
                } else {
                    Ok(())
                }
            }
            Op::RemoveProject { n, .. } => match self.projects.get(&u32::from(*n)) {
                Some(p) if p.live && !self.has_live_session(u32::from(*n)) => Ok(()),
                Some(p) if p.live => Err("a live chat session holds it".to_string()),
                _ => Err("no live project".to_string()),
            },
            Op::Start { n, wt, .. } => {
                let n = u32::from(*n);
                if !self.projects.get(&n).is_some_and(|p| p.live) {
                    return Err("no live project".to_string());
                }
                if self.has_live_session(n) {
                    return Err("a live Orchestrator".to_string());
                }
                match wt {
                    Wt::Existing(w) => match self.worktrees.get(&u32::from(*w)) {
                        Some(w) if w.project == n && w.live() => Ok(()),
                        _ => Err("the worktree is no live worktree of the project".to_string()),
                    },
                    Wt::Record(i) => {
                        let path = format!("/p/{i}");
                        if self.worktrees.values().any(|w| w.live() && w.path == path) {
                            Err("a live row holds the path".to_string())
                        } else {
                            Ok(())
                        }
                    }
                }
            }
            Op::End { n, .. } => match self.sessions.get(&u32::from(*n)) {
                Some(s) if s.live => Ok(()),
                _ => Err("no live session".to_string()),
            },
            Op::Redact { n, seq, .. } => match self.sessions.get(&u32::from(*n)) {
                Some(s) => match s.items.get(usize::from(*seq) - 1) {
                    Some(i) if !i.redacted => Ok(()),
                    Some(_) => Err("redacted already".to_string()),
                    None => Err("no such item".to_string()),
                },
                None => Err("no session".to_string()),
            },
            Op::MarkGone { .. } | Op::Append { .. } => unreachable!("not a command"),
        }
    }

    // ------------------------------------------------------------ one command

    fn command(&mut self, op: &Op) {
        let (kind, cmd, content, effects, subject, line) = match op {
            Op::AddProject { cmd, git, root } => (
                "project_add",
                format!("cmd-{cmd}"),
                serde_json::json!({ "name": format!("name-{root}"), "root": format!("/p/{root}") })
                    .to_string(),
                vec![Effect::AddProject(NewProject {
                    name: format!("name-{root}"),
                    root: format!("/p/{root}"),
                    created_at: "t".to_string(),
                    kind: if *git {
                        ProjectKind::Git
                    } else {
                        ProjectKind::Folder
                    },
                    remote_key: None,
                })],
                NEW_ID.to_string(),
                format!("project_add {NEW_ID} admitted command=cmd-{cmd}"),
            ),
            Op::RemoveProject { cmd, n } => (
                "project_remove",
                format!("cmd-{cmd}"),
                serde_json::json!({ "id": pid(u32::from(*n)) }).to_string(),
                vec![Effect::RemoveProject {
                    id: pid(u32::from(*n)),
                    removed_at: "t".to_string(),
                }],
                pid(u32::from(*n)),
                format!(
                    "project_remove {} admitted command=cmd-{cmd}",
                    pid(u32::from(*n))
                ),
            ),
            Op::Start { cmd, n, wt } => (
                "chat_session_start",
                format!("cmd-{cmd}"),
                serde_json::json!({ "agent_id": "orchestrator", "project_id": pid(u32::from(*n)) })
                    .to_string(),
                vec![Effect::StartChatSession(NewChatSession {
                    project_id: pid(u32::from(*n)),
                    agent_id: ORCHESTRATOR_AGENT.to_string(),
                    worktree: match wt {
                        Wt::Existing(w) => SessionWorktree::Existing {
                            id: wid(u32::from(*w)),
                        },
                        Wt::Record(i) => SessionWorktree::Record {
                            path: format!("/p/{i}"),
                            created_by: WorktreeCreatedBy::Root,
                        },
                    },
                    created_at: "t".to_string(),
                })],
                NEW_ID.to_string(),
                format!(
                    "chat_session_start {NEW_ID} admitted project={} worktree={WORKTREE_ID} command=cmd-{cmd}",
                    pid(u32::from(*n))
                ),
            ),
            Op::End { cmd, n } => (
                "chat_session_end",
                format!("cmd-{cmd}"),
                serde_json::json!({ "session_id": sid(u32::from(*n)) }).to_string(),
                vec![Effect::EndChatSession {
                    id: sid(u32::from(*n)),
                    ended_at: "t".to_string(),
                }],
                sid(u32::from(*n)),
                format!(
                    "chat_session_end {} admitted command=cmd-{cmd}",
                    sid(u32::from(*n))
                ),
            ),
            Op::Redact { cmd, n, seq } => (
                "chat_item_redact",
                format!("cmd-{cmd}"),
                serde_json::json!({ "seq": u64::from(*seq), "session_id": sid(u32::from(*n)) })
                    .to_string(),
                vec![Effect::RedactChatItem {
                    session_id: sid(u32::from(*n)),
                    seq: u64::from(*seq),
                    redacted_at: "t".to_string(),
                }],
                sid(u32::from(*n)),
                format!(
                    "chat_item_redact {} seq={seq} admitted command=cmd-{cmd}",
                    sid(u32::from(*n))
                ),
            ),
            Op::MarkGone { .. } | Op::Append { .. } => unreachable!("not a command"),
        };
        let verdict = self.verdict(op);
        let before_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        let new_before = self.new_commands;
        let first = commit(
            kind,
            &cmd,
            &content,
            AdmissionOutcome::Admitted,
            None,
            Some(&subject),
            effects,
            Some(line),
        );
        let answer = self.store.commit(first).unwrap();

        match (self.seen.get(&cmd).cloned(), answer) {
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
            (None, Committed::Applied { admission, ledger }) => {
                assert!(
                    verdict.is_ok(),
                    "the store admitted what the model refuses: {op:?}"
                );
                assert_eq!(admission.outcome, AdmissionOutcome::Admitted);
                let body = ledger.expect("a ledger line").body;
                assert!(
                    !body.contains(NEW_ID) && !body.contains(WORKTREE_ID),
                    "{body}"
                );
                self.apply_model(op, &admission, &body);
                self.new_commands += 1;
                self.seen.insert(
                    cmd,
                    Seen {
                        content,
                        row: admission,
                    },
                );
            }
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
                    Some(&format!("refused: {why}")),
                    None,
                    vec![],
                    Some(format!("{kind} refused command={cmd}")),
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
        let after_lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        assert_eq!(
            after_lines - before_lines,
            self.new_commands - new_before,
            "one outbox line per new command, none for a replay or a conflict"
        );
    }

    /// The model takes an admitted command, and checks the IDs the store issued.
    fn apply_model(&mut self, op: &Op, admission: &AdmissionRow, line: &str) {
        match op {
            Op::AddProject { git, root, .. } => {
                let id = self.projects.len() as u32 + 1;
                assert_eq!(
                    admission.unit_id,
                    Some(pid(id)),
                    "the next P- id, never reused"
                );
                self.projects.insert(
                    id,
                    MProject {
                        live: true,
                        git: *git,
                        name: format!("name-{root}"),
                        root: format!("/p/{root}"),
                    },
                );
            }
            Op::RemoveProject { n, .. } => {
                let n = u32::from(*n);
                self.projects.get_mut(&n).unwrap().live = false;
                for w in self
                    .worktrees
                    .values_mut()
                    .filter(|w| w.project == n && w.live())
                {
                    w.removed = true;
                }
            }
            Op::Start { n, wt, .. } => {
                let n = u32::from(*n);
                let worktree = match wt {
                    Wt::Existing(w) => u32::from(*w),
                    Wt::Record(i) => {
                        let id = self.worktrees.len() as u32 + 1;
                        self.worktrees.insert(
                            id,
                            MWorktree {
                                project: n,
                                path: format!("/p/{i}"),
                                gone: false,
                                removed: false,
                            },
                        );
                        id
                    }
                };
                let id = self.sessions.len() as u32 + 1;
                assert_eq!(
                    admission.unit_id,
                    Some(sid(id)),
                    "the next S- id, never reused"
                );
                assert!(
                    line.contains(&format!("worktree={}", wid(worktree))),
                    "the line names the worktree: {line}"
                );
                self.sessions.insert(
                    id,
                    MSession {
                        project: n,
                        worktree,
                        live: true,
                        items: Vec::new(),
                    },
                );
            }
            Op::End { n, .. } => self.sessions.get_mut(&u32::from(*n)).unwrap().live = false,
            Op::Redact { n, seq, .. } => {
                self.sessions.get_mut(&u32::from(*n)).unwrap().items[usize::from(*seq) - 1]
                    .redacted = true;
            }
            Op::MarkGone { .. } | Op::Append { .. } => unreachable!("not a command"),
        }
    }

    // ------------------------------------------------------------ the writes outside admission

    fn mark_gone(&mut self, n: u8) {
        let n = u32::from(n);
        let expect = self.worktrees.get(&n).is_some_and(MWorktree::live);
        let wrote = self.store.mark_worktree_gone(&wid(n), "t").unwrap();
        assert_eq!(wrote, expect, "a gone mark writes a live row once");
        if expect {
            self.worktrees.get_mut(&n).unwrap().gone = true;
        }
    }

    fn append(&mut self, n: u8, what: &What) {
        self.appended += 1;
        let n = u32::from(n);
        let mut new = NewChatItem {
            session_id: sid(n),
            kind: ChatItemKind::AssistantText,
            payload: format!("{{\"n\":{}}}", self.appended),
            tool_input: None,
            tool_output: None,
            answers_seq: None,
            usage: None,
            created_at: "t".to_string(),
        };
        let mut expect_ok = self.sessions.contains_key(&n);
        match what {
            What::Request { approval } => {
                new.kind = if *approval {
                    ChatItemKind::ApprovalRequest
                } else {
                    ChatItemKind::QuestionRequest
                };
            }
            What::Answer {
                approval,
                target,
                outcome,
            } => {
                new.kind = if *approval {
                    ChatItemKind::ApprovalAnswer
                } else {
                    ChatItemKind::QuestionAnswer
                };
                let target = u64::from(*target);
                new.answers_seq = Some(target);
                new.payload = match outcome {
                    0 => "{\"outcome\":\"answered\"}",
                    1 => "{\"outcome\":\"lost\"}",
                    2 => "{\"outcome\":\"withdrawn\"}",
                    _ => "{\"outcome\":\"approved\"}",
                }
                .to_string();
                let good_target = self.sessions.get(&n).is_some_and(|s| {
                    s.items
                        .get(target as usize - 1)
                        .is_some_and(|i| Some(i.kind) == new.kind.answers())
                        && !s.answered(target)
                });
                expect_ok = expect_ok && *outcome < 3 && good_target;
            }
            What::Tool => {
                new.kind = ChatItemKind::ToolCall;
                new.tool_input = Some(ToolText::uncapped(format!("in-{}", self.appended)));
                new.tool_output = Some(ToolText::uncapped(format!("out-{}", self.appended)));
            }
            What::Text => {}
            What::Stray { target } => {
                new.answers_seq = Some(u64::from(*target));
                expect_ok = false;
            }
        }
        let lines = self.store.unwritten_ledger_lines(LEDGER).unwrap().len();
        let result = self.store.append_chat_item(new.clone());
        assert_eq!(result.is_ok(), expect_ok, "{what:?} on S-{n}: {result:?}");
        if let Ok(row) = result {
            let session = self.sessions.get_mut(&n).unwrap();
            assert_eq!(row.seq, session.items.len() as u64 + 1, "the next seq");
            session.items.push(MItem {
                kind: new.kind,
                answers: new.answers_seq,
                redacted: false,
                payload: new.payload.clone(),
                tool_input: new.tool_input.map(|t| t.text),
                tool_output: new.tool_output.map(|t| t.text),
            });
        }
        assert_eq!(
            self.store.unwritten_ledger_lines(LEDGER).unwrap().len(),
            lines,
            "a chat item makes no outbox line"
        );
    }

    // ------------------------------------------------------------ the invariants

    fn check(&self) {
        // The live sets equal the model's.
        let live_projects: Vec<String> = self
            .store
            .projects()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        let model_live: Vec<String> = self
            .projects
            .iter()
            .filter(|(_, p)| p.live)
            .map(|(n, _)| pid(*n))
            .collect();
        assert_eq!(live_projects, model_live, "the live projects");
        for (n, p) in &self.projects {
            let row = self.store.project(&pid(*n)).unwrap().unwrap();
            assert_eq!(
                row.kind,
                Some(if p.git {
                    ProjectKind::Git
                } else {
                    ProjectKind::Folder
                })
            );
            assert_eq!(
                (row.name.as_str(), row.root.as_str()),
                (p.name.as_str(), p.root.as_str())
            );
        }

        // Worktrees: the rows are the model's, and a live path is held by one row.
        let mut live_paths = Vec::new();
        let mut ids = Vec::new();
        for n in self.projects.keys() {
            for row in self.store.worktrees(&pid(*n)).unwrap() {
                ids.push(row.id.clone());
                let number: u32 = row.id[2..].parse().unwrap();
                let m = &self.worktrees[&number];
                assert_eq!(row.project_id, pid(m.project));
                assert_eq!(row.path, m.path);
                assert_eq!(row.gone_at.is_some(), m.gone, "{}", row.id);
                assert_eq!(row.removed_at.is_some(), m.removed, "{}", row.id);
                if row.gone_at.is_none() && row.removed_at.is_none() {
                    live_paths.push(row.path.clone());
                    assert_eq!(
                        self.store
                            .live_worktree_at(&row.path)
                            .unwrap()
                            .map(|w| w.id),
                        Some(row.id.clone())
                    );
                }
            }
        }
        assert_eq!(
            ids.len(),
            self.worktrees.len(),
            "every model worktree is in the store"
        );
        let mut unique = live_paths.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            live_paths.len(),
            "at most one live worktree row per path"
        );

        // Sessions.
        let live: Vec<String> = self
            .store
            .live_chat_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        let model_live: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.live)
            .map(|(n, _)| sid(*n))
            .collect();
        assert_eq!(live, model_live, "the live sessions");
        let mut per_project: BTreeMap<String, usize> = BTreeMap::new();
        for id in &live {
            let s = self.store.chat_session(id).unwrap().unwrap();
            *per_project.entry(s.project_id.clone()).or_default() += 1;
            assert!(
                self.projects[&s.project_id[2..].parse::<u32>().unwrap()].live,
                "a live session's project is live"
            );
        }
        assert!(
            per_project.values().all(|n| *n <= 1),
            "one live Orchestrator per project"
        );
        for (n, m) in &self.sessions {
            let s = self.store.chat_session(&sid(*n)).unwrap().unwrap();
            assert_eq!(
                (s.project_id.as_str(), s.worktree_id.as_str()),
                (pid(m.project).as_str(), wid(m.worktree).as_str())
            );
            let w = self.store.worktree(&s.worktree_id).unwrap().unwrap();
            assert_eq!(
                w.project_id, s.project_id,
                "the session's project is its worktree's"
            );
            assert_eq!(s.ended_at.is_none(), m.live);

            // The items: seq 1, 2, 3 with no gap; each as appended, or as one redaction left it.
            let items = self.store.chat_items_after(&s.id, 0, 1000).unwrap();
            assert_eq!(items.len(), m.items.len());
            for (i, (row, want)) in items.iter().zip(&m.items).enumerate() {
                assert_eq!(row.seq, i as u64 + 1, "no gap in seq");
                assert_eq!(*row, expected_row(&s.id, i as u64 + 1, want));
            }
            // Every request has at most one answer, of the matching kind, and the pending set is
            // the model's.
            for row in &items {
                let answers: Vec<&ChatItemRow> = items
                    .iter()
                    .filter(|a| a.answers_seq == Some(row.seq))
                    .collect();
                assert!(answers.len() <= 1, "at most one answer");
                for a in answers {
                    let want = ChatItemKind::parse(&a.kind).and_then(ChatItemKind::answers);
                    assert_eq!(want.map(ChatItemKind::as_str), Some(row.kind.as_str()));
                }
            }
            let pending: Vec<u64> = self
                .store
                .pending_requests(&s.id)
                .unwrap()
                .into_iter()
                .map(|r| r.seq)
                .collect();
            assert_eq!(pending, m.pending(), "the pending requests of {}", s.id);
        }
    }
}

/// The row of an item as the model expects it: as appended, or as one redaction left it.
fn expected_row(session: &str, seq: u64, m: &MItem) -> ChatItemRow {
    let text = |t: &Option<String>| {
        t.as_ref().map(|t| {
            if m.redacted {
                REDACTED_TEXT.to_string()
            } else {
                t.clone()
            }
        })
    };
    ChatItemRow {
        session_id: session.to_string(),
        seq,
        kind: m.kind.as_str().to_string(),
        payload: if m.redacted {
            REDACTED_PAYLOAD.to_string()
        } else {
            m.payload.clone()
        },
        tool_input: text(&m.tool_input),
        tool_input_bytes: m.tool_input.as_ref().map(|t| t.len() as u64),
        tool_input_truncated: false,
        tool_output: text(&m.tool_output),
        tool_output_bytes: m.tool_output.as_ref().map(|t| t.len() as u64),
        tool_output_truncated: false,
        answers_seq: m.answers,
        redacted_at: m.redacted.then(|| "t".to_string()),
        created_at: "t".to_string(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn chat_commands_admit_once_replay_by_id_and_keep_the_hierarchy_rules(
        ops in prop::collection::vec(op(), 1..45)
    ) {
        let mut rig = Rig::new();
        for op in &ops {
            match op {
                Op::MarkGone { n } => rig.mark_gone(*n),
                Op::Append { n, what } => rig.append(*n, what),
                command => rig.command(command),
            }
            rig.check();
        }
        // The outbox holds one line for each new command, in all, numbered 1, 2, 3 with no gap.
        let lines = rig.store.unwritten_ledger_lines(LEDGER).unwrap();
        prop_assert_eq!(lines.len(), rig.new_commands);
        let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
        let want: Vec<u64> = (1..=lines.len() as u64).collect();
        prop_assert_eq!(seqs, want);
        let admissions = rig.store.admissions(CONTROLLER_SCOPE).unwrap();
        prop_assert_eq!(admissions.len(), rig.new_commands);
        let mut ids: Vec<&str> = admissions.iter().map(|a| a.command_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        prop_assert_eq!(ids.len(), admissions.len());
    }
}
