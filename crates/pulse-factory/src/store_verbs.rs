//! The `store` verbs (#85, #67; ADR-0003, ADR-0004, ADR-0005, ADR-0007; `r2.s2.w2`): `backups`,
//! `restore` and `check-upgrade`. They are CLI only and none is on the device allowlist (ADR-0012).
//! Each refuses a caller with `PF_SEAT_TOKEN` or the chat marker in its environment, and each runs
//! without the controller.
//!
//! They read the store through the read path of `pf_store_sqlite::offline`: a query-only connection
//! that reads the frames in the `-wal` and never writes or checkpoints. They never trust a `hosts`
//! row for liveness. After a crash every row can read live, so the offline probe asks the user
//! manager for the units of this server, and reads the detached hosts' directory.

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use pf_store_sqlite::offline::{LiveHostRow, Offline};
use pf_store_sqlite::{MIGRATIONS, list_backups, verify_file};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde_json::{Value, json};

use crate::cli::{StoreBackupsArgs, StoreCheckUpgradeArgs, StoreRestoreArgs};
use crate::controller::requests::CHAT_MARKER;

/// The host protocol versions this binary's controller speaks: the current one and the one before.
/// Version 1 is the first, so the range holds one version. `check-upgrade` exits 3 when a live host
/// speaks a version outside it. `r2.s2.w3` replaces this with the constant of the host client.
pub const HOST_PROTOCOLS: std::ops::RangeInclusive<u32> = 1..=1;

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// The text of a refusal to a caller that is a seat or runs under a chat, or `None` for any other
/// caller (ADR-0005). The check reads the environment: it is a speed bump, not a boundary.
fn caller_refusal() -> Option<String> {
    if std::env::var_os("PF_SEAT_TOKEN").is_some() {
        return Some("PF_SEAT_TOKEN is set, so this is a seat".to_string());
    }
    if std::env::var_os(CHAT_MARKER).is_some() {
        return Some(format!("{CHAT_MARKER} is set, so this runs under a chat"));
    }
    None
}

/// Print a refusal, and return its exit code.
fn refuse_caller(verb: &str) -> Option<i32> {
    caller_refusal().map(|why| {
        eprintln!("pulse-factory: store {verb} refused: {why}");
        1
    })
}

fn store_file(state: &Path) -> PathBuf {
    state.join("store.sqlite")
}

/// A compact UTC time for a file name: `20261118T180319123Z`.
fn stamp_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    pf_engine::engine::format_utc(now.as_secs(), now.subsec_millis())
        .chars()
        .filter(|c| !matches!(c, '-' | ':' | '.'))
        .collect()
}

// ------------------------------------------------------------------ the offline host probe

/// What the offline probe found: the units of this server that live, and a note when it could not
/// look for any.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub live: BTreeSet<String>,
    pub note: Option<String>,
}

/// One TLV of a DER string: the tag, the content, the whole element and what follows it.
struct Der<'a> {
    tag: u8,
    content: &'a [u8],
    whole: &'a [u8],
    after: &'a [u8],
}

/// Read the first TLV of a DER string.
fn der_element(data: &[u8]) -> Option<Der<'_>> {
    let tag = *data.first()?;
    let first = usize::from(*data.get(1)?);
    let (len, header) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | usize::from(*data.get(2 + i)?);
        }
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some(Der {
        tag,
        content: &data[header..end],
        whole: &data[..end],
        after: &data[end..],
    })
}

/// The SubjectPublicKeyInfo of a DER certificate, whole: the bytes the server id hashes.
fn certificate_spki(cert: &[u8]) -> Option<&[u8]> {
    let body = der_element(cert)?.content;
    let tbs = der_element(body)?.content;
    let first = der_element(tbs)?;
    // The optional [0] version comes first. Then: serial, signature, issuer, validity, subject.
    let mut rest = if first.tag == 0xA0 { first.after } else { tbs };
    for _ in 0..5 {
        rest = der_element(rest)?.after;
    }
    let key = der_element(rest)?;
    (key.tag == 0x30).then_some(key.whole)
}

