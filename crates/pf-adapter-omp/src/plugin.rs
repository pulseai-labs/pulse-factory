//! Finding a plugin by name and version. It reads directories only and makes no prompt.

use std::path::{Path, PathBuf};

use pf_engine::port::agent::{AgentError, ResolvedPlugin};

use crate::version::version_at_least;

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn manifest_version(dir: &Path) -> Option<String> {
    for rel in [".claude-plugin/plugin.json", "package.json", "plugin.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(rel)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
            return Some(ver.to_string());
        }
    }
    None
}

/// Candidates under one root. A plugin is `<root>/<name>`, `<root>/<name>/<version>` or
/// `<root>/<marketplace>/<name>/<version>`.
fn candidates(root: &Path, name: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut add_name_dir = |dir: PathBuf| {
        if let Some(v) = manifest_version(&dir) {
            out.push((v, dir.clone()));
        }
        for sub in subdirs(&dir) {
            let v = manifest_version(&sub).or_else(|| {
                sub.file_name()
                    .and_then(|n| n.to_str())
                    .filter(|n| n.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    .map(str::to_string)
            });
            if let Some(v) = v {
                out.push((v, sub));
            }
        }
    };
    let direct = root.join(name);
    if direct.is_dir() {
        add_name_dir(direct);
    }
    for market in subdirs(root) {
        let nested = market.join(name);
        if nested.is_dir() {
            add_name_dir(nested);
        }
    }
    out
}

/// The highest version of `name` under the roots that is at least `min_version`.
pub fn resolve(
    roots: &[PathBuf],
    name: &str,
    min_version: &str,
) -> Result<ResolvedPlugin, AgentError> {
    let mut best: Option<(String, PathBuf)> = None;
    let mut found_lower: Option<String> = None;
    for root in roots {
        for (version, path) in candidates(root, name) {
            if !version_at_least(&version, min_version) {
                found_lower = Some(version);
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(b, _)| version_at_least(&version, b))
            {
                best = Some((version, path));
            }
        }
    }
    match (best, found_lower) {
        (Some((version, path)), _) => Ok(ResolvedPlugin {
            name: name.to_string(),
            version,
            path: path.display().to_string(),
        }),
        (None, Some(v)) => Err(AgentError::Refused(format!(
            "plugin {name} is at version {v}, below the minimum {min_version}"
        ))),
        (None, None) => Err(AgentError::Refused(format!(
            "plugin {name} was not found in any plugin directory"
        ))),
    }
}
