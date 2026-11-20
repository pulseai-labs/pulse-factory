//! Unit, slice and directory names. One function builds each, so a name round-trips to the session
//! and the incarnation it came from (SPINE.md G9).

use std::path::{Path, PathBuf};

/// The prefix of every host unit.
pub const UNIT_PREFIX: &str = "pf-host-";

/// True for a name made of characters a unit name and a directory name can carry without escaping.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        && !name.starts_with('.')
}

/// Check the three parts of a host's name.
pub fn check(server: &str, session: &str) -> Result<(), String> {
    for (what, value) in [("server", server), ("session", session)] {
        if !plain(value) {
            return Err(format!(
                "the {what} id {value:?} is not 1 to 64 characters of A-Z a-z 0-9 - _ . : (not first a dot)"
            ));
        }
    }
    Ok(())
}

/// The unit of a host: `pf-host-<server>-<session>-<incarnation>`.
pub fn unit_name(server: &str, session: &str, incarnation: u64) -> Result<String, String> {
    check(server, session)?;
    Ok(format!("{UNIT_PREFIX}{server}-{session}-{incarnation}"))
}

/// The slice of a server's hosts: `pf-hosts-<server>.slice`.
pub fn slice_name(server: &str) -> Result<String, String> {
    check(server, "x")?;
    Ok(format!("pf-hosts-{server}.slice"))
}

/// The session and the incarnation in a unit name of this server. The incarnation is the digits
/// after the last hyphen, so a session id may hold hyphens.
pub fn parse_unit(server: &str, unit: &str) -> Option<(String, u64)> {
    let unit = unit.strip_suffix(".service").unwrap_or(unit);
    let rest = unit
        .strip_prefix(UNIT_PREFIX)?
        .strip_prefix(server)?
        .strip_prefix('-')?;
    let (session, incarnation) = rest.rsplit_once('-')?;
    if session.is_empty() || !plain(session) {
        return None;
    }
    Some((session.to_string(), incarnation.parse().ok()?))
}

/// The directory of a detached host: `<runtime>/pf-hosts/<server>/<session>-<incarnation>/`.
pub fn detached_dir(runtime: &Path, server: &str, session: &str, incarnation: u64) -> PathBuf {
    runtime
        .join("pf-hosts")
        .join(server)
        .join(format!("{session}-{incarnation}"))
}

/// The secrets file of a unit: `<runtime>/pulse-factory/secrets/<unit>.json`.
pub fn secrets_path(runtime: &Path, unit: &str) -> PathBuf {
    runtime
        .join("pulse-factory")
        .join("secrets")
        .join(format!("{unit}.json"))
}

/// The exit file of a unit: `<state>/hosts/exits/<unit>.json`.
pub fn exit_path(state: &Path, unit: &str) -> PathBuf {
    state
        .join("hosts")
        .join("exits")
        .join(format!("{unit}.json"))
}
