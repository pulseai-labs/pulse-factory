//! `pulse-factory store check-upgrade` (#67, G8; ADR-0003, ADR-0007; `r2.s2.w2`). The new binary
//! runs it before an install: it reports what an upgrade needs to know and exits 3 when the upgrade
//! must not start. It reads the live store through a query-only connection that neither writes nor
//! checkpoints, and it asks the user manager (a stub `systemctl` here) which hosts are alive.

mod store_support;

use pf_engine::port::store::Store;
use serde_json::Value;
use store_support::{Sandbox, chat_host, sleeper, store_bytes, text};

fn report(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}\n{}", text(&out.stdout), text(&out.stderr)))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not a list: {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

const HOST_BIN: &str = "/share/hosts/aa11/pf-host";
const CLI_COPY: &str = "/share/claude/2.1.0/claude";

#[test]
fn it_reads_the_rows_in_the_wal_and_changes_not_one_byte_of_the_store() {
    let sb = Sandbox::new();
    // The writer stays open and never checkpoints: the host row and its record exist only as frames
    // of the `-wal`, as after a crash.
    let writer = sb.open_store();
    let unit = chat_host(&writer, &sb, "S-1", 1, 1, HOST_BIN, CLI_COPY);
    sb.list_units(&[sb.active_unit_line("S-1", 1)]);
    let wal = format!("{}-wal", sb.store_path().display());
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    // A running controller has read these rows already. A reader that finds no read mark at the last
    // frame moves one in the `-shm`, and SQLite has no read that avoids it (`immutable=1` would skip
    // the `-wal`, which the spec forbids). So the writer reads first, and the verb joins a mark that
    // exists.
    assert_eq!(writer.live_hosts().unwrap().len(), 1);
    let before = store_bytes(&sb.store_path());

    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    let hosts = v["live_hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 1, "{v}");
    assert_eq!(hosts[0]["unit"], unit);
    assert_eq!(hosts[0]["protocol"], 1);
    assert_eq!(hosts[0]["host_binary"], HOST_BIN);

    // The store, its `-wal` and its `-shm` are as they were: nothing was written or checkpointed.
    let after = store_bytes(&sb.store_path());
    assert_eq!(after[0], before[0], "the store file");
    assert_eq!(after[1], before[1], "the -wal");
    assert_eq!(after[2], before[2], "the -shm");
    drop(writer);
}

#[test]
fn it_exits_three_and_names_the_run_when_a_run_is_active_and_a_migration_is_pending() {
    let sb = Sandbox::new();
    sb.version_eight_store();
    sb.activate_run("run-7");
    let before = store_bytes(&sb.store_path());
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["schema_version"], 1, "the contract version");
    assert_eq!(v["store_version"], 8);
    assert_eq!(v["pending_migrations"][0]["version"], 9);
    assert_eq!(v["pending_migrations"][0]["name"], "launch_records");
    assert_eq!(strings(&v["active_runs"]), ["run-7"]);
    assert_eq!(v["can_start"], false);
    let refusals = v["refusals"].as_array().unwrap();
    assert_eq!(refusals.len(), 1, "{v}");
    assert_eq!(refusals[0]["kind"], "run_active_migration_pending");
    assert_eq!(strings(&refusals[0]["runs"]), ["run-7"]);
    // The check wrote nothing to the version-8 store.
    assert_eq!(store_bytes(&sb.store_path())[0], before[0]);

    // The text form names the run too.
    let out = sb.run(&["store", "check-upgrade"]);
    assert_eq!(out.status.code(), Some(3));
    let said = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(said.contains("run-7"), "{said}");
    assert!(said.contains("migration"), "{said}");
}

#[test]
fn an_active_run_with_nothing_pending_does_not_stop_the_upgrade() {
    let sb = Sandbox::new();
    let store = sb.open_store();
    drop(store);
    sb.activate_run("run-7");
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["pending_migrations"], serde_json::json!([]));
    assert_eq!(strings(&v["active_runs"]), ["run-7"]);
    assert_eq!(v["can_start"], true);
    assert_eq!(v["refusals"], serde_json::json!([]));
}

