//! AC-11: one audit line for each decision, through `redact`.

mod common;

use common::*;
use pf_rules::{ToolCall, audit_line, decide};

/// The secret forms of `r0.s1.w2`'s redaction (`pf_engine::status::redact`).
const SECRETS: [&str; 9] = [
    "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
    "github_pat_11ABCDEFG0abcdefghijklmnopqrstuvwxyz",
    "sk-abcdefghijklmnopqrstuvwx",
    "Bearer abc.def.ghi-123",
    "Authorization: Basic dXNlcjpwYXNz",
    "hunter2secret",
    "supersecretvalue",
    "pw12345",
    "gho_abcdefghijklmnopqrstuvwxyz0123456789",
];

#[test]
fn an_allow_and_a_deny_each_give_one_line_with_seat_command_rule_and_outcome() {
    let close = shipped("close");
    let c = ctx();
    let call = ToolCall::bash("git push origin work/u1");
    let d = decide(&close, &c, &call);
    let line = audit_line(&c, &call, &d);
    assert!(!line.contains('\n'), "{line}");
    for part in [
        "seat=seat-1",
        "outcome=allow",
        "rule=push-own-branch",
        "command=git push origin work/u1",
    ] {
        assert!(line.contains(part), "{part} missing from {line}");
    }

    let call = ToolCall::bash("gh pr merge 7");
    let d = decide(&close, &c, &call);
    let line = audit_line(&c, &call, &d);
    for part in [
        "seat=seat-1",
        "outcome=deny",
        "rule=gh-pr-merge",
        "command=gh pr merge 7",
    ] {
        assert!(line.contains(part), "{part} missing from {line}");
    }

    // A write tool call has a line too.
    let call = ToolCall::write("write", "/etc/passwd");
    let d = decide(&close, &c, &call);
    let line = audit_line(&c, &call, &d);
    assert!(
        line.contains("outcome=deny")
            && line.contains("rule=write-outside-worktree")
            && line.contains("/etc/passwd"),
        "{line}"
    );
}

#[test]
fn a_multi_line_command_is_one_line() {
    let c = ctx();
    let call = ToolCall::bash("git commit -m 'a\nb'\necho done");
    let d = decide(&shipped("close"), &c, &call);
    let line = audit_line(&c, &call, &d);
    assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
    assert!(line.contains("echo done"));
}

#[test]
fn each_secret_form_is_absent_and_the_rest_of_the_command_is_present() {
    let c = ctx();
    let close = shipped("close");
    let commands = [
        format!("GH_TOKEN={} cargo test --workspace", SECRETS[0]),
        format!("GH_TOKEN={} cargo test --workspace", SECRETS[1]),
        format!(
            "curl -s https://example.com --header 'X-Key: {}' --workspace",
            SECRETS[2]
        ),
        format!(
            "curl -s https://example.com/x --workspace -H 'Authorization: {}'",
            SECRETS[3]
        ),
        format!(
            "curl -s https://example.com/x --workspace -H '{}'",
            SECRETS[4]
        ),
        format!("API_SECRET={} cargo test --workspace", SECRETS[5]),
        format!("cargo test --workspace --api-key={}", SECRETS[6]),
        format!("cargo test --workspace --api-key {}", SECRETS[6]),
        format!(
            "git clone https://user:{}@host.example/repo.git --workspace",
            SECRETS[7]
        ),
        format!("echo {} --workspace", SECRETS[8]),
    ];
    let secrets_in = [
        SECRETS[0],
        SECRETS[1],
        SECRETS[2],
        "abc.def.ghi-123",
        "dXNlcjpwYXNz",
        SECRETS[5],
        SECRETS[6],
        SECRETS[6],
        SECRETS[7],
        SECRETS[8],
    ];
    for (command, secret) in commands.iter().zip(secrets_in) {
        let call = ToolCall::bash(command);
        let d = decide(&close, &c, &call);
        let line = audit_line(&c, &call, &d);
        assert!(!line.contains(secret), "{secret} is in {line}");
        assert!(
            line.contains("--workspace"),
            "the rest of the command is present: {line}"
        );
        assert!(line.contains("[redacted]"), "{line}");
    }
    // A denied command is redacted the same way.
    let call = ToolCall::bash(&format!("GH_TOKEN={} gh pr merge 1", SECRETS[0]));
    let d = decide(&close, &c, &call);
    let line = audit_line(&c, &call, &d);
    assert!(
        line.contains("outcome=deny")
            && line.contains("gh pr merge 1")
            && !line.contains(SECRETS[0]),
        "{line}"
    );
}

#[test]
fn a_line_without_a_secret_is_unchanged() {
    let c = ctx();
    let call = ToolCall::bash("cargo test -p pf-rules --test audit_redaction");
    let d = decide(&shipped("implementer"), &c, &call);
    let line = audit_line(&c, &call, &d);
    assert_eq!(
        line,
        "seat=seat-1 outcome=allow rule=allowed command=cargo test -p pf-rules --test audit_redaction"
    );
}

/// An `Authorization:` value runs to the end of its own line. The audit line holds the whole
/// command on one line, so the command is redacted before its line breaks are written out.
/// Otherwise the value would swallow every command after it.
#[test]
fn an_authorization_value_does_not_swallow_the_next_line_of_the_command() {
    let c = ctx();
    let call = ToolCall::bash(
        "curl -s https://example.com/x -H 'Authorization: Token s3cr3tvalue9'\ncargo test --workspace",
    );
    let d = decide(&shipped("close"), &c, &call);
    let line = audit_line(&c, &call, &d);
    assert!(!line.contains("s3cr3tvalue9"), "{line}");
    assert!(
        line.contains("cargo test --workspace"),
        "the second line was swallowed: {line}"
    );
}
