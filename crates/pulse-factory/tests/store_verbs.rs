//! `pulse-factory store backups` and `store restore` (#85; ADR-0003, ADR-0004, ADR-0005, ADR-0007,
//! ADR-0012; `r2.s2.w2`). A stub `systemctl` on `PATH` stands in for the user manager. The verbs run
//! without a controller, and none is on the device allowlist.

mod store_support;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use pf_engine::port::store::{AdmissionOutcome, Store};
use pf_store_sqlite::SqliteStore;
use rusqlite::Connection;
use store_support::{Sandbox, chat_host, sleeper, store_bytes, text};

const STAMP: &str = "20261118T010203000Z";

/// A sandbox whose store holds the migrated schema and one project (`P-1`), and a backup of it that
/// the store would have written before migration 9.
fn with_backup() -> (Sandbox, PathBuf) {
    let sb = Sandbox::new();
    {
        let store = sb.open_store();
        add_project(&store, "alpha");
    }
    let backup = sb.make_backup("v8-v9", STAMP);
    (sb, backup)
}

fn add_project(store: &SqliteStore, name: &str) {
    use pf_engine::port::store::{
        CONTROLLER_SCOPE, Commit, Effect, NewAdmission, NewProject, ProjectKind,
    };
    let c = Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: format!("add-{name}"),
        content: format!("{{\"name\":\"{name}\"}}"),
        admission: NewAdmission {
            kind: "project_add".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t1".to_string(),
        },
        effects: vec![Effect::AddProject(NewProject {
            name: name.to_string(),
            root: format!("/work/{name}"),
            created_at: "t1".to_string(),
            kind: ProjectKind::Folder,
            remote_key: None,
        })],
        ledger: None,
    };
    store.commit(c).unwrap();
}

fn project_names(path: &Path) -> Vec<String> {
    let store = SqliteStore::open(path).unwrap();
    store
        .projects()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

fn files_named(dir: &Path, prefix: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix))
        .collect();
    found.sort();
    found
}

// ------------------------------------------------------------------ backups

