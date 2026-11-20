//! Support for the tests of the `store` verbs (`r2.s2.w2`): a state directory with a server key, a
//! stub `systemctl` on `PATH` that stands in for the user manager, a runtime directory for the
//! detached hosts, and rows in a real store. Nothing here needs a controller, systemd or a network.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry, NewAdmission,
    NewHost, NewLaunchRecord, Store,
};
use pf_store_sqlite::SqliteStore;
use rusqlite::Connection;

pub const BIN: &str = env!("CARGO_BIN_EXE_pulse-factory");

const MIGRATIONS_0001_TO_0008: [(&str, &str); 8] = [
    (
        "init",
        include_str!("../../../pf-store-sqlite/migrations/0001_init.sql"),
    ),
    (
        "requests",
        include_str!("../../../pf-store-sqlite/migrations/0002_requests.sql"),
    ),
    (
        "projects",
        include_str!("../../../pf-store-sqlite/migrations/0003_projects.sql"),
    ),
    (
        "devices",
        include_str!("../../../pf-store-sqlite/migrations/0004_devices.sql"),
    ),
    (
        "hierarchy",
        include_str!("../../../pf-store-sqlite/migrations/0005_hierarchy.sql"),
    ),
    (
        "session_options",
        include_str!("../../../pf-store-sqlite/migrations/0006_session_options.sql"),
    ),
    (
        "settings",
        include_str!("../../../pf-store-sqlite/migrations/0007_settings.sql"),
    ),
    (
        "run_project",
        include_str!("../../../pf-store-sqlite/migrations/0008_run_project.sql"),
    ),
];

/// A state directory, a runtime directory and a stub `systemctl`, in one temporary directory.
pub struct Sandbox {
    pub dir: tempfile::TempDir,
    pub state: PathBuf,
    pub runtime: PathBuf,
    pub stub: PathBuf,
    pub server_id: String,
}