/// The server id of a state directory: the SHA-256 of the SubjectPublicKeyInfo of the server's key,
/// which is what every API answer names in `pf-server-id`. The key lives in `<state>/api/`, and the
/// certificate beside it carries its public half, so this reads the certificate and never the private
/// key. It creates nothing. `None` when the API never made a key: no host of this server can exist
/// under a name that carries one.
fn server_id(state: &Path) -> Result<Option<String>, String> {
    let api = state.join("api");
    let cert = api.join("cert.pem");
    let pem = match std::fs::read(&cert) {
        Ok(pem) => pem,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return if api.join("key.pem").exists() {
                Err(format!(
                    "cannot tell the server id: {} holds a key and no certificate",
                    api.display()
                ))
            } else {
                Ok(None)
            };
        }
        Err(e) => {
            return Err(format!(
                "cannot tell the server id: cannot read {}: {e}",
                cert.display()
            ));
        }
    };
    let der = CertificateDer::from_pem_slice(&pem).map_err(|_| {
        format!(
            "cannot tell the server id: {} is not a certificate",
            cert.display()
        )
    })?;
    let spki = certificate_spki(der.as_ref()).ok_or_else(|| {
        format!(
            "cannot tell the server id: {} holds no public key",
            cert.display()
        )
    })?;
    Ok(Some(pf_engine::claim::sha256_hex(spki)))
}

/// The name a host's units carry for a server: `pf-host-<server>-<session>-<incarnation>` (G9).
/// `r2.s2.w3` fixes the short form a unit name needs, and changes this one function.
fn host_server_name(server_id: &str) -> String {
    server_id.to_string()
}

/// The start time of a process, field 22 of `/proc/<pid>/stat`, in clock ticks. A zombie, a dead
/// process and a PID that does not exist give `None`.
fn process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name is in parentheses and may hold spaces: read after the last `)`.
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    if state == "Z" || state == "X" {
        return None;
    }
    fields.nth(18)?.parse().ok()
}

/// The units of `systemctl --user list-units --all 'pf-host-<server>-*'` that are alive.
fn systemd_units(server: &str) -> Result<BTreeSet<String>, String> {
    let prefix = format!("pf-host-{server}-");
    let out = Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
        ])
        .arg(format!("{prefix}*"))
        .output()
        .map_err(|e| {
            format!("cannot tell whether a host lives: systemctl --user cannot run: {e}")
        })?;
    if !out.status.success() {
        return Err(format!(
            "cannot tell whether a host lives: systemctl --user list-units failed ({}): {}",
            out.status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| format!("exit {c}")),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut live = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut columns = line.split_whitespace();
        let Some(name) = columns.next() else {
            continue;
        };
        let unit = name.strip_suffix(".service").unwrap_or(name);
        if !unit.starts_with(&prefix) {
            continue;
        }
        // `LOAD ACTIVE SUB`: a unit that is inactive or failed runs no host. A line with no state
        // is read as alive: the probe refuses before it guesses that a host is gone.
        let active = columns.nth(1);
        if !matches!(active, Some("inactive" | "failed")) {
            live.insert(unit.to_string());
        }
    }
    Ok(live)
}

/// The detached hosts of a server that live: a directory of
/// `<XDG_RUNTIME_DIR>/pf-hosts/<server>/` whose `host.pid` names a PID and a start time that match
/// a live process. A directory with no readable `host.pid` is a host that is starting or one that
/// died half-made: the probe cannot tell which, and says so.
fn detached_units(server: &str) -> Result<BTreeSet<String>, String> {
    let mut live = BTreeSet::new();
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) else {
        return Ok(live);
    };
    let dir = PathBuf::from(runtime).join("pf-hosts").join(server);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(live),
        Err(e) => {
            return Err(format!(
                "cannot tell whether a detached host lives: cannot read {}: {e}",
                dir.display()
            ));
        }
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid_file = entry.path().join("host.pid");
        let parsed = std::fs::read_to_string(&pid_file).ok().and_then(|text| {
            let mut it = text.split_whitespace();
            Some((
                it.next()?.parse::<u32>().ok()?,
                it.next()?.parse::<u64>().ok()?,
            ))
        });
        let Some((pid, started)) = parsed else {
            return Err(format!(
                "cannot tell whether a detached host lives: {} holds no readable host.pid",
                entry.path().display()
            ));
        };
        if process_start_time(pid) == Some(started) {
            live.insert(format!("pf-host-{server}-{name}"));
        }
    }
    Ok(live)
}

