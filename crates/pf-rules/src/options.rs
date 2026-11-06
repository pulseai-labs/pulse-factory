//! Long options by unique prefix (canonical issue #8).
//!
//! `git` reads a long option of a command by its exact name or by any unique prefix of it
//! (`parse-options`): `git push --forc` is `--force` when no other option starts that way, and an
//! error when two do. The guards of this crate read long options by exact name, so
//! `git commit --no-verif` passed. `git()` now rewrites each such word to the full name before any
//! guard reads it, and a prefix that two guarded options share is denied as `unparsed-command`.
//!
//! Only the options that a guard reads are listed. That is enough: a word that is a unique prefix
//! of one option in the full list of the tool is a prefix of that option in the guarded list too,
//! and no other guarded option shares it. A word that is unique here but ambiguous in git's full
//! list is one that git refuses, so no valid work is lost.
//!
//! Tools that read long options by exact name only need no list: `gh` (cobra and pflag) and `cargo`
//! (clap, without `infer_long_args`) accept no prefix, and `find` has single-dash expressions. No
//! guard reads a long option of `curl`, `wget` or `rm`, which do accept prefixes: the forge-host
//! check of `curl` and `wget` reads the values, and `rm` treats every dash word as an option.

use crate::shell::Word;

/// Long options of any git command that a guard reads.
///
/// Source: git-commit(1) `-n, --no-verify`; the same option in git-push(1), git-merge(1),
/// git-am(1) and git-cherry-pick(1), OPTIONS.
pub const ANY_OPTIONS: [&str; 1] = ["no-verify"];

/// Long options of `git push` that a guard reads. The last four take a value.
///
/// Source: git-push(1), OPTIONS: `--all` (alias `--branches`), `--mirror`, `--tags`, `--delete`,
/// `--force`, `--force-with-lease`, `--force-if-includes`, `--push-option`, `--repo`,
/// `--receive-pack` (alias `--exec`).
pub const PUSH_OPTIONS: [&str; 12] = [
    "all",
    "branches",
    "mirror",
    "tags",
    "delete",
    "force",
    "force-with-lease",
    "force-if-includes",
    "push-option",
    "repo",
    "receive-pack",
    "exec",
];

/// Long options of `git branch` that a guard reads.
///
/// Source: git-branch(1), OPTIONS: `-d, --delete`.
pub const BRANCH_OPTIONS: [&str; 1] = ["delete"];

/// Long options of `git clean` that a guard reads.
///
/// Source: git-clean(1), OPTIONS: `-n, --dry-run`.
pub const CLEAN_OPTIONS: [&str; 1] = ["dry-run"];

/// Long options of `git config` that a guard reads: the ones that read or remove (`--get` to
/// `--remove-section`, `--get-color`, `--get-colorbool`), the ones that write more (`--add`,
/// `--replace-all`), the ones that open the config in an editor or rename a section (`--edit`,
/// `--rename-section`), the ones that take a value (`--file` to `--url`), and the flags that name
/// no action (a scope, a format, a type, `--includes`, `--fixed-value`). A prefix of one
/// is written in full, so `--glo` reads as `--global` and `--bo` is refused as ambiguous, as git
/// refuses it.
///
/// Source: git-config(1), OPTIONS and ACTIONS; `--value` and `--url` are options of the `get` and
/// `set` verbs.
pub const CONFIG_OPTIONS: [&str; 39] = [
    "get",
    "get-all",
    "get-regexp",
    "get-urlmatch",
    "get-color",
    "get-colorbool",
    "list",
    "unset",
    "unset-all",
    "remove-section",
    "add",
    "replace-all",
    "edit",
    "rename-section",
    "file",
    "blob",
    "default",
    "comment",
    "type",
    "value",
    "url",
    "global",
    "local",
    "system",
    "worktree",
    "null",
    "name-only",
    "show-origin",
    "show-scope",
    "includes",
    "no-includes",
    "no-type",
    "bool",
    "int",
    "bool-or-int",
    "bool-or-str",
    "path",
    "expiry-date",
    "fixed-value",
];

/// Long options of `git switch` and `git checkout` that a guard reads: the ones that name a
/// branch to create or detach.
///
/// Source: git-switch(1), OPTIONS: `--create`, `--force-create`, `--detach`, `--orphan`;
/// git-checkout(1), OPTIONS: `--orphan`, `--detach`, `--patch`.
pub const SWITCH_OPTIONS: [&str; 5] = ["create", "force-create", "orphan", "detach", "patch"];

/// The guarded long options of one git subcommand.
pub fn git_options(sub: &str) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ANY_OPTIONS.to_vec();
    match sub {
        "push" => v.extend(PUSH_OPTIONS),
        "branch" => v.extend(BRANCH_OPTIONS),
        "clean" => v.extend(CLEAN_OPTIONS),
        "config" => v.extend(CONFIG_OPTIONS),
        "switch" | "checkout" => v.extend(SWITCH_OPTIONS),
        _ => {}
    }
    v
}

/// What a long option word is, read against a list of guarded options.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// The word is this option: its exact name, or a prefix that no other listed option shares.
    Name(&'static str),
    /// The word is a prefix of more than one listed option, and none is its exact name.
    Ambiguous(Vec<&'static str>),
    /// No listed option starts with the word.
    Other,
}

/// Read the text of a long option, without its `--` and without a `=value`.
pub fn resolve(word: &str, names: &[&'static str]) -> Resolved {
    if word.is_empty() {
        return Resolved::Other;
    }
    if let Some(exact) = names.iter().find(|n| **n == word) {
        return Resolved::Name(exact);
    }
    let mut hits: Vec<&'static str> = Vec::new();
    for n in names {
        if n.starts_with(word) && !hits.contains(n) {
            hits.push(n);
        }
    }
    match hits.len() {
        0 => Resolved::Other,
        1 => Resolved::Name(hits[0]),
        _ => Resolved::Ambiguous(hits),
    }
}

/// The words with each long option that is a prefix written in full, so the guards can match by
/// exact name. Words after a `--` are operands and stay as they are, and so does a word that holds
/// an expansion or a glob. A prefix that two guarded options share stays as written, and the first
/// such word comes back as a reason: git refuses it, so the caller denies the command.
pub fn canonical_words(rest: &[Word], names: &[&'static str]) -> (Vec<Word>, Option<String>) {
    let end = rest
        .iter()
        .position(|w| w.text == "--")
        .unwrap_or(rest.len());
    let mut out = rest.to_vec();
    let mut ambiguous: Option<String> = None;
    for w in &mut out[..end] {
        if w.dynamic || w.glob {
            continue;
        }
        let Some(long) = w.text.strip_prefix("--") else {
            continue;
        };
        let (name, value) = match long.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (long, None),
        };
        match resolve(name, names) {
            Resolved::Name(full) if full != name => {
                w.text = match value {
                    Some(v) => format!("--{full}={v}"),
                    None => format!("--{full}"),
                };
            }
            Resolved::Name(_) | Resolved::Other => {}
            Resolved::Ambiguous(all) => {
                let list: Vec<String> = all.iter().map(|n| format!("--{n}")).collect();
                ambiguous.get_or_insert_with(|| {
                    format!(
                        "the option --{name} starts more than one option ({}), and git refuses it",
                        list.join(", ")
                    )
                });
            }
        }
    }
    (out, ambiguous)
}