impl Sandbox {
    /// A sandbox with a host server name, so the verbs can name the units of this server. The stub
    /// `systemctl` lists no unit until a test sets some.
    pub fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let runtime = dir.path().join("run");
        let stub = dir.path().join("bin");
        for d in [&state, &runtime, &stub] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let script = stub.join("systemctl");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             echo \"$@\" >> \"$STUB_DIR/calls\"\n\
             if [ -e \"$STUB_DIR/fail\" ]; then\n\
               echo 'Failed to connect to bus: No medium found' >&2\n\
               exit 1\n\
             fi\n\
             cat \"$STUB_DIR/units\" 2>/dev/null\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The host server name (`r2.s2.w3`, G9): 12 hex digits in `<state>/hosts/server`, made by the
        // controller. It is not the API key's id, and the sandbox makes no API key.
        let name = pf_host::sha256::sha256_hex(dir.path().display().to_string().as_bytes())[..12]
            .to_string();
        std::fs::create_dir_all(state.join("hosts")).unwrap();
        std::fs::write(state.join("hosts/server"), format!("{name}\n")).unwrap();
        Sandbox {
            dir,
            state,
            runtime,
            stub,
            server_id: name,
        }
    }

    /// Make the API key of this state directory, as `controller --api-listen` does. Its server id
    /// is the SHA-256 of the key's public half, and it is not the host server name.
    pub fn make_api_key(&self) -> String {
        pf_api::tls::load_or_create(
            &pf_api::tls::api_dir(&self.state),
            &"127.0.0.1:7443".parse().unwrap(),
        )
        .unwrap()
        .spki_sha256_hex()
    }

    pub fn store_path(&self) -> PathBuf {
        self.state.join("store.sqlite")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.state.join("controller.sock")
    }

    /// The command of one `pulse-factory` call, with this sandbox's environment: no seat token, no
    /// chat marker, the stub `systemctl` first on `PATH`.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.stub.display()))
            .env("HOME", self.dir.path())
            .env("PF_STATE_DIR", &self.state)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("STUB_DIR", &self.stub);
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// The units the stub `systemctl` lists: one `list-units` line each.
    pub fn list_units(&self, lines: &[String]) {
        std::fs::write(self.stub.join("units"), lines.join("\n") + "\n").unwrap();
    }

    /// The `list-units` line of an active unit of this server.
    pub fn active_unit_line(&self, session: &str, incarnation: u32) -> String {
        format!(
            "{}.service loaded active running Pulse Factory session host",
            self.unit(session, incarnation)
        )
    }

    pub fn unit(&self, session: &str, incarnation: u32) -> String {
        format!("pf-host-{}-{session}-{incarnation}", self.server_id)
    }

    /// Make the stub `systemctl` fail, as it does with no user manager.
    pub fn systemctl_cannot_answer(&self) {
        std::fs::write(self.stub.join("fail"), "").unwrap();
    }

    /// What the stub `systemctl` was asked, one call a line.
    pub fn systemctl_calls(&self) -> String {
        std::fs::read_to_string(self.stub.join("calls")).unwrap_or_default()
    }

    /// Open the store of the state directory with the binary's own migrations.
    pub fn open_store(&self) -> SqliteStore {
        SqliteStore::open(self.store_path()).unwrap()
    }

    /// A store file at version 8 in the state directory, with an ended run and one project.
    pub fn version_eight_store(&self) {
        let conn = Connection::open(self.store_path()).unwrap();
        for (i, (name, sql)) in MIGRATIONS_0001_TO_0008.iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
                rusqlite::params![i64::try_from(i + 1).unwrap(), name],
            )
            .unwrap();
        }
        conn.execute_batch(
            "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
                 VALUES ('run-1', 'wf', '/l.md', 'ended', 't0', 't0'); \
             INSERT INTO projects (id, name, root, created_at, kind) \
                 VALUES ('P-1', 'alpha', '/work/alpha', 't2', 'folder');",
        )
        .unwrap();
    }

    /// Mark a run of a version-8 or later store active.
    pub fn activate_run(&self, id: &str) {
        Connection::open(self.store_path())
            .unwrap()
            .execute(
                "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
                 VALUES (?1, 'wf', '/l2.md', 'active', 't0', 't0')",
                [id],
            )
            .unwrap();
    }

    /// A backup of the store as it is now, made with `VACUUM INTO`, under the name the store would
    /// give it. `versions` is `v8-v9`, and `stamp` is `20261118T010203000Z`.
    pub fn make_backup(&self, versions: &str, stamp: &str) -> PathBuf {
        let path = self
            .state
            .join(format!("store.sqlite.{versions}.{stamp}.bak"));
        Connection::open(self.store_path())
            .unwrap()
            .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    /// A detached host directory of this server, with a `host.pid` that names `child`.
    pub fn detached_host(&self, session: &str, incarnation: u32, child: &Child) -> PathBuf {
        let dir = self
            .runtime
            .join("pf-hosts")
            .join(&self.server_id)
            .join(format!("{session}-{incarnation}"));
        std::fs::create_dir_all(&dir).unwrap();
        let pid = child.id();
        std::fs::write(
            dir.join("host.pid"),
            format!("{pid} {}\n", start_time(pid).unwrap()),
        )
        .unwrap();
        dir
    }
}

/// A process that lives until the test ends: a detached host's stand-in.
pub fn sleeper() -> Child {
    Command::new("sleep")
        .arg("60")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// The start time of a process, field 22 of `/proc/<pid>/stat`.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19)?.parse().ok()
}

pub fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

// ------------------------------------------------------------------ rows

fn commit(kind: &str, command_id: &str, content: &str, effects: Vec<Effect>) -> Commit {
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: content.to_string(),
        admission: NewAdmission {
            kind: kind.to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t1".to_string(),
        },
        effects,
        ledger: Some(LedgerEntry {
            ledger_file: "controller.ledger".to_string(),
            body: format!("{kind} {command_id}"),
        }),
    }
}