#[test]
fn it_exits_three_and_names_the_host_when_the_binarys_range_drops_its_protocol() {
    let sb = Sandbox::new();
    let store = sb.open_store();
    let old = chat_host(&store, &sb, "S-1", 1, 0, HOST_BIN, CLI_COPY);
    let new = chat_host(&store, &sb, "S-2", 1, 9, HOST_BIN, CLI_COPY);
    let fine = chat_host(&store, &sb, "S-3", 1, 1, HOST_BIN, CLI_COPY);
    sb.list_units(&[
        sb.active_unit_line("S-1", 1),
        sb.active_unit_line("S-2", 1),
        sb.active_unit_line("S-3", 1),
    ]);
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["protocol"]["min"], 1);
    assert_eq!(v["protocol"]["max"], 1);
    assert_eq!(v["binary_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["can_start"], false);
    let dropped: Vec<&Value> = v["refusals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "protocol_dropped")
        .collect();
    let named: Vec<&str> = dropped
        .iter()
        .map(|r| r["unit"].as_str().unwrap())
        .collect();
    assert_eq!(named, [old.as_str(), new.as_str()], "{v}");
    assert!(!named.contains(&fine.as_str()));
    assert_eq!(dropped[0]["protocol"], 0);
    assert_eq!(dropped[1]["protocol"], 9);
    // The text form names each host.
    let out = sb.run(&["store", "check-upgrade"]);
    assert_eq!(out.status.code(), Some(3));
    let said = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(said.contains(&old) && said.contains(&new), "{said}");
    drop(store);
}

#[test]
fn it_reports_the_host_binary_and_the_cli_copy_paths_that_live_records_name() {
    let sb = Sandbox::new();
    let store = sb.open_store();
    chat_host(&store, &sb, "S-1", 1, 1, HOST_BIN, CLI_COPY);
    chat_host(
        &store,
        &sb,
        "S-2",
        1,
        1,
        "/share/hosts/bb22/pf-host",
        "/share/claude/2.0.9/claude",
    );
    // A host that ended: its binary and its copy are not needed.
    let gone = chat_host(
        &store,
        &sb,
        "S-3",
        1,
        1,
        "/share/hosts/cc33/pf-host",
        "/share/claude/1.0.0/claude",
    );
    end_host(&sb, &gone);
    sb.list_units(&[sb.active_unit_line("S-1", 1), sb.active_unit_line("S-2", 1)]);
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(
        strings(&v["host_binaries"]),
        ["/share/hosts/aa11/pf-host", "/share/hosts/bb22/pf-host"]
    );
    assert_eq!(
        strings(&v["cli_copies"]),
        ["/share/claude/2.0.9/claude", "/share/claude/2.1.0/claude"]
    );
    let hosts = v["live_hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 2);
    assert_eq!(strings(&hosts[0]["cli_copies"]), [CLI_COPY]);
    assert_eq!(hosts[0]["session"], "S-1");
    assert_eq!(hosts[0]["incarnation"], 1);
    assert_eq!(hosts[0]["launcher"], "systemd");
    drop(store);

    // The text form lists them too.
    let out = sb.run(&["store", "check-upgrade"]);
    assert_eq!(out.status.code(), Some(0));
    let said = text(&out.stdout);
    assert!(said.contains(HOST_BIN) && said.contains(CLI_COPY), "{said}");
    assert!(!said.contains("cc33") && !said.contains("1.0.0"), "{said}");
}

fn end_host(sb: &Sandbox, unit: &str) {
    use pf_engine::port::store::{
        AdmissionOutcome, CONTROLLER_SCOPE, Commit, Effect, NewAdmission, Store,
    };
    let store = sb.open_store();
    store
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: format!("end-{unit}"),
            content: format!("{{\"unit\":\"{unit}\"}}"),
            admission: NewAdmission {
                kind: "host_ended".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: None,
                role: None,
                model: None,
                attempt: None,
                admitted_at: "t3".to_string(),
            },
            effects: vec![Effect::HostEnded {
                unit: unit.to_string(),
                ended_at: "t3".to_string(),
                exit: "{\"code\":0}".to_string(),
            }],
            ledger: None,
        })
        .unwrap();
}

#[test]
fn a_row_that_reads_live_but_no_unit_or_process_backs_is_not_reported_and_stops_nothing() {
    let sb = Sandbox::new();
    let store = sb.open_store();
    // A protocol the range drops, on a row that reads live: the unit is gone, so nothing is stranded.
    chat_host(&store, &sb, "S-1", 1, 0, HOST_BIN, CLI_COPY);
    sb.list_units(&[]);
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["live_hosts"], serde_json::json!([]));
    assert_eq!(v["host_binaries"], serde_json::json!([]));
    assert_eq!(v["cli_copies"], serde_json::json!([]));
    drop(store);
}

