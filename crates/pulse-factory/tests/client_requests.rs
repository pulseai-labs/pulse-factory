//! AC-5 of `r1.s4.w3`: the Needs you client against a real controller. The spine-close rig (a
//! `FakeAdapter` and a `BareForge`) with the API on `127.0.0.1:0`, and `pf_client::transport::Transport`
//! over a `FileStore`, paired over pinned TLS, as the Mac app's Rust side is. Every merge lands in a
//! local bare repo. Nothing here reaches beyond `127.0.0.1` or starts `claude`.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use std::sync::Arc;
use std::time::Duration;

use common::chat::{add_project, chat_ledger, controller_ledger, repo};
use common::*;
use pf_api::types::{
    ConfirmCodeArgs, ConfirmSubjectArg, ConfirmVerb, DeviceAckArgs, DeviceAnswerArgs, RequestFrame,
    RequestsArgs, RunListArgs, SettingValue, SettingsGetArgs, SettingsSetArgs, ShowArgs,
};
use pf_client::error::{ClientError, ErrorKind};
use pf_client::store::{DeviceStore, FileStore};
use pf_client::stream::{ChatStream, StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::Transport;
use pf_engine::claim::sha256_hex;

/// A client over a tokio runtime. The stream's task runs while a call or a wait of this synchronous
/// test is in `block_on`, and the frames wait on the socket between them.
struct Client {
    rt: tokio::runtime::Runtime,
    transport: Transport<FileStore>,
    store: Arc<FileStore>,
    _dir: tempfile::TempDir,
}

fn client(api: &rig::Api) -> Client {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    let transport = Transport::new(store.clone()).with_stream_tuning(StreamTuning {
        schedule: vec![Duration::from_millis(50), Duration::from_millis(100)],
        reset_after: Duration::from_secs(60),
        silence: Duration::from_secs(30),
    });
    rt.block_on(transport.pair(&api.pair_code(), "MacBook"))
        .unwrap();
    Client {
        rt,
        transport,
        store,
        _dir: dir,
    }
}

impl Client {
    fn token(&self) -> String {
        self.store.load().unwrap().unwrap().token
    }

    fn open_overview(&self) -> ChatStream {
        let _enter = self.rt.enter();
        self.transport.open_stream(StreamOptions::default())
    }

    /// The request frames of the stream, in order, until `done` accepts the list. Chat frames are
    /// skipped, and a `closed` or `reconnecting` event fails the test.
    fn frames_until(
        &self,
        stream: &mut ChatStream,
        what: &str,
        done: impl Fn(&[RequestFrame]) -> bool,
    ) -> Vec<RequestFrame> {
        self.rt.block_on(async {
            let end = tokio::time::Instant::now() + Duration::from_secs(20);
            let mut frames = Vec::new();
            while !done(&frames) {
                let left = end.saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(left, stream.next()).await {
                    Ok(Some(StreamEvent::RequestFrame { frame })) => frames.push(frame),
                    Ok(Some(StreamEvent::Frame { .. })) => {}
                    Ok(other) => panic!("{other:?} while waiting for {what}"),
                    Err(_) => panic!("timed out waiting for {what}"),
                }
            }
            frames
        })
    }

    /// Wait for the request frame that `want` accepts.
    fn frame_where(
        &self,
        stream: &mut ChatStream,
        what: &str,
        want: impl Fn(&RequestFrame) -> bool,
    ) -> RequestFrame {
        let frames = self.frames_until(stream, what, |f| f.last().is_some_and(&want));
        frames.into_iter().last().unwrap()
    }
}

fn is_request(frame: &RequestFrame, id: &str, status: &str) -> bool {
    matches!(frame, RequestFrame::Request { request } if request.id == id && request.status == status)
}

fn code_of(issued: &pf_api::types::ConfirmIssued) -> String {
    issued
        .code
        .clone()
        .expect("the first answer carries the code")
}

fn answer_subject(id: &str, text: &str) -> ConfirmCodeArgs {
    ConfirmCodeArgs {
        subject: ConfirmSubjectArg::Request {
            request: id.to_string(),
            revision: 1,
            verb: ConfirmVerb::Answer,
            text_sha256: Some(sha256_hex(text.as_bytes())),
            to: None,
        },
        command_id: "issue-answer".to_string(),
    }
}

fn ack_subject(id: &str) -> ConfirmCodeArgs {
    ConfirmCodeArgs {
        subject: ConfirmSubjectArg::Request {
            request: id.to_string(),
            revision: 1,
            verb: ConfirmVerb::Ack,
            text_sha256: None,
            to: None,
        },
        command_id: "issue-ack".to_string(),
    }
}

fn reads(api: &rig::Api, verb: &str) -> usize {
    api.api_ledger()
        .iter()
        .filter(|l| l.contains(&format!("verb={verb} ")))
        .count()
}

/// No code and no token is in any error message, `Debug` print or ledger.
fn assert_no_secret(api: &rig::Api, errors: &[ClientError], secrets: &[&str]) {
    let s = &api.s;
    let mut seen: Vec<String> = errors
        .iter()
        .flat_map(|e| [e.message.clone(), format!("{e:?}")])
        .collect();
    seen.extend(api.api_ledger());
    seen.extend(controller_ledger(&s.live));
    seen.extend(chat_ledger(&s.live));
    seen.push(s.ledger());
    for line in &seen {
        for secret in secrets {
            assert!(!line.contains(secret), "a secret is in: {line}");
        }
        assert!(!line.contains("pfc1-"), "a code is in: {line}");
    }
}

/// The overview stream's first request frame is the snapshot, and the client has read nothing.
fn expect_empty_snapshot(api: &rig::Api, c: &Client, stream: &mut ChatStream) {
    let first = c.frame_where(stream, "the requests snapshot", |f| {
        matches!(f, RequestFrame::Requests { .. })
    });
    match first {
        RequestFrame::Requests { requests, claim } => {
            assert!(requests.is_empty() && claim.is_none());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(reads(api, "requests"), 0);
}

#[test]
fn a_client_answers_a_stop_request_and_a_second_spend_of_the_code_is_refused() {
    let api = rig::api();
    let c = client(&api);
    let t = &c.transport;
    let token = c.token();
    let mut errors: Vec<ClientError> = Vec::new();

    // An overview stream (no session): `hello`, then a `requests` snapshot, and no read by the client.
    let mut stream = c.open_overview();
    expect_empty_snapshot(&api, &c, &mut stream);

    // The stop request opens, and its frame arrives with no read.
    let question = "Which base branch?";
    let stop = api.open_stop(question);
    c.frame_where(&mut stream, "the stop request frame", |f| {
        is_request(f, &stop, "open")
    });
    assert_eq!(reads(&api, "requests") + reads(&api, "show"), 0);

    // `requests`, `show`, `confirm_code` and `answer` answer it (G2): the device takes the claim.
    let list =
        c.rt.block_on(t.requests(RequestsArgs {
            run: None,
            all: Some(true),
        }))
        .unwrap();
    assert_eq!(list.open, 1);
    assert_eq!(list.requests[0].id, stop);
    assert!(list.claim.is_none());
    let shown =
        c.rt.block_on(t.show(ShowArgs { id: stop.clone() }))
            .unwrap();
    assert!(
        shown.body.to_string().contains(question),
        "the body shows the question: {}",
        shown.body
    );
    let text = "Use main.";
    let issued =
        c.rt.block_on(t.confirm_code(answer_subject(&stop, text)))
            .unwrap();
    let code = code_of(&issued);
    assert!(code.starts_with("pfc1-"));
    assert!(issued.holder.is_none(), "no one held the claim");
    assert_eq!(issued.device, "D-1");
    let args = DeviceAnswerArgs {
        request: stop.clone(),
        rev: 1,
        text: text.to_string(),
        code: code.clone(),
        command_id: "answer-1".to_string(),
    };
    let acted = c.rt.block_on(t.answer(args.clone())).unwrap();
    assert!(!acted.replayed);
    assert_eq!(acted.generation, Some(1));
    assert_eq!(
        acted.request.as_ref().map(|r| r.status.as_str()),
        Some("answered")
    );
    // The answered request and the claim change arrive as frames, in either order: the device holds
    // the claim now.
    c.frames_until(&mut stream, "the answered and claim frames", |frames| {
        frames.iter().any(|f| is_request(f, &stop, "answered"))
            && frames.iter().any(|f| {
                matches!(f, RequestFrame::Claim { claim: Some(c) } if c.device.as_deref() == Some("D-1"))
            })
    });

    // A second spend of the same code is refused with its word.
    let again = DeviceAnswerArgs {
        command_id: "answer-2".to_string(),
        ..args
    };
    let refused = c.rt.block_on(t.answer(again)).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Refused, "{refused:?}");
    assert_eq!(refused.refusal.as_deref(), Some("code_used"), "{refused:?}");
    errors.push(refused);
    assert_no_secret(&api, &errors, &[code.as_str(), token.as_str()]);
}

#[test]
fn a_client_merges_the_spine_pr_and_reads_the_run_and_the_settings() {
    let api = rig::api();
    let s = &api.s;
    let c = client(&api);
    let t = &c.transport;
    let token = c.token();
    let mut errors: Vec<ClientError> = Vec::new();
    let mut stream = c.open_overview();
    expect_empty_snapshot(&api, &c, &mut stream);

    // The run reaches the spine PR: a new `merge_ack` frame, and no read by the client.
    let (ack_id, head) = api.to_merge_ack();
    c.frame_where(&mut stream, "the merge ack frame", |f| {
        is_request(f, &ack_id, "open")
    });
    assert_eq!(reads(&api, "requests") + reads(&api, "show"), 0);

    // `confirm_code` and `ack` merge the spine PR on the bare forge.
    let main_before = s.head("main");
    let issued = c.rt.block_on(t.confirm_code(ack_subject(&ack_id))).unwrap();
    assert_eq!(issued.subject.head_sha.as_deref(), Some(head.as_str()));
    assert_eq!(issued.subject.base.as_deref(), Some("main"));
    assert_eq!(
        issued.subject.head_branch.as_deref(),
        Some(SPINE_BRANCH_NAME)
    );
    assert!(issued.holder.is_none(), "no one holds the claim yet");
    let ack_code = code_of(&issued);
    let ack = DeviceAckArgs {
        request: ack_id.clone(),
        rev: 1,
        code: ack_code.clone(),
        command_id: "ack-1".to_string(),
    };
    let merged = c.rt.block_on(t.ack(ack.clone())).unwrap();
    assert_eq!(merged.merged, Some(true), "{merged:?}");
    assert_eq!(merged.head_sha.as_deref(), Some(head.as_str()));
    assert_eq!(merged.outcome.as_deref(), Some("landed"));
    assert_ne!(s.head("main"), main_before);
    git(
        &s.live.bare,
        &[
            "merge-base",
            "--is-ancestor",
            head.as_str(),
            s.head("main").as_str(),
        ],
    );
    // A replay of the command ID acts on nothing.
    let replay = c.rt.block_on(t.ack(ack)).unwrap();
    assert!(replay.replayed && replay.merged.is_none(), "{replay:?}");

    // `run_list` shows `waiting` once the lane ends, and `ended` after the end file.
    api.wait_for_the_close();
    let runs = c.rt.block_on(t.run_list(RunListArgs {})).unwrap();
    assert_eq!(runs.runs.len(), 1);
    let waiting = runs.runs[0]
        .waiting
        .clone()
        .expect("the run waits for its close");
    assert!(
        s.live.sb.status_json("")["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == waiting.as_str())
    );
    assert_eq!(runs.runs[0].state, "active");
    api.close();
    let runs = c.rt.block_on(t.run_list(RunListArgs {})).unwrap();
    assert_eq!(runs.runs[0].state, "ended");
    assert!(runs.runs[0].waiting.is_none());
    assert_eq!(
        runs.runs[0].end_reason.as_deref(),
        s.live.sb.status_json("")["end_reason"].as_str()
    );

    // `settings_set` at project scope and `settings_get` round-trip, with `set_by` the device.
    let project = add_project(&s.live, &repo(&s.live, "alpha"));
    let set =
        c.rt.block_on(t.settings_set(SettingsSetArgs {
            key: "chat.self_turn_cap".to_string(),
            value: SettingValue::Number(5),
            project_id: Some(project.clone()),
            agent_id: None,
            command_id: "set-1".to_string(),
        }))
        .unwrap();
    assert_eq!(set.value, Some(SettingValue::Number(5)));
    let got =
        c.rt.block_on(t.settings_get(SettingsGetArgs {
            key: Some("chat.self_turn_cap".to_string()),
            project_id: Some(project),
            agent_id: None,
        }))
        .unwrap();
    let record = &got.settings[0];
    assert_eq!(record.value, SettingValue::Number(5));
    assert_eq!(
        serde_json::to_value(&record.from).unwrap()["kind"],
        "project"
    );
    assert_eq!(record.layers[0].set_by, "D-1");

    // A second spend of the same ack code is refused with its word.
    let refused =
        c.rt.block_on(t.ack(DeviceAckArgs {
            request: ack_id.clone(),
            rev: 1,
            code: ack_code.clone(),
            command_id: "ack-2".to_string(),
        }))
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Refused, "{refused:?}");
    errors.push(refused);

    assert_no_secret(&api, &errors, &[ack_code.as_str(), token.as_str()]);
}

const SPINE_BRANCH_NAME: &str = "spine/s1";
