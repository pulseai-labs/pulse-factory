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
