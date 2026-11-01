//! The command line of one OMP seat (ADR-0011).

use std::path::Path;

use pf_engine::port::agent::{AgentError, LaunchPlan};

/// The tools a seat gets. The list leaves out `ask` and every self-wake tool, and it leaves out
/// `task` too: the probes could not show that the adapter tracks subagents (see the report).
pub const SEAT_TOOLS: [&str; 6] = ["read", "bash", "edit", "write", "grep", "glob"];

/// A comma-joined setting as a list.
pub fn csv(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn setting<'a>(plan: &'a LaunchPlan, key: &str) -> Option<&'a str> {
    plan.settings
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
}

/// Build the arguments for `omp`. `hook` is the path of Pulse Factory's own extension, and
/// `session_dir` holds the seat's OMP session files.
pub fn build_argv(
    plan: &LaunchPlan,
    hook: &Path,
    session_dir: &Path,
    brief_root: Option<&Path>,
) -> Result<Vec<String>, AgentError> {
    let mut a: Vec<String> = Vec::new();
    a.push("--model".into());
    a.push(plan.model.clone());
    if let Some(level) = setting(plan, "thinking") {
        a.push("--thinking".into());
        a.push(level.to_string());
    }
    a.push("--no-extensions".into());
    a.push("--no-rules".into());
    for dir in csv(setting(plan, "plugins").unwrap_or_default()) {
        a.push("--plugin-dir".into());
        a.push(dir);
    }
    let skills = csv(setting(plan, "skills").unwrap_or_default());
    if skills.is_empty() {
        // No declared skill: no skill at all, never "all of them".
        a.push("--no-skills".into());
    } else {
        a.push(format!("--skills={}", skills.join(",")));
    }
    a.push("--approval-mode".into());
    a.push("yolo".into());
    a.push("--tools".into());
    a.push(SEAT_TOOLS.join(","));
    a.push("--hook".into());
    a.push(hook.display().to_string());
    a.push("--session-dir".into());
    a.push(session_dir.display().to_string());

    let briefs = csv(setting(plan, "brief").unwrap_or_default());
    for b in &briefs {
        let path = match brief_root {
            Some(root) if !Path::new(b).is_absolute() => root.join(b),
            _ => Path::new(b).to_path_buf(),
        };
        a.push(format!("@{}", path.display()));
    }
    match setting(plan, "message") {
        Some(m) => a.push(m.to_string()),
        None if briefs.is_empty() => {
            return Err(AgentError::Refused(
                "the launch plan has no first message".to_string(),
            ));
        }
        None => {}
    }
    Ok(a)
}