fn applied(c: Committed) {
    assert!(matches!(c, Committed::Applied { .. }), "{c:?}");
}

/// The body of a launch record that names a CLI copy and carries no secret value.
pub fn record_body(cli_copy: &str) -> String {
    serde_json::json!({
        "argv": ["claude"],
        "cwd": "/work/alpha",
        "env": {"HOME": "/home/dev"},
        "secret_names": [],
        "transport": {"kind": "pipes"},
        "runtime": {
            "id": "claude-code",
            "version": "2.1.0",
            "binary_path": cli_copy,
            "binary_sha256": "ab".repeat(32),
        },
        "settings": {},
        "plugins": [],
        "skills": [],
        "rules_sha256": "cd".repeat(32),
        "run_binding": null,
        "resume": "fresh",
    })
    .to_string()
}

/// Write revision 1 of a chat's launch record and the host row that started from it, through the
/// store port. The host row reads live: it has no end.
pub fn chat_host(
    store: &SqliteStore,
    sandbox: &Sandbox,
    session: &str,
    incarnation: u32,
    protocol: u32,
    host_binary: &str,
    cli_copy: &str,
) -> String {
    let body = record_body(cli_copy);
    let (_, hash) = NewLaunchRecord::seal(&body).unwrap();
    let unit = sandbox.unit(session, incarnation);
    let record = NewLaunchRecord {
        session_kind: "chat".to_string(),
        session_id: session.to_string(),
        incarnation,
        revision: 1,
        launcher: "systemd".to_string(),
        host_unit: unit.clone(),
        host_socket: format!("/run/user/1000/pf-hosts/{unit}/host.sock"),
        record: body,
        record_sha256: hash.clone(),
        created_at: "t1".to_string(),
    };
    applied(
        store
            .commit(commit(
                "launch_record_write",
                &format!("lr-{session}-{incarnation}"),
                &format!("{{\"session\":\"{session}\",\"sha256\":\"{hash}\"}}"),
                vec![Effect::LaunchRecordWrite(record)],
            ))
            .unwrap(),
    );
    let host = NewHost {
        unit: unit.clone(),
        session_kind: "chat".to_string(),
        session_id: session.to_string(),
        incarnation,
        launched_revision: 1,
        launched_record_sha256: hash.clone(),
        launcher: "systemd".to_string(),
        socket: format!("/run/user/1000/pf-hosts/{unit}/host.sock"),
        invocation_id: "inv-1".to_string(),
        host_binary: host_binary.to_string(),
        host_sha256: "aa".repeat(32),
        protocol,
        started_at: "t2".to_string(),
    };
    applied(
        store
            .commit(commit(
                "host_started",
                &format!("hs-{session}-{incarnation}"),
                &format!("{{\"unit\":\"{unit}\",\"sha256\":\"{hash}\"}}"),
                vec![Effect::HostStarted(host)],
            ))
            .unwrap(),
    );
    unit
}

/// The bytes of a store's three files, for a test that says a verb changed none of them.
///
/// A child process reads them. POSIX record locks belong to the process: when this process opened
/// and closed the `-shm` itself, it would drop the locks that a writer connection held in this
/// process has on it. The next reader would then find no live connection, rebuild the wal index and
/// rewrite the `-shm` header, and the test would blame the verb for what the helper did.
pub fn store_bytes(path: &Path) -> [Option<Vec<u8>>; 3] {
    let read = |name: std::ffi::OsString| {
        let out = Command::new("cat").arg(&name).output().expect("run cat");
        out.status.success().then_some(out.stdout)
    };
    let side = |suffix: &str| {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        read(name)
    };
    [
        read(path.as_os_str().to_os_string()),
        side("-wal"),
        side("-shm"),
    ]
}