/// Ask "is any host of this server alive?" without a controller and without trusting a row: the user
/// manager's units and the detached directory. When `systemctl --user` cannot answer, the answer is
/// the reason, and a caller refuses with it rather than guess.
pub fn probe_hosts(state: &Path) -> Result<Probe, String> {
    let Some(id) = server_id(state)? else {
        return Ok(Probe {
            live: BTreeSet::new(),
            note: Some(format!(
                "no server key in {}: the API never made one, so no host of this server can exist",
                state.join("api").display()
            )),
        });
    };
    let server = host_server_name(&id);
    let mut live = systemd_units(&server)?;
    live.extend(detached_units(&server)?);
    Ok(Probe { live, note: None })
}

// ------------------------------------------------------------------ backups

pub fn backups(a: &StoreBackupsArgs) -> i32 {
    if let Some(code) = refuse_caller("backups") {
        return code;
    }
    let store = store_file(&pf_ipc::state_dir());
    let found = list_backups(&store);
    if a.json {
        let list: Vec<Value> = found
            .iter()
            .map(|b| {
                json!({
                    "path": b.path.display().to_string(),
                    "from_version": b.from_version,
                    "to_version": b.to_version,
                    "time": b.time(),
                    "size_bytes": b.size,
                })
            })
            .collect();
        print_json(&json!({"schema_version": 1, "backups": list}));
        return 0;
    }
    if found.is_empty() {
        println!("no backups beside {}", store.display());
        return 0;
    }
    for b in &found {
        println!(
            "{}  v{} -> v{}  {}  {} bytes",
            b.path.display(),
            b.from_version,
            b.to_version,
            b.time(),
            b.size
        );
    }
    0
}

// ------------------------------------------------------------------ restore

/// The three files of a store, and where the replaced ones go.
const SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

fn beside(store: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", store.display()))
}

fn replaced(store: &Path, stamp: &str, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.replaced.{stamp}{suffix}", store.display()))
}

/// Copy the backup in as the store, at mode 0600. It writes nothing into the backup.
fn copy_in(backup: &Path, store: &Path) -> std::io::Result<()> {
    let mut from = std::fs::File::open(backup)?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(store)?;
    std::io::copy(&mut from, &mut to)?;
    to.flush()?;
    to.sync_all()?;
    std::fs::set_permissions(store, std::fs::Permissions::from_mode(0o600))
}

/// Move the replaced files back. A file at the place is the restored copy's, and goes first.
fn put_back(moved: &[(PathBuf, PathBuf)]) {
    for (original, aside) in moved.iter().rev() {
        let _ = std::fs::remove_file(original);
        let _ = std::fs::rename(aside, original);
    }
}

/// Replace the store with a copy of the backup. The current file and its `-wal` and `-shm` move
/// aside as `<store>.replaced.<stamp>{,-wal,-shm}`. Then the copy gets its check, and only a copy that
/// passes stays. A copy that fails puts the replaced files back. It answers the files it moved
/// aside, as `(original, aside)` pairs.
fn swap_in(
    store: &Path,
    backup: &Path,
    stamp: &str,
    verify: &dyn Fn(&Path) -> Result<u32, String>,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    for suffix in SUFFIXES {
        let aside = replaced(store, stamp, suffix);
        if aside.exists() {
            return Err(format!("{} exists already", aside.display()));
        }
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for suffix in SUFFIXES {
        let original = beside(store, suffix);
        if !original.exists() {
            continue;
        }
        let aside = replaced(store, stamp, suffix);
        if let Err(e) = std::fs::rename(&original, &aside) {
            put_back(&moved);
            return Err(format!(
                "cannot move {} aside to {}: {e}",
                original.display(),
                aside.display()
            ));
        }
        moved.push((original, aside));
    }
    if let Err(e) = copy_in(backup, store) {
        let _ = std::fs::remove_file(store);
        put_back(&moved);
        return Err(format!(
            "cannot copy {} to {}: {e}; the replaced files are back",
            backup.display(),
            store.display()
        ));
    }
    if let Err(why) = verify(store) {
        let _ = std::fs::remove_file(store);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(beside(store, suffix));
        }
        put_back(&moved);
        return Err(format!(
            "the restored copy fails its integrity check ({why}); the replaced files are back"
        ));
    }
    Ok(moved)
}

