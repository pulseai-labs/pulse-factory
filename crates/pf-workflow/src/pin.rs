//! Pins (Approach 8, ADR-0007).
//!
//! A pin freezes a directory, or a list of files, at `arm`: `{name, version, path, content}`.
//! `content` is a hash over the sorted relative paths and the file contents. ADR-0008 names no
//! hash crate, so the hash runs the system's `sha256sum` through the command runner, one call
//! per file, so the output bound never cuts a hash. Pinning refuses when `sha256sum` is absent.
//!
//! [`check`] re-reads the pinned files and names what changed. Before each launch, the
//! controller checks every plugin pin, and a mismatch parks the unit with
//! [`Mismatch::park_reason`]. A changed plugin needs a new run (ADR-0007).
//!
//! The same function pins the workflow file, its briefs and its rule files ([`pin_package`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::commands::{CommandFailure, run_argv};
use crate::file::is_version;
use crate::package::Package;

/// The program that hashes.
pub const DEFAULT_HASH_PROGRAM: &str = "sha256sum";

/// Time for one hash call.
const HASH_TIMEOUT_SECONDS: u64 = 60;

/// How a pin reads its files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinScope {
    /// Every file under the directory. A new file is a change.
    Directory,
    /// The listed files only.
    Files,
}

/// One pinned directory, or one pinned list of files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    /// The hash over the sorted relative paths and file hashes.
    pub content: String,
    /// The hash of each file, by relative path. The check names a change with it.
    pub files: BTreeMap<String, String>,
    pub scope: PinScope,
}

/// Why a pin is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinError {
    Io {
        path: PathBuf,
        why: String,
    },
    /// `sha256sum` could not start.
    NoSha256sum {
        program: String,
        why: String,
    },
    /// `sha256sum` failed or printed something else than a hash.
    Hash {
        path: String,
        why: String,
    },
    BelowMinimum {
        name: String,
        version: String,
        minimum: String,
    },
    BadVersion {
        name: String,
        version: String,
    },
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinError::Io { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            PinError::NoSha256sum { program, why } => write!(
                f,
                "pinning needs sha256sum, and `{program}` could not start: {why}"
            ),
            PinError::Hash { path, why } => write!(f, "cannot hash {path}: {why}"),
            PinError::BelowMinimum {
                name,
                version,
                minimum,
            } => write!(
                f,
                "plugin {name} is at version {version}, below the minimum version {minimum}"
            ),
            PinError::BadVersion { name, version } => write!(
                f,
                "plugin {name} has version {version:?}, which is not whole numbers joined by dots"
            ),
        }
    }
}

impl std::error::Error for PinError {}

/// The hash program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hasher {
    program: String,
}

impl Hasher {
    pub fn new(program: &str) -> Hasher {
        Hasher {
            program: program.to_string(),
        }
    }
}

impl Default for Hasher {
    fn default() -> Hasher {
        Hasher::new(DEFAULT_HASH_PROGRAM)
    }
}

/// The hash in the first word of `sha256sum` output.
fn read_hash(out: &str) -> Option<String> {
    let word = out.split_whitespace().next()?.trim_start_matches('\\');
    (word.len() == 64 && word.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| word.to_ascii_lowercase())
}

