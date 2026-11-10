//! AC-4 of `r1.s2.w2`: the committed `apps/api-client/src/gen/` equals what `ts-rs` renders now.
//!
//! A difference fails with the name of the file. To rewrite the committed files after a change to
//! a wire type, run this from the repository root:
//!
//! ```text
//! TS_RS_EXPORT_DIR=apps/api-client/src/gen cargo test -p pf-api --test ts_types -- --ignored write
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pf_api::types::export_all;
use ts_rs::Config;

/// The workspace root: two levels above this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn committed_dir() -> PathBuf {
    root().join("apps/api-client/src/gen")
}

/// The `.ts` files of a directory: name to text. A missing directory holds none.
fn read_ts(dir: &Path) -> BTreeMap<String, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ts"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect()
}

/// What `ts-rs` renders now, in a temporary directory.
fn rendered() -> (tempfile::TempDir, BTreeMap<String, String>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::new().with_out_dir(dir.path());
    export_all(&cfg).expect("ts-rs renders every wire type");
    let files = read_ts(dir.path());
    (dir, files)
}

#[test]
fn the_committed_types_equal_what_ts_rs_renders_now() {
    let (_dir, now) = rendered();
    assert!(!now.is_empty(), "ts-rs rendered no file");
    let committed = read_ts(&committed_dir());
    let hint = "run: TS_RS_EXPORT_DIR=apps/api-client/src/gen cargo test -p pf-api --test ts_types -- --ignored write";
    let mut problems = Vec::new();
    for (name, text) in &now {
        match committed.get(name) {
            None => problems.push(format!("{name}: not committed")),
            Some(old) if old != text => problems.push(format!("{name}: differs from the render")),
            Some(_) => {}
        }
    }
    for name in committed.keys() {
        if !now.contains_key(name) {
            problems.push(format!("{name}: committed, but no type renders it"));
        }
    }
    assert!(
        problems.is_empty(),
        "apps/api-client/src/gen/ has drifted:\n  {}\n{hint}",
        problems.join("\n  ")
    );
}

/// `r1.s3.w2`: a project shows its kind, its remote key and why it has no kind. A remote URL has no
/// field in any wire type: only the canonical key `host/path` does.
#[test]
fn a_project_record_has_its_kind_its_remote_key_and_the_cause_of_no_kind() {
    let (_dir, now) = rendered();
    let record = &now["ProjectRecord.ts"];
    for field in [
        "kind: ProjectKind | null",
        "remote_key: string | null",
        "kind_error: string | null",
    ] {
        assert!(
            record.contains(field),
            "ProjectRecord.ts lacks {field}: {record}"
        );
    }
    assert!(record.contains("import type { ProjectKind }"), "{record}");
    let kind = &now["ProjectKind.ts"];
    assert!(kind.contains("\"git\" | \"folder\""), "{kind}");
    for (name, text) in &now {
        assert!(
            !text.contains("remote_url"),
            "{name} has a remote URL field"
        );
    }
}