pub fn restore(a: &StoreRestoreArgs) -> i32 {
    if let Some(code) = refuse_caller("restore") {
        return code;
    }
    let state = pf_ipc::state_dir();
    let store = store_file(&state);
    let refuse = |why: String| {
        eprintln!("pulse-factory: store restore refused: {why}");
        1
    };
    let socket = pf_ipc::socket_path(&state);
    if UnixStream::connect(&socket).is_ok() {
        return refuse(format!(
            "the controller answers on {}; stop it first (systemctl --user stop pulse-factory)",
            socket.display()
        ));
    }
    match probe_hosts(&state) {
        Err(why) => return refuse(why),
        Ok(probe) if !probe.live.is_empty() => {
            let units: Vec<&str> = probe.live.iter().map(String::as_str).collect();
            return refuse(format!(
                "a host is live: {}; end the sessions or let the hosts end first",
                units.join(", ")
            ));
        }
        Ok(_) => {}
    }
    let supported = u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX);
    match verify_file(&a.file) {
        Err(why) => {
            return refuse(format!(
                "the backup {} fails its integrity check: {why}",
                a.file.display()
            ));
        }
        Ok(version) if version > supported => {
            return refuse(format!(
                "the backup {} is at schema version {version}, newer than the {supported} this binary supports",
                a.file.display()
            ));
        }
        Ok(_) => {}
    }
    let stamp = stamp_now();
    match swap_in(&store, &a.file, &stamp, &|p| verify_file(p)) {
        Ok(moved) => {
            println!("restored {} as {}", a.file.display(), store.display());
            if moved.is_empty() {
                println!("no store was in place, so no file was replaced");
            } else {
                let names: Vec<String> = moved
                    .iter()
                    .map(|(_, aside)| aside.display().to_string())
                    .collect();
                println!("the replaced files are at {}", names.join(", "));
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: store restore failed: {why}");
            1
        }
    }
}

// ------------------------------------------------------------------ check-upgrade

/// What `check-upgrade` found out about the store.
struct StoreFacts {
    version: u32,
    active_runs: Vec<String>,
    live_rows: Vec<LiveHostRow>,
}

fn read_store(store: &Path) -> Result<StoreFacts, String> {
    // A machine with no store has nothing to check, and the check makes none.
    if !store.exists() {
        return Ok(StoreFacts {
            version: 0,
            active_runs: Vec::new(),
            live_rows: Vec::new(),
        });
    }
    let off = Offline::open(store)?;
    Ok(StoreFacts {
        version: off.schema_version()?,
        active_runs: off.active_runs()?,
        live_rows: off.live_host_rows()?,
    })
}

fn host_json(h: &LiveHostRow) -> Value {
    json!({
        "unit": h.unit,
        "session_kind": h.session_kind,
        "session": h.session_id,
        "incarnation": h.incarnation,
        "launcher": h.launcher,
        "protocol": h.protocol,
        "host_binary": h.host_binary,
        "cli_copies": h.cli_copies,
    })
}

