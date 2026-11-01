//! `pf-rules`: role rules. A pure library: it decides allow or deny for one tool call by role,
//! and it knows no agent. Any runtime's hook can call it (ADR-0005, `docs/contracts/role-rules.md`).
//!
//! `decide(rule_file, context, tool_call)` reads a shell command by parsing it, not by matching
//! words. It sees through quotes, heredocs, compound commands, substitutions and wrappers. It
//! does not match words inside quoted text, heredocs or `grep` patterns, with two exceptions: a
//! string that a shell will run is parsed again, and a commit message is checked for AI trailers.
//! A command it cannot parse is denied with the rule `unparsed-command`.

mod analyze;
pub mod shell;

use std::path::PathBuf;

use pf_engine::status::redact;
use pf_workflow::rules::{Allow, RoleRules};

use analyze::{Analyzer, normalize, under};

/// One part of a push the parser may or may not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// The text is known.
    Literal(String),
    /// The text holds a variable or a substitution. The raw text is kept.
    Unresolved(String),
    /// The command did not give it. For a branch this means the current branch.
    Absent,
}

/// One push: a remote, a branch and the directory of `git -C`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushIntent {
    pub remote: Part,
    pub branch: Part,
    pub dir: Part,
}

/// Find each push in a command, in the forms `decide` reads: plain, compound, `git -C`, inside a
/// shell string or a heredoc that feeds a shell, and behind wrappers. A push has one intent for
/// each branch it names. A text that cannot be parsed has none.
pub fn push_of(command: &str) -> Vec<PushIntent> {
    let rules = RoleRules {
        version: 1,
        role: String::new(),
        allow: Vec::new(),
    };
    let ctx = Context::default();
    let mut a = Analyzer::new(&rules, &ctx);
    a.text(command, 0);
    a.walk.pushes
}

/// The paths that belong to a seat.
#[derive(Clone, Debug, Default)]
pub struct OwnPaths {
    /// The seat's scratch directory.
    pub scratch: PathBuf,
    /// The seat's build target directory.
    pub build_target: PathBuf,
    /// The seat's worktree.
    pub worktree: PathBuf,
    /// The seat's report and the files under its run directory.
    pub report_and_run: Vec<PathBuf>,
}

/// What `decide` needs to know about the seat and the forge.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub seat: String,
    /// The branch checked out in the seat's worktree now.
    pub current_branch: String,
    /// The branch of the seat's own unit.
    pub own_branch: String,
    /// The tag of the seat's own unit, when it has one.
    pub own_tag: Option<String>,
    pub default_branch: String,
    /// The forge's API hosts, such as `api.github.com`.
    pub api_hosts: Vec<String>,
    /// The seat's working directory. Relative paths are read from here.
    pub cwd: PathBuf,
    pub paths: OwnPaths,
}

/// A tool call, as any runtime's hook can state it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCall {
    /// The tool's name, such as `bash`, `write` or `edit`.
    pub tool: String,
    /// The command line of a shell tool.
    pub command: Option<String>,
    /// The path a file tool works on.
    pub path: Option<String>,
}

impl ToolCall {
    pub fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool: "bash".to_string(),
            command: Some(command.to_string()),
            path: None,
        }
    }

    pub fn write(tool: &str, path: &str) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            command: None,
            path: Some(path.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Deny,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Allow => "allow",
            Outcome::Deny => "deny",
        }
    }
}

/// The answer for one tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub outcome: Outcome,
    /// The rule that decided, such as `force-push` or `own-branch-unverified`.
    pub rule: String,
    pub reason: String,
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        self.outcome == Outcome::Allow
    }
}

/// Tools that write a file at `path`.
const WRITE_TOOLS: [&str; 12] = [
    "write",
    "edit",
    "multiedit",
    "multi_edit",
    "notebookedit",
    "notebook_edit",
    "str_replace",
    "str_replace_editor",
    "ast_edit",
    "apply_patch",
    "create",
    "write_file",
];

fn allow(rule: &str, reason: &str) -> Decision {
    Decision {
        outcome: Outcome::Allow,
        rule: rule.to_string(),
        reason: reason.to_string(),
    }
}

/// Decide one tool call for a role.
pub fn decide(rules: &RoleRules, ctx: &Context, call: &ToolCall) -> Decision {
    if let Some(command) = &call.command {
        let mut a = Analyzer::new(rules, ctx);
        a.text(command, 0);
        let walk = a.walk;
        if let Some(d) = walk.denies.first() {
            return Decision {
                outcome: Outcome::Deny,
                rule: d.rule.clone(),
                reason: d.reason.clone(),
            };
        }
        for rule in [
            "own-branch-unverified",
            "push-own-branch",
            "push-own-tag",
            "delete-own-scratch",
            "delete-own-build-target",
        ] {
            if let Some(f) = walk.allows.iter().find(|f| f.rule == rule) {
                return allow(&f.rule, &f.reason);
            }
        }
        return allow("allowed", "no rule denies it, and seats need a shell");
    }
    let tool = call.tool.to_ascii_lowercase();
    if let (Some(path), true) = (&call.path, WRITE_TOOLS.contains(&tool.as_str())) {
        let abs = normalize(&ctx.cwd, path);
        if under(&abs, &ctx.paths.worktree) {
            return allow(
                "write-in-worktree",
                "a path inside the seat's worktree is its normal work",
            );
        }
        if rules.allow.contains(&Allow::WriteOwnReportAndRunFiles)
            && ctx.paths.report_and_run.iter().any(|p| under(&abs, p))
        {
            return allow(
                "write-own-report-and-run-files",
                "the role may write its report and run files",
            );
        }
        let why = if rules.allow.contains(&Allow::WriteOwnReportAndRunFiles) {
            format!("{path} lies outside the worktree and outside the seat's report and run files")
        } else {
            format!(
                "{path} lies outside the worktree, and role {} does not list write_own_report_and_run_files",
                rules.role
            )
        };
        return Decision {
            outcome: Outcome::Deny,
            rule: "write-outside-worktree".to_string(),
            reason: why,
        };
    }
    allow("allowed", "the tool is not one that this rule set limits")
}

/// One audit line for a decision: seat, command, rule and outcome. The line goes through
/// `redact`, so no secret form of `r0.s1.w2` stays in it, and a line break becomes `\n`.
pub fn audit_line(ctx: &Context, call: &ToolCall, decision: &Decision) -> String {
    let command = match (&call.command, &call.path) {
        (Some(c), _) => c.clone(),
        (None, Some(p)) => format!("{} {p}", call.tool),
        (None, None) => call.tool.clone(),
    };
    // Redact each part by itself, and the command before its line breaks are written out. An
    // `Authorization:` value runs to the end of its own line. On the finished audit line, a second
    // pass would take that header again and cut the rest of the command after it.
    let flat = redact(&command).replace('\n', "\\n").replace('\r', "\\r");
    format!(
        "seat={} outcome={} rule={} command={}",
        redact(&ctx.seat),
        decision.outcome.as_str(),
        redact(&decision.rule),
        flat
    )
}