#[test]
fn the_wire_types_a_client_needs_are_rendered() {
    let (_dir, now) = rendered();
    for name in [
        "ProjectAddArgs.ts",
        "ProjectRemoveArgs.ts",
        "ProjectRecord.ts",
        "ProjectKind.ts",
        "ProjectChanged.ts",
        "ProjectList.ts",
        "DeviceRecord.ts",
        "PairRequest.ts",
        "PairResult.ts",
        "ApiStatus.ts",
        "ApiError.ts",
        "ListenState.ts",
        "ApiState.ts",
        // r1.s3.w3: the server, the chat records, the frames of the stream and the verb arguments.
        "ServerRef.ts",
        "ChatState.ts",
        "ChatUsage.ts",
        "ChatTaskRecord.ts",
        "ChatProcessRecord.ts",
        "ChatContext.ts",
        "ChatSessionRecord.ts",
        "ChatOptionRecord.ts",
        "MidTurnMode.ts",
        "ChatRuntimeRecord.ts",
        "AgentRecord.ts",
        "SessionList.ts",
        "SessionChanged.ts",
        "SessionItems.ts",
        "ToolTextRecord.ts",
        "AnswerOutcome.ts",
        "QuestionRecord.ts",
        "ChatItemBody.ts",
        "ChatItemRecord.ts",
        "ChatFrame.ts",
        "ChatAnswerArg.ts",
        "SessionStartArgs.ts",
        "SessionSendArgs.ts",
        "SessionAnswerArgs.ts",
        "SessionInterruptArgs.ts",
        "SessionSetArgs.ts",
        "SessionEndArgs.ts",
        "SessionListArgs.ts",
        // r1.s4.w4: the scoped settings.
        "SettingValue.ts",
        "SettingScope.ts",
        "SettingMerge.ts",
        "SettingRecord.ts",
        "SettingsGetArgs.ts",
        "SettingsSetArgs.ts",
        "SettingsUnsetArgs.ts",
        "SettingsView.ts",
        "SettingChanged.ts",
        // r1.s4.w1: the requests, the claim, the confirm code and the request frames.
        "ClaimRecord.ts",
        "RequestSummary.ts",
        "RequestDetail.ts",
        "RequestList.ts",
        "RequestsArgs.ts",
        "ShowArgs.ts",
        "ConfirmVerb.ts",
        "ConfirmSubjectArg.ts",
        "ConfirmCodeArgs.ts",
        "ConfirmSubjectShown.ts",
        "ConfirmIssued.ts",
        "DeviceAnswerArgs.ts",
        "DeviceClearArgs.ts",
        "DeviceAckArgs.ts",
        "RequestActed.ts",
        "RequestFrame.ts",
    ] {
        assert!(now.contains_key(name), "{name} is not rendered");
    }
    // The token is a field of the pair answer and of no other type, and no type has a field for
    // its hash or for the code's secret. (The doc comments may use the word.)
    assert!(now["PairResult.ts"].contains("token: string"));
    for (name, text) in &now {
        if name != "PairResult.ts" {
            assert!(!text.contains("token:"), "{name} has a token field");
        }
        assert!(!text.contains("token_sha256"), "{name} has a hash field");
        assert!(!text.contains("secret"), "{name} names a secret");
    }
    // The listener's states are the four strings of the contract.
    for state in ["off", "listening", "waiting", "failed"] {
        assert!(
            now["ListenState.ts"].contains(&format!("\"{state}\"")),
            "{state}"
        );
    }
}

/// `r1.s3.w3`: no generated file has a `bigint`, because every `u64` is a `number` (the values stay
/// below 2^53), and a client never meets a `bigint` in JSON. The redaction arguments are CLI-only and
/// have no file. `ApiStatus.server` is optional, so a fixture of an older server still fits.
#[test]
fn the_chat_types_hold_no_bigint_and_the_stream_frames_are_tagged() {
    let (_dir, now) = rendered();
    for (name, text) in &now {
        assert!(!text.contains("bigint"), "{name} has a bigint: {text}");
    }
    assert!(
        !now.contains_key("SessionRedactArgs.ts"),
        "the CLI-only arguments are exported"
    );
    let status = &now["ApiStatus.ts"];
    assert!(status.contains("server?: ServerRef"), "{status}");
    let frame = &now["ChatFrame.ts"];
    for kind in [
        "hello",
        "session",
        "item",
        "caught",
        "delta",
        "tool_running",
    ] {
        assert!(
            frame.contains(&format!("\"type\": \"{kind}\"")),
            "ChatFrame.ts lacks {kind}: {frame}"
        );
    }
    let body = &now["ChatItemBody.ts"];
    for kind in [
        "operator_message",
        "assistant_text",
        "tool_call",
        "approval_request",
        "approval_answer",
        "question_request",
        "question_answer",
        "background_task",
        "notice",
        "turn_result",
        "redacted",
        "unknown",
    ] {
        assert!(
            body.contains(&format!("\"kind\": \"{kind}\"")),
            "ChatItemBody.ts lacks {kind}: {body}"
        );
    }
    let session = &now["ChatSessionRecord.ts"];
    for field in [
        "state: ChatState",
        "pending_options",
        "context: ChatContext | null",
        "cache_ttl_seconds: number | null",
    ] {
        assert!(
            session.contains(field),
            "ChatSessionRecord.ts lacks {field}: {session}"
        );
    }
}

