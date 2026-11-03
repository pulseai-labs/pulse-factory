//! A workflow package: a directory that holds `workflow.yaml`, the briefs, the rule files and an
//! optional `scripts/` directory.
//!
//! Loading a package checks that every brief and rule file the workflow names exists inside the
//! package, and that each rule file parses and belongs to its role. A command of an `on_answer`
//! hook that names a file of the package, as `{run.package}/<path>`, must name a file that the
//! package pins. The files in `scripts/` are the scripts that such hooks run.

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
    /// The files in the package's `scripts/` directory, relative to the package, sorted. A
    /// run pins them like the briefs. The directory is optional.
    pub scripts: Vec<PathBuf>,
}

/// The directory of a package that holds the scripts the workflow's commands run.
pub const SCRIPTS_DIR: &str = "scripts";

/// The placeholder prefix that names a file of the run's copy of the package.
const PACKAGE_REF: &str = "{run.package}/";

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

/// The plain files of `<dir>/scripts`, as paths relative to the package, sorted. A directory,
/// a symbolic link or any other entry in it refuses the package: a pin hashes plain files.
fn list_scripts(dir: &Path) -> Result<Vec<PathBuf>, PackageError> {
    let scripts_dir = dir.join(SCRIPTS_DIR);
    let entries = match std::fs::read_dir(&scripts_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(PackageError::Io {
                path: scripts_dir,
                why: e.to_string(),
            });
        }
    };
    let mut scripts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PackageError::Io {
            path: scripts_dir.clone(),
            why: e.to_string(),
        })?;
        let name = entry.file_name().to_string_lossy().to_string();
        let kind = entry.file_type().map_err(|e| PackageError::Io {
            path: entry.path(),
            why: e.to_string(),
        })?;
        if !kind.is_file() {
            return Err(PackageError::Bad {
                path: format!("{SCRIPTS_DIR}/{name}"),
                why: "a script must be a plain file: not a directory and not a link".to_string(),
            });
        }
        scripts.push(PathBuf::from(SCRIPTS_DIR).join(name));
    }
    scripts.sort();
    Ok(scripts)
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

        let scripts = list_scripts(dir)?;
        let package = Package {
            dir: dir.to_path_buf(),
            file,
            rules,
            briefs,
            scripts,
        };
        package.check_hook_files()?;
        Ok(package)
    }

    /// A command of an `on_answer` hook may run a file of the package. The file must be one the
    /// run pins, or the run would run a file that nothing checks.
    fn check_hook_files(&self) -> Result<(), PackageError> {
        let pinned = self.pinned_files();
        for track in &self.file.tracks {
            for stage in &track.stages {
                let hooks = stage
                    .park_on
                    .iter()
                    .map(|p| &p.on_answer)
                    .chain(std::iter::once(&stage.stop_on_answer));
                for command in hooks.flatten() {
                    for item in &command.argv {
                        let Some(rest) = item.strip_prefix(PACKAGE_REF) else {
                            continue;
                        };
                        let named = inside(rest)?;
                        if !pinned.contains(&named) {
                            return Err(PackageError::Bad {
                                path: item.clone(),
                                why: format!(
                                    "the `on_answer` hook of stage `{}` runs a file the package does not pin; put it in `{SCRIPTS_DIR}/`",
                                    stage.name
                                ),
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Every file a run pins (ADR-0007): the workflow file, the briefs, the rule files and the
    /// scripts, as paths relative to the package, sorted.
    pub fn pinned_files(&self) -> Vec<PathBuf> {
        let mut files = vec![PathBuf::from("workflow.yaml")];
        files.extend(self.briefs.iter().cloned());
        files.extend(self.scripts.iter().cloned());
        files.extend(self.file.roles.iter().map(|r| PathBuf::from(&r.rules)));
        files.sort();
        files.dedup();
        files
    }
}
