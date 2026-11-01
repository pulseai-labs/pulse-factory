//! The run directory (ADR-0007): `queue/ out/ package/ plugins/<name>/ manifest.json run.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pf_workflow::pin::Pin;
use serde::{Deserialize, Serialize};

/// A plugin the run pinned: the copy that seats launch from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCopy {
    pub name: String,
    pub version: String,
    pub min_version: String,
    pub path: PathBuf,
}

/// `run.json`: what a run is. It is written last, so a half-made run directory has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJson {
    pub run_id: String,
    pub spine: String,
    pub workflow: String,
    pub params: BTreeMap<String, String>,
    /// `live` or `paper`.
    pub env: String,
    pub ledger: PathBuf,
    pub plugin_roots: Vec<PathBuf>,
    pub paper: Option<PathBuf>,
    pub forge_repo: String,
    pub default_branch: String,
    pub target_dir: String,
    pub plugins: Vec<PluginCopy>,
    pub created: String,
}

/// `manifest.json`: the pins of the run's copies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub package: Pin,
    pub plugins: Vec<Pin>,
}

impl Manifest {
    pub fn all(&self) -> Vec<Pin> {
        let mut v = vec![self.package.clone()];
        v.extend(self.plugins.iter().cloned());
        v
    }
}

pub fn runs_dir(state: &Path) -> PathBuf {
    state.join("runs")
}

/// The run ID for a spine and a number: dots become dashes.
pub fn run_id_for(spine: &str, n: u32) -> String {
    format!("{}-{n}", spine.replace('.', "-"))
}

/// Make the next free run directory. `create_dir` fails if the directory exists, so two arms
/// cannot take one number. `taken` says that the store already knows a run ID: the store lives
/// outside the run directory, so a failed arm that removed its directory still leaves its rows,
/// and its ID must not be handed out again.
pub fn create_run_dir(
    state: &Path,
    spine: &str,
    taken: &dyn Fn(&str) -> bool,
) -> Result<(String, PathBuf), String> {
    let runs = runs_dir(state);
    std::fs::create_dir_all(&runs).map_err(|e| format!("cannot make {}: {e}", runs.display()))?;
    for n in 1..100_000u32 {
        let id = run_id_for(spine, n);
        if taken(&id) {
            continue;
        }
        let dir = runs.join(&id);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((id, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot make {}: {e}", dir.display())),
        }
    }
    Err("no free run number".to_string())
}

/// Copy a directory tree. A symlink stays a symlink.
pub fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot make {}: {e}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot stat {}: {e}", src.display()))?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&src).map_err(|e| format!("{}: {e}", src.display()))?;
            std::os::unix::fs::symlink(&target, &dst)
                .map_err(|e| format!("cannot link {}: {e}", dst.display()))?;
        } else if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)
                .map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), dst.display()))?;
        }
    }
    Ok(())
}

/// Copy the listed files of a package, keeping the relative layout.
pub fn copy_files(from: &Path, to: &Path, files: &[PathBuf]) -> Result<(), String> {
    for rel in files {
        let src = from.join(rel);
        let dst = to.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot make {}: {e}", parent.display()))?;
        }
        std::fs::copy(&src, &dst)
            .map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), dst.display()))?;
    }
    Ok(())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_run_id_is_never_handed_out_again() {
        let state = tempfile::tempdir().unwrap();
        let taken = |id: &str| id == "s1-1" || id == "s1-2";
        let (id, dir) = create_run_dir(state.path(), "s1", &taken).unwrap();
        assert_eq!(id, "s1-3");
        assert!(dir.is_dir());
        let (id, _) = create_run_dir(state.path(), "s1", &taken).unwrap();
        assert_eq!(id, "s1-4");
    }
}
