//! A workflow package: a directory that holds `workflow.yaml`, the briefs and the rule files.
//!
//! Loading a package checks that every brief and rule file the workflow names exists inside the
//! package, and that each rule file parses and belongs to its role.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::file::WorkflowFile;
use crate::marked::ParseError;
use crate::rules::RoleRules;

/// Why a package is refused.
#[derive(Debug)]
pub enum PackageError {
    /// A file could not be read.
    Io { path: PathBuf, why: String },
    /// A file did not parse.
    Parse { path: PathBuf, error: ParseError },
    /// The workflow names a file the package does not hold, or one outside the package.
    Bad { path: String, why: String },
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageError::Io { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            PackageError::Parse { path, error } => write!(f, "{}: {error}", path.display()),
            PackageError::Bad { path, why } => write!(f, "{path}: {why}"),
        }
    }
}

impl std::error::Error for PackageError {}

/// A loaded package.
#[derive(Clone, Debug)]
pub struct Package {
    pub dir: PathBuf,
    pub file: WorkflowFile,
    /// The rule file of each role, by role name.
    pub rules: BTreeMap<String, RoleRules>,
    /// The brief files the stages name, relative to the package, sorted, without repeats.
    pub briefs: Vec<PathBuf>,
}

fn inside(path: &str) -> Result<PathBuf, PackageError> {
    let p = Path::new(path);
    let ok = !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)));
    if ok {
        Ok(p.to_path_buf())
    } else {
        Err(PackageError::Bad {
            path: path.to_string(),
            why: "a package file must be a relative path inside the package, with no `..`"
                .to_string(),
        })
    }
}

fn read(path: &Path) -> Result<String, PackageError> {
    std::fs::read_to_string(path).map_err(|e| PackageError::Io {
        path: path.to_path_buf(),
        why: e.to_string(),
    })
}

impl Package {
    /// Load the package at `dir`.
    pub fn load(dir: &Path) -> Result<Package, PackageError> {
        let workflow_path = dir.join("workflow.yaml");
        let text = read(&workflow_path)?;
        let file = WorkflowFile::parse(&text).map_err(|error| PackageError::Parse {
            path: workflow_path.clone(),
            error,
        })?;

        let mut rules = BTreeMap::new();
        for role in &file.roles {
            let rel = inside(&role.rules)?;
            let path = dir.join(&rel);
            if !path.is_file() {
                return Err(PackageError::Bad {
                    path: role.rules.clone(),
                    why: format!("the rule file of role `{}` does not exist", role.name),
                });
            }
            let name = rel
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parsed =
                RoleRules::parse(&read(&path)?, &name).map_err(|error| PackageError::Parse {
                    path: path.clone(),
                    error,
                })?;
            if parsed.role != role.name {
                return Err(PackageError::Bad {
                    path: role.rules.clone(),
                    why: format!(
                        "the rule file holds role `{}`, but the workflow assigns it to role `{}`",
                        parsed.role, role.name
                    ),
                });
            }
            rules.insert(role.name.clone(), parsed);
        }

        let mut briefs: Vec<PathBuf> = Vec::new();
        for track in &file.tracks {
            for stage in &track.stages {
                for brief in &stage.brief {
                    let rel = inside(brief)?;
                    if !dir.join(&rel).is_file() {
                        return Err(PackageError::Bad {
                            path: brief.clone(),
                            why: format!("the brief of stage `{}` does not exist", stage.name),
                        });
                    }
                    briefs.push(rel);
                }
            }
        }
        briefs.sort();
        briefs.dedup();

        Ok(Package {
            dir: dir.to_path_buf(),
            file,
            rules,
            briefs,
        })
    }

    /// Every file a run pins (ADR-0007): the workflow file, the briefs and the rule files, as
    /// paths relative to the package, sorted.
    pub fn pinned_files(&self) -> Vec<PathBuf> {
        let mut files = vec![PathBuf::from("workflow.yaml")];
        files.extend(self.briefs.iter().cloned());
        files.extend(self.file.roles.iter().map(|r| PathBuf::from(&r.rules)));
        files.sort();
        files.dedup();
        files
    }
}