impl Hasher {
    fn run(
        &self,
        argv: Vec<String>,
        stdin: Option<Vec<u8>>,
        what: &str,
    ) -> Result<String, PinError> {
        let out = run_argv(&argv, stdin, HASH_TIMEOUT_SECONDS).map_err(|f| match f {
            CommandFailure::Start(why) => PinError::NoSha256sum {
                program: self.program.clone(),
                why,
            },
            other => PinError::Hash {
                path: what.to_string(),
                why: other.to_string(),
            },
        })?;
        if out.exit_code != Some(0) {
            return Err(PinError::Hash {
                path: what.to_string(),
                why: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        read_hash(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| PinError::Hash {
            path: what.to_string(),
            why: "the output holds no SHA-256 hash".to_string(),
        })
    }

    /// The hash of one file. One call per file.
    fn file(&self, path: &Path) -> Result<String, PinError> {
        let shown = path.display().to_string();
        self.run(
            vec![self.program.clone(), "--".to_string(), shown.clone()],
            None,
            &shown,
        )
    }

    /// The hash of some bytes, through stdin.
    fn bytes(&self, data: Vec<u8>, what: &str) -> Result<String, PinError> {
        self.run(vec![self.program.clone()], Some(data), what)
    }
}

// ------------------------------------------------------------------ versions

fn parse_version(text: &str) -> Option<Vec<u64>> {
    if !is_version(text) {
        return None;
    }
    text.split('.').map(|p| p.parse().ok()).collect()
}

/// Compare two dotted versions part by part. A missing part counts as 0.
fn at_least(version: &[u64], minimum: &[u64]) -> bool {
    let len = version.len().max(minimum.len());
    for i in 0..len {
        let (v, m) = (
            version.get(i).copied().unwrap_or(0),
            minimum.get(i).copied().unwrap_or(0),
        );
        if v != m {
            return v > m;
        }
    }
    true
}

fn check_version(name: &str, version: &str, minimum: Option<&str>) -> Result<(), PinError> {
    let bad = |v: &str| PinError::BadVersion {
        name: name.to_string(),
        version: v.to_string(),
    };
    let have = parse_version(version).ok_or_else(|| bad(version))?;
    if let Some(min) = minimum {
        let need = parse_version(min).ok_or_else(|| bad(min))?;
        if !at_least(&have, &need) {
            return Err(PinError::BelowMinimum {
                name: name.to_string(),
                version: version.to_string(),
                minimum: min.to_string(),
            });
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ reading files

fn io(path: &Path, e: &std::io::Error) -> PinError {
    PinError::Io {
        path: path.to_path_buf(),
        why: e.to_string(),
    }
}

/// The relative paths of every file under `dir`, sorted. A symlink is one entry and is not followed.
fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), PinError> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .map_err(|e| io(dir, &e))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()
        .map_err(|e| io(dir, &e))?;
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let name = name.into_string().map_err(|_| PinError::Io {
            path: path.clone(),
            why: "the name is not UTF-8".to_string(),
        })?;
        let rel_path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let meta = std::fs::symlink_metadata(&path).map_err(|e| io(&path, &e))?;
        if meta.is_dir() {
            walk(&path, &rel_path, out)?;
        } else if meta.is_file() || meta.file_type().is_symlink() {
            out.push(rel_path);
        } else {
            return Err(PinError::Io {
                path,
                why: "it is neither a file, a directory nor a symlink".to_string(),
            });
        }
    }
    Ok(())
}

/// Hash one entry: a file by its bytes, a symlink by its target text.
fn hash_entry(hasher: &Hasher, base: &Path, rel: &str) -> Result<String, PinError> {
    let path = base.join(rel);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| io(&path, &e))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&path).map_err(|e| io(&path, &e))?;
        hasher.bytes(format!("link:{}", target.display()).into_bytes(), rel)
    } else {
        hasher.file(&path)
    }
}