#[test]
fn backups_lists_each_backup_with_its_versions_its_time_and_its_size() {
    let (sb, backup) = with_backup();
    let size = std::fs::metadata(&backup).unwrap().len();
    let out = sb.run(&["store", "backups"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let listed = text(&out.stdout);
    assert!(listed.contains(&backup.display().to_string()), "{listed}");
    assert!(listed.contains("v8") && listed.contains("v9"), "{listed}");
    assert!(listed.contains("2026-11-18T01:02:03"), "{listed}");
    assert!(listed.contains(&size.to_string()), "{listed}");

    let out = sb.run(&["store", "backups", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    let list = v["backups"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["path"], backup.display().to_string());
    assert_eq!(list[0]["from_version"], 8);
    assert_eq!(list[0]["to_version"], 9);
    assert_eq!(list[0]["time"], "2026-11-18T01:02:03.000Z");
    assert_eq!(list[0]["size_bytes"], size);
}

#[test]
fn backups_with_none_says_so_and_a_missing_store_is_no_error() {
    let sb = Sandbox::new();
    let out = sb.run(&["store", "backups"]);
    assert!(out.status.success());
    assert!(
        text(&out.stdout).contains("no backups"),
        "{}",
        text(&out.stdout)
    );
    let out = sb.run(&["store", "backups", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["backups"], serde_json::json!([]));
}

#[test]
fn backups_lists_the_newest_first_and_skips_a_file_that_is_no_backup() {
    let sb = Sandbox::new();
    drop(sb.open_store());
    let a = sb.make_backup("v8-v9", "20261118T010203000Z");
    let b = sb.make_backup("v9-v10", "20261119T010203000Z");
    std::fs::write(sb.state.join("store.sqlite.notes.bak"), "x").unwrap();
    std::fs::write(sb.state.join("other.bak"), "x").unwrap();
    let out = sb.run(&["store", "backups", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let paths: Vec<&str> = v["backups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [b.display().to_string(), a.display().to_string()]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------------ the refusals

fn assert_refused(out: &std::process::Output, needle: &str) {
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("refused"), "{err}");
    assert!(err.contains(needle), "the refusal names {needle:?}: {err}");
}

#[test]
fn restore_is_refused_while_a_controller_answers_on_the_socket() {
    let (sb, backup) = with_backup();
    let before = store_bytes(&sb.store_path());
    let _listener = UnixListener::bind(sb.socket_path()).unwrap();
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert_refused(&out, "controller");
    assert_eq!(
        store_bytes(&sb.store_path()),
        before,
        "the store is untouched"
    );
    assert!(files_named(&sb.state, "store.sqlite.replaced").is_empty());
}

#[test]
fn a_stale_socket_file_does_not_stop_a_restore() {
    // A socket file that no one answers is stale: it does not stop a restore.
    let (sb, backup) = with_backup();
    drop(UnixListener::bind(sb.socket_path()).unwrap());
    assert!(sb.socket_path().exists());
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn restore_is_refused_while_a_host_unit_of_this_server_is_live() {
    let (sb, backup) = with_backup();
    sb.list_units(&[sb.active_unit_line("S-1", 1)]);
    let before = store_bytes(&sb.store_path());
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert_refused(&out, &sb.unit("S-1", 1));
    assert_eq!(store_bytes(&sb.store_path()), before);
    // The probe asked the user manager for this server's units, all of them.
    let calls = sb.systemctl_calls();
    assert!(calls.contains("--user"), "{calls}");
    assert!(calls.contains("list-units"), "{calls}");
    assert!(calls.contains("--all"), "{calls}");
    assert!(
        calls.contains(&format!("pf-host-{}-*", sb.server_id)),
        "{calls}"
    );
}

#[test]
fn a_unit_of_another_server_and_a_dead_unit_do_not_stop_a_restore() {
    let (sb, backup) = with_backup();
    sb.list_units(&[
        "pf-host-someone-else-S-1-1.service loaded active running other server".to_string(),
        format!(
            "{}.service loaded failed failed Pulse Factory session host",
            sb.unit("S-2", 1)
        ),
        format!(
            "{}.service loaded inactive dead Pulse Factory session host",
            sb.unit("S-3", 1)
        ),
    ]);
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn restore_is_refused_while_a_detached_host_of_this_server_is_live() {
    let (sb, backup) = with_backup();
    let mut host = sleeper();
    sb.detached_host("S-1", 1, &host);
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert_refused(&out, "S-1-1");
    // The host ends: its directory is stale, and no live process matches its PID and start time.
    host.kill().unwrap();
    host.wait().unwrap();
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn a_host_pid_whose_start_time_differs_names_no_live_host() {
    // A reused PID: the process is alive, but it is not the host that wrote the file.
    let (sb, backup) = with_backup();
    let host = sleeper();
    let dir = sb.detached_host("S-1", 1, &host);
    std::fs::write(dir.join("host.pid"), format!("{} 1\n", host.id())).unwrap();
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let mut host = host;
    host.kill().unwrap();
    host.wait().unwrap();
}

#[test]
fn restore_is_refused_when_the_user_manager_cannot_answer() {
    let (sb, backup) = with_backup();
    sb.systemctl_cannot_answer();
    let before = store_bytes(&sb.store_path());
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("refused"), "{err}");
    assert!(err.contains("cannot tell"), "{err}");
    assert!(
        err.contains("systemctl"),
        "the reason names the probe: {err}"
    );
    assert_eq!(store_bytes(&sb.store_path()), before);
}

#[test]
fn restore_is_refused_for_a_backup_at_a_newer_schema() {
    let (sb, backup) = with_backup();
    Connection::open(&backup)
        .unwrap()
        .execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (99, 'future', 't')",
            [],
        )
        .unwrap();
    let before = store_bytes(&sb.store_path());
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert_refused(&out, "99");
    assert!(text(&out.stderr).contains("newer"), "{}", text(&out.stderr));
    assert_eq!(store_bytes(&sb.store_path()), before);
}

#[test]
fn restore_is_refused_for_a_truncated_backup_and_for_one_that_is_no_database() {
    let (sb, backup) = with_backup();
    let before = store_bytes(&sb.store_path());
    let truncated = sb.state.join("store.sqlite.v8-v9.20261119T000000000Z.bak");
    let bytes = std::fs::read(&backup).unwrap();
    std::fs::write(&truncated, &bytes[..bytes.len() / 3]).unwrap();
    let out = sb.run(&["store", "restore", truncated.to_str().unwrap()]);
    assert_refused(&out, "integrity");
    let junk = sb.state.join("junk.bak");
    std::fs::write(&junk, vec![7u8; 4096]).unwrap();
    let out = sb.run(&["store", "restore", junk.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let missing = sb.state.join("nothing.bak");
    let out = sb.run(&["store", "restore", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(store_bytes(&sb.store_path()), before);
    assert!(files_named(&sb.state, "store.sqlite.replaced").is_empty());
}

#[test]
fn the_verbs_refuse_a_seat_and_a_chat() {
    let (sb, backup) = with_backup();
    let before = store_bytes(&sb.store_path());
    for args in [
        vec!["store", "backups"],
        vec!["store", "restore", backup.to_str().unwrap()],
        vec!["store", "check-upgrade"],
    ] {
        let seat = sb
            .command(&args)
            .env("PF_SEAT_TOKEN", "x")
            .output()
            .unwrap();
        assert_refused(&seat, "PF_SEAT_TOKEN");
        let chat = sb
            .command(&args)
            .env("PULSE_FACTORY_CHAT", "S-1")
            .output()
            .unwrap();
        assert_refused(&chat, "PULSE_FACTORY_CHAT");
        // A chat marker that names no session is refused too: a marker is a marker.
        let odd = sb
            .command(&args)
            .env("PULSE_FACTORY_CHAT", "garbage")
            .output()
            .unwrap();
        assert_refused(&odd, "PULSE_FACTORY_CHAT");
    }
    assert_eq!(store_bytes(&sb.store_path()), before);
}

// ------------------------------------------------------------------ a restore that is allowed

#[test]
fn a_row_that_reads_live_while_no_unit_or_process_exists_does_not_refuse_a_restore() {
    let (sb, backup) = with_backup();
    {
        // After a crash, every host row can read live: the row says nothing of the process.
        let store = sb.open_store();
        chat_host(
            &store,
            &sb,
            "S-1",
            1,
            1,
            "/share/hosts/aa/pf-host",
            "/share/claude/2.1.0/claude",
        );
        assert_eq!(store.live_hosts().unwrap().len(), 1);
    }
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn a_valid_restore_moves_the_store_and_its_wal_and_shm_aside_and_puts_the_backup_in() {
    let (sb, backup) = with_backup();
    let backup_before = std::fs::read(&backup).unwrap();
    // The state of a crash: a second project exists only in the `-wal`. A writer holds the store
    // open while its three files are copied, so the copy holds the frames that no checkpoint wrote.
    let held = sb.open_store();
    add_project(&held, "beta");
    let crash = sb.dir.path().join("crash");
    std::fs::create_dir_all(&crash).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let from = format!("{}{suffix}", sb.store_path().display());
        let to = crash.join(format!("store.sqlite{suffix}"));
        std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("{from}: {e}"));
    }
    drop(held);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", sb.store_path().display()));
        std::fs::copy(
            crash.join(format!("store.sqlite{suffix}")),
            format!("{}{suffix}", sb.store_path().display()),
        )
        .unwrap();
    }
    let wal_before = std::fs::read(format!("{}-wal", sb.store_path().display())).unwrap();
    assert!(
        !wal_before.is_empty(),
        "the crashed store has frames in its -wal"
    );

    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let said = text(&out.stdout);
    // It names both paths: the restored store and the files it replaced.
    assert!(
        said.contains(&sb.store_path().display().to_string()),
        "{said}"
    );
    let replaced = files_named(&sb.state, "store.sqlite.replaced.");
    assert_eq!(replaced.len(), 3, "{replaced:?}");
    // The shortest name is the main file: `store.sqlite.replaced.<utc>`.
    let stamp = replaced[0]
        .strip_prefix("store.sqlite.replaced.")
        .unwrap()
        .to_string();
    assert!(
        said.contains(&format!("store.sqlite.replaced.{stamp}")),
        "{said}"
    );
    assert_eq!(
        replaced,
        [
            format!("store.sqlite.replaced.{stamp}"),
            format!("store.sqlite.replaced.{stamp}-shm"),
            format!("store.sqlite.replaced.{stamp}-wal"),
        ]
    );
    assert_eq!(
        std::fs::read(sb.state.join(format!("store.sqlite.replaced.{stamp}-wal"))).unwrap(),
        wal_before,
        "the replaced -wal is kept whole"
    );
    // The restored file is the backup at mode 0600, and the backup itself is as it was.
    let mode = std::fs::metadata(sb.store_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        backup_before,
        "nothing is written into the backup"
    );
    assert!(
        !sb.state.join("store.sqlite-wal").exists(),
        "no -wal of the replaced store stays beside the restored one"
    );
    // The store opens at the backup's version with the backup's rows, and none of the frames of the
    // replaced -wal: `beta` is gone.
    assert_eq!(project_names(&sb.store_path()), ["alpha"]);
}

#[test]
fn a_restore_with_no_store_file_puts_the_backup_in() {
    let (sb, backup) = with_backup();
    std::fs::remove_file(sb.store_path()).unwrap();
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", sb.store_path().display()));
    }
    let out = sb.run(&["store", "restore", backup.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(files_named(&sb.state, "store.sqlite.replaced").is_empty());
    assert_eq!(project_names(&sb.store_path()), ["alpha"]);
}