#[test]
fn a_detached_host_that_is_alive_confirms_its_row() {
    let sb = Sandbox::new();
    let store = sb.open_store();
    chat_host(&store, &sb, "S-1", 1, 1, HOST_BIN, CLI_COPY);
    let mut host = sleeper();
    sb.detached_host("S-1", 1, &host);
    sb.list_units(&[]);
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["live_hosts"].as_array().unwrap().len(), 1, "{v}");
    host.kill().unwrap();
    host.wait().unwrap();
    drop(store);
}

#[test]
fn on_a_version_eight_store_with_no_hosts_table_it_reports_no_hosts_and_exits_zero() {
    let sb = Sandbox::new();
    sb.version_eight_store();
    // No run is active, so the pending migration does not stop the upgrade.
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["store_version"], 8);
    assert_eq!(v["supported_store_version"], 10);
    // Migration 9 (launch records) and migration 10 (the chat host cursor) are both pending.
    assert_eq!(v["pending_migrations"].as_array().unwrap().len(), 2);
    assert_eq!(v["pending_migrations"][1]["version"], 10);
    assert_eq!(v["pending_migrations"][1]["name"], "chat_host_cursor");
    assert_eq!(v["active_runs"], serde_json::json!([]));
    assert_eq!(v["live_hosts"], serde_json::json!([]));
    assert_eq!(v["can_start"], true);
    // With no live row, the user manager is not asked: a first upgrade needs no systemctl.
    sb.systemctl_cannot_answer();
    let out = sb.run(&["store", "check-upgrade"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn a_new_machine_with_no_store_has_nothing_to_check() {
    let sb = Sandbox::new();
    assert!(!sb.store_path().exists());
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["store_version"], 0);
    assert_eq!(v["live_hosts"], serde_json::json!([]));
    assert!(!sb.store_path().exists(), "the check creates no store");
}

#[test]
fn it_exits_one_when_rows_read_live_and_the_user_manager_cannot_answer() {
    let sb = Sandbox::new();
    let store = sb.open_store();
    chat_host(&store, &sb, "S-1", 1, 1, HOST_BIN, CLI_COPY);
    drop(store);
    sb.systemctl_cannot_answer();
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.contains("cannot tell") && err.contains("systemctl"),
        "{err}"
    );
}

#[test]
fn it_exits_one_on_a_store_it_cannot_read() {
    let sb = Sandbox::new();
    std::fs::write(sb.store_path(), vec![7u8; 4096]).unwrap();
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!text(&out.stderr).is_empty());
}

/// `r2.s2.w3`: the range the verb reports and enforces is the host client's, not a constant of the
/// verb's own.
#[test]
fn the_protocol_range_is_the_host_clients() {
    let sb = Sandbox::new();
    let out = sb.run(&["store", "check-upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let v = report(&out);
    assert_eq!(v["protocol"]["min"], pf_host::protocol::PROTO_MIN, "{v}");
    assert_eq!(v["protocol"]["max"], pf_host::protocol::PROTO_MAX, "{v}");
    assert_eq!(
        pulse_factory::store_verbs::HOST_PROTOCOLS,
        pf_host::protocol::PROTO_MIN..=pf_host::protocol::PROTO_MAX
    );
}