fn manifest_hash(hasher: &Hasher, files: &BTreeMap<String, String>) -> Result<String, PinError> {
    let mut manifest = Vec::new();
    for (rel, hash) in files {
        manifest.extend_from_slice(rel.as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(hash.as_bytes());
        manifest.push(b'\n');
    }
    hasher.bytes(manifest, "the manifest")
}

fn hash_all(
    hasher: &Hasher,
    base: &Path,
    rels: &[String],
) -> Result<BTreeMap<String, String>, PinError> {
    rels.iter()
        .map(|rel| hash_entry(hasher, base, rel).map(|h| (rel.clone(), h)))
        .collect()
}

// ------------------------------------------------------------------ pin and check

/// Pin a directory with the system's `sha256sum`.
pub fn pin_dir(
    name: &str,
    version: &str,
    min_version: Option<&str>,
    path: &Path,
) -> Result<Pin, PinError> {
    pin_dir_with(&Hasher::default(), name, version, min_version, path)
}

/// Pin a directory. `min_version` refuses a plugin that is too old.
pub fn pin_dir_with(
    hasher: &Hasher,
    name: &str,
    version: &str,
    min_version: Option<&str>,
    path: &Path,
) -> Result<Pin, PinError> {
    check_version(name, version, min_version)?;
    let mut rels = Vec::new();
    walk(path, "", &mut rels)?;
    let files = hash_all(hasher, path, &rels)?;
    Ok(Pin {
        name: name.to_string(),
        version: version.to_string(),
        path: path.to_path_buf(),
        content: manifest_hash(hasher, &files)?,
        files,
        scope: PinScope::Directory,
    })
}

/// Pin the files of a package: the workflow file, its briefs and its rule files.
pub fn pin_package(package: &Package) -> Result<Pin, PinError> {
    pin_package_with(&Hasher::default(), package)
}

pub fn pin_package_with(hasher: &Hasher, package: &Package) -> Result<Pin, PinError> {
    let rels: Vec<String> = package
        .pinned_files()
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let files = hash_all(hasher, &package.dir, &rels)?;
    Ok(Pin {
        name: package.file.name.clone(),
        version: package.file.version.to_string(),
        path: package.dir.clone(),
        content: manifest_hash(hasher, &files)?,
        files,
        scope: PinScope::Files,
    })
}

/// One difference between a pin and the directory now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Changed(String),
    Added(String),
    Removed(String),
    /// The pin could not be checked. A pin that cannot be checked fails.
    Unreadable(String),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Change::Changed(p) => write!(f, "changed {p}"),
            Change::Added(p) => write!(f, "added {p}"),
            Change::Removed(p) => write!(f, "removed {p}"),
            Change::Unreadable(why) => write!(f, "could not be checked: {why}"),
        }
    }
}

/// A pin that no longer matches. It names what changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub name: String,
    pub changes: Vec<Change>,
}

impl Mismatch {
    /// The reason a launch parks with.
    pub fn park_reason(&self) -> String {
        format!("plugin {} changed since arm", self.name)
    }
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown: Vec<String> = self
            .changes
            .iter()
            .take(8)
            .map(ToString::to_string)
            .collect();
        write!(f, "{}: {}", self.park_reason(), shown.join(", "))?;
        if self.changes.len() > 8 {
            write!(f, " and {} more", self.changes.len() - 8)?;
        }
        Ok(())
    }
}

impl std::error::Error for Mismatch {}

/// Re-read a pinned directory, or its listed files, and name what changed.
pub fn check(pin: &Pin) -> Result<(), Mismatch> {
    check_with(&Hasher::default(), pin)
}

pub fn check_with(hasher: &Hasher, pin: &Pin) -> Result<(), Mismatch> {
    let fail = |changes: Vec<Change>| Mismatch {
        name: pin.name.clone(),
        changes,
    };
    let rels: Vec<String> = match pin.scope {
        PinScope::Files => pin.files.keys().cloned().collect(),
        PinScope::Directory => {
            let mut rels = Vec::new();
            match walk(&pin.path, "", &mut rels) {
                Ok(()) => rels,
                // A directory that went away has no files now.
                Err(PinError::Io { path, .. }) if path == pin.path && !pin.path.exists() => {
                    Vec::new()
                }
                Err(e) => return Err(fail(vec![Change::Unreadable(e.to_string())])),
            }
        }
    };
    // A listed file that is gone is a removal. Hash the others.
    let present: Vec<String> = rels
        .iter()
        .filter(|rel| std::fs::symlink_metadata(pin.path.join(rel)).is_ok())
        .cloned()
        .collect();
    let now = hash_all(hasher, &pin.path, &present)
        .map_err(|e| fail(vec![Change::Unreadable(e.to_string())]))?;

    let mut changes = Vec::new();
    changes.extend(
        now.iter()
            .filter(|(rel, hash)| pin.files.get(*rel).is_some_and(|old| old != *hash))
            .map(|(rel, _)| Change::Changed(rel.clone())),
    );
    changes.extend(
        now.keys()
            .filter(|rel| !pin.files.contains_key(*rel))
            .map(|rel| Change::Added(rel.clone())),
    );
    changes.extend(
        pin.files
            .keys()
            .filter(|rel| !now.contains_key(*rel))
            .map(|rel| Change::Removed(rel.clone())),
    );
    if changes.is_empty() {
        Ok(())
    } else {
        Err(fail(changes))
    }
}
