//! Helpers for the rule tests. All data here is synthetic.
#![allow(dead_code)]

use std::path::PathBuf;

use pf_rules::{Context, Decision, OwnPaths, ToolCall, decide};
use pf_workflow::rules::{Allow, RoleRules};

/// A role with the given allow values.
pub fn role(name: &str, allow: &[Allow]) -> RoleRules {
    RoleRules {
        version: 1,
        role: name.to_string(),
        allow: allow.to_vec(),
    }
}

/// A shipped rule file of the `ossify-spine` workflow.
pub fn shipped(name: &str) -> RoleRules {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../workflows/ossify-spine/rules")
        .join(format!("{name}.yaml"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    RoleRules::parse(&text, &format!("{name}.yaml")).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

pub const SHIPPED_ROLES: [&str; 4] = ["lane", "implementer", "verifier", "close"];

/// Every role to try a universal deny against: the shipped ones and one that lists nothing.
pub fn every_role() -> Vec<RoleRules> {
    let mut v: Vec<RoleRules> = SHIPPED_ROLES.iter().map(|n| shipped(n)).collect();
    v.push(role("nothing", &[]));
    v.push(role(
        "everything",
        &[
            Allow::DeleteOwnScratch,
            Allow::DeleteOwnBuildTarget,
            Allow::PushOwnBranch,
            Allow::PushOwnTag,
            Allow::WriteOwnReportAndRunFiles,
        ],
    ));
    v
}

/// A seat on its own branch `work/u1`, in `/work/u1`.
pub fn ctx() -> Context {
    Context {
        seat: "seat-1".to_string(),
        current_branch: "work/u1".to_string(),
        own_branch: "work/u1".to_string(),
        own_tag: Some("v-u1".to_string()),
        default_branch: "main".to_string(),
        api_hosts: vec!["api.github.com".to_string()],
        cwd: PathBuf::from("/work/u1"),
        push_dir: PathBuf::new(),
        paths: OwnPaths {
            scratch: PathBuf::from("/scratch/u1"),
            build_target: PathBuf::from("/build/u1"),
            worktree: PathBuf::from("/work/u1"),
            report_and_run: vec![
                PathBuf::from("/run/u1"),
                PathBuf::from("/reports/u1/report.md"),
            ],
        },
    }
}

pub fn bash(rules: &RoleRules, command: &str) -> Decision {
    decide(rules, &ctx(), &ToolCall::bash(command))
}

pub fn bash_in(rules: &RoleRules, context: &Context, command: &str) -> Decision {
    decide(rules, context, &ToolCall::bash(command))
}

/// Assert a command is denied with `rule` for every role.
pub fn denied_for_every_role(command: &str, rule: &str) {
    for r in every_role() {
        let d = bash(&r, command);
        assert!(
            !d.is_allow() && d.rule == rule,
            "role {}: {command:?} -> {:?} {} ({})",
            r.role,
            d.outcome,
            d.rule,
            d.reason
        );
    }
}

/// Assert a command is allowed for every role in the list.
pub fn allowed_for_every_role(command: &str) {
    for r in every_role() {
        let d = bash(&r, command);
        assert!(
            d.is_allow(),
            "role {}: {command:?} -> {} ({})",
            r.role,
            d.rule,
            d.reason
        );
    }
}