/// `r1.s4.w4`: a setting's value is a number or a list of strings, and a record carries the layers
/// the UI's Settings rows read.
#[test]
fn a_setting_value_reads_number_or_list_and_a_record_carries_its_layers() {
    let (_dir, now) = rendered();
    assert!(
        now["SettingValue.ts"].contains("number | Array<string>"),
        "{}",
        now["SettingValue.ts"]
    );
    let record = &now["SettingRecord.ts"];
    for field in [
        "from: SettingScope",
        "layers: Array<",
        "masked: Array<",
        "merge: SettingMerge",
    ] {
        assert!(
            record.contains(field),
            "SettingRecord.ts lacks {field}: {record}"
        );
    }
    assert!(now["SettingMerge.ts"].contains("\"override\""));
}

/// `r1.s4.w1`: the request frames are a tagged type of their own and `ChatFrame` does not change; the
/// confirm subject is tagged by `kind`; `ApiError.refusal` is optional, so a body of an older server
/// still fits; a `u64` is a `number`.
#[test]
fn the_request_frames_and_the_confirm_subject_are_tagged_and_the_chat_frame_is_unchanged() {
    let (_dir, now) = rendered();
    let frame = &now["RequestFrame.ts"];
    for kind in ["requests", "request", "claim"] {
        assert!(
            frame.contains(&format!("\"type\": \"{kind}\"")),
            "RequestFrame.ts lacks {kind}: {frame}"
        );
    }
    let chat = &now["ChatFrame.ts"];
    for kind in ["requests", "request", "claim"] {
        assert!(
            !chat.contains(&format!("\"type\": \"{kind}\"")),
            "ChatFrame.ts names {kind}: the chat frames do not change"
        );
    }
    let subject = &now["ConfirmSubjectArg.ts"];
    assert!(subject.contains("\"kind\": \"request\""), "{subject}");
    for field in ["text_sha256?: string", "to?: string", "verb: ConfirmVerb"] {
        assert!(
            subject.contains(field),
            "ConfirmSubjectArg.ts lacks {field}: {subject}"
        );
    }
    let verbs = &now["ConfirmVerb.ts"];
    for verb in ["answer", "clear", "ack"] {
        assert!(verbs.contains(&format!("\"{verb}\"")), "{verbs}");
    }
    assert!(
        now["ApiError.ts"].contains("refusal?: string"),
        "{}",
        now["ApiError.ts"]
    );
    let claim = &now["ClaimRecord.ts"];
    for field in [
        "generation: number",
        "device: string | null",
        "revoked: boolean",
    ] {
        assert!(
            claim.contains(field),
            "ClaimRecord.ts lacks {field}: {claim}"
        );
    }
    let issued = &now["ConfirmIssued.ts"];
    for field in ["code: string | null", "generation: number | null"] {
        assert!(
            issued.contains(field),
            "ConfirmIssued.ts lacks {field}: {issued}"
        );
    }
}

/// Rewrite the committed files. `TS_RS_EXPORT_DIR` names the directory, from the repository root
/// when it is a relative path. Files that no type renders any more are removed.
#[test]
#[ignore = "writes files: run it by hand, with TS_RS_EXPORT_DIR set"]
fn write() {
    let given = std::env::var("TS_RS_EXPORT_DIR")
        .expect("set TS_RS_EXPORT_DIR, for example apps/api-client/src/gen");
    let dir = {
        let p = PathBuf::from(given);
        if p.is_absolute() { p } else { root().join(p) }
    };
    std::fs::create_dir_all(&dir).unwrap();
    for stale in read_ts(&dir).into_keys() {
        std::fs::remove_file(dir.join(stale)).unwrap();
    }
    let cfg = Config::new().with_out_dir(&dir);
    export_all(&cfg).expect("ts-rs writes every wire type");
}