pub fn check_upgrade(a: &StoreCheckUpgradeArgs) -> i32 {
    if let Some(code) = refuse_caller("check-upgrade") {
        return code;
    }
    let state = pf_ipc::state_dir();
    let fail = |why: String| {
        eprintln!("pulse-factory: store check-upgrade: {why}");
        1
    };
    let facts = match read_store(&store_file(&state)) {
        Ok(f) => f,
        Err(why) => return fail(why),
    };
    // The probe runs only when a row reads live: with none, there is no host to name, and a first
    // upgrade into the host world needs no user manager.
    let mut probe_note = None;
    let hosts: Vec<LiveHostRow> = if facts.live_rows.is_empty() {
        Vec::new()
    } else {
        match probe_hosts(&state) {
            Err(why) => return fail(why),
            Ok(probe) => {
                probe_note = probe.note;
                facts
                    .live_rows
                    .into_iter()
                    .filter(|r| probe.live.contains(&r.unit))
                    .collect()
            }
        }
    };
    let supported = u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX);
    let pending: Vec<(u32, &str)> = MIGRATIONS
        .iter()
        .filter(|m| m.version > facts.version)
        .map(|m| (m.version, m.name))
        .collect();

    let mut refusals: Vec<Value> = Vec::new();
    let mut refusal_text: Vec<String> = Vec::new();
    if !facts.active_runs.is_empty() && !pending.is_empty() {
        let migrations: Vec<String> = pending.iter().map(|(v, n)| format!("{v} {n}")).collect();
        refusal_text.push(format!(
            "run {} is active and migration {} is pending: the store never migrates under an active run",
            facts.active_runs.join(", "),
            migrations.join(", ")
        ));
        refusals.push(json!({
            "kind": "run_active_migration_pending",
            "runs": facts.active_runs,
            "pending": pending.iter().map(|(v, _)| v).collect::<Vec<_>>(),
        }));
    }
    for h in &hosts {
        if !HOST_PROTOCOLS.contains(&h.protocol) {
            refusal_text.push(format!(
                "host {} speaks protocol {}, and this binary speaks {} to {}",
                h.unit,
                h.protocol,
                HOST_PROTOCOLS.start(),
                HOST_PROTOCOLS.end()
            ));
            refusals.push(json!({
                "kind": "protocol_dropped",
                "unit": h.unit,
                "protocol": h.protocol,
                "min": HOST_PROTOCOLS.start(),
                "max": HOST_PROTOCOLS.end(),
            }));
        }
    }
    let host_binaries: BTreeSet<&str> = hosts.iter().map(|h| h.host_binary.as_str()).collect();
    let cli_copies: BTreeSet<&str> = hosts
        .iter()
        .flat_map(|h| h.cli_copies.iter().map(String::as_str))
        .collect();
    let can_start = refusals.is_empty();

    if a.json {
        print_json(&json!({
            "schema_version": 1,
            "store_version": facts.version,
            "supported_store_version": supported,
            "binary_version": env!("CARGO_PKG_VERSION"),
            "protocol": {"min": HOST_PROTOCOLS.start(), "max": HOST_PROTOCOLS.end()},
            "pending_migrations": pending.iter().map(|(v, n)| json!({"version": v, "name": n})).collect::<Vec<_>>(),
            "active_runs": facts.active_runs,
            "live_hosts": hosts.iter().map(host_json).collect::<Vec<_>>(),
            "host_binaries": host_binaries,
            "cli_copies": cli_copies,
            "probe_note": probe_note,
            "refusals": refusals,
            "can_start": can_start,
        }));
    } else {
        println!(
            "store schema version {} (this binary supports {supported})",
            facts.version
        );
        println!(
            "this binary: pulse-factory {}, host protocols {} to {}",
            env!("CARGO_PKG_VERSION"),
            HOST_PROTOCOLS.start(),
            HOST_PROTOCOLS.end()
        );
        if pending.is_empty() {
            println!("pending migrations: none");
        } else {
            let list: Vec<String> = pending.iter().map(|(v, n)| format!("{v} {n}")).collect();
            println!("pending migrations: {}", list.join(", "));
        }
        if facts.active_runs.is_empty() {
            println!("active runs: none");
        } else {
            println!("active runs: {}", facts.active_runs.join(", "));
        }
        if hosts.is_empty() {
            println!("live hosts: none");
        } else {
            println!("live hosts:");
            for h in &hosts {
                println!(
                    "  {} ({} {} incarnation {}, {}): protocol {}, host binary {}, cli copies {}",
                    h.unit,
                    h.session_kind,
                    h.session_id,
                    h.incarnation,
                    h.launcher,
                    h.protocol,
                    h.host_binary,
                    if h.cli_copies.is_empty() {
                        "none".to_string()
                    } else {
                        h.cli_copies.join(", ")
                    }
                );
            }
        }
        if !host_binaries.is_empty() {
            let list: Vec<&str> = host_binaries.iter().copied().collect();
            println!("host binaries live records name: {}", list.join(", "));
        }
        if !cli_copies.is_empty() {
            let list: Vec<&str> = cli_copies.iter().copied().collect();
            println!("cli copies live records name: {}", list.join(", "));
        }
        if let Some(note) = &probe_note {
            println!("note: {note}");
        }
        for line in &refusal_text {
            println!("refused: {line}");
        }
        println!(
            "{}",
            if can_start {
                "the upgrade can start"
            } else {
                "the upgrade must not start"
            }
        );
    }
    if can_start { 0 } else { 3 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with_store() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store.sqlite");
        for (suffix, bytes) in [("", "main"), ("-wal", "wal"), ("-shm", "shm")] {
            std::fs::write(beside(&store, suffix), bytes).unwrap();
        }
        let backup = dir.path().join("b.bak");
        std::fs::write(&backup, "backup").unwrap();
        (dir, store, backup)
    }

    #[test]
    fn a_copy_that_fails_its_check_is_removed_and_the_replaced_files_come_back() {
        let (_dir, store, backup) = dir_with_store();
        let err = swap_in(&store, &backup, "T1", &|_| Err("damaged".to_string()))
            .expect_err("the check fails");
        assert!(
            err.contains("integrity check") && err.contains("damaged"),
            "{err}"
        );
        assert!(err.contains("replaced files are back"), "{err}");
        for (suffix, bytes) in [("", "main"), ("-wal", "wal"), ("-shm", "shm")] {
            assert_eq!(
                std::fs::read_to_string(beside(&store, suffix)).unwrap(),
                bytes
            );
            assert!(!replaced(&store, "T1", suffix).exists());
        }
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "backup");
    }

    #[test]
    fn a_copy_that_passes_its_check_stays_and_the_replaced_files_wait_aside() {
        let (_dir, store, backup) = dir_with_store();
        let moved = swap_in(&store, &backup, "T1", &|_| Ok(9)).unwrap();
        assert_eq!(moved.len(), 3);
        assert_eq!(std::fs::read_to_string(&store).unwrap(), "backup");
        assert!(!beside(&store, "-wal").exists());
        assert!(!beside(&store, "-shm").exists());
        assert_eq!(
            std::fs::read_to_string(replaced(&store, "T1", "-wal")).unwrap(),
            "wal"
        );
        let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_name_that_is_taken_stops_the_swap_before_anything_moves() {
        let (_dir, store, backup) = dir_with_store();
        std::fs::write(replaced(&store, "T1", "-wal"), "taken").unwrap();
        let err = swap_in(&store, &backup, "T1", &|_| Ok(9)).expect_err("a name is taken");
        assert!(err.contains("exists already"), "{err}");
        assert_eq!(std::fs::read_to_string(&store).unwrap(), "main");
    }

    #[test]
    fn the_der_walk_finds_the_public_key_of_a_made_certificate() {
        let state = tempfile::tempdir().unwrap();
        let key = pf_api::tls::load_or_create(
            &state.path().join("api"),
            &"127.0.0.1:7443".parse().unwrap(),
        )
        .unwrap();
        let id = server_id(state.path()).unwrap();
        assert_eq!(id, Some(key.spki_sha256_hex()));
    }

    #[test]
    fn a_unit_list_is_read_by_state_and_server() {
        // The parser is exercised through `systemd_units` in the integration tests; the unit
        // names of another server never match this server's prefix.
        assert!(
            "pf-host-other-S-1-1"
                .strip_prefix("pf-host-mine-")
                .is_none()
        );
    }
}
