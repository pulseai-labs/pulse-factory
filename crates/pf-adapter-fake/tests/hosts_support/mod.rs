//! A `SessionHosts` for the paper adapter's tests, built on `pf_host`'s detached launcher. It does
//! what the controller's host service does for a launch (record file, key file, start, attach) and
//! keeps no store: a second `TestHosts` over the same directories finds a host by its files, as a
//! restarted controller finds one by its rows.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pf_engine::port::host::*;
use pf_engine::port::store::NewLaunchRecord;
use pf_host::client::{self, ClientError, Conn, Event};
use pf_host::launcher::{Detached, HostState, Launcher, StartSpec};
use pf_host::protocol::SignalKind;
use serde_json::{Value, json};

static N: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory short enough for a Unix socket path, removed on drop.
pub struct Dirs {
    pub root: PathBuf,
}

impl Dirs {
    pub fn new() -> Dirs {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("pfa-{}-{n}", std::process::id()));
        std::fs::create_dir_all(root.join("rt")).unwrap();
        std::fs::create_dir_all(root.join("st")).unwrap();
        Dirs { root }
    }

    pub fn rt(&self) -> PathBuf {
        self.root.join("rt")
    }

    pub fn state(&self) -> PathBuf {
        self.root.join("st")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The `pf-host` binary: `PF_HOST_BIN`, else `pf-host` in the test binary's target directory.
pub fn host_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("PF_HOST_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .and_then(Path::parent)
        .expect("the test binary has a grandparent directory")
        .join("pf-host")
}

pub struct TestHosts {
    pub state: PathBuf,
    pub rt: PathBuf,
    pub server: String,
    launcher: Detached,
    /// How many times the launcher's `start` ran.
    pub starts: AtomicUsize,
    /// Every unit this value started, so a guard can stop them.
    pub units: Mutex<Vec<String>>,
}

pub fn session_name(key: &str) -> String {
    let hash = pf_host::sha256::sha256_hex(format!("seat:{key}").as_bytes());
    format!("s{}", &hash[..16])
}

fn masked(value: &str) -> String {
    format!("sha256:{}", pf_host::sha256::sha256_hex(value.as_bytes()))
}

impl TestHosts {
    pub fn new(dirs: &Dirs) -> Arc<TestHosts> {
        Arc::new(TestHosts {
            state: dirs.state(),
            rt: dirs.rt(),
            server: "tsrv000000ab".to_string(),
            launcher: Detached::new(dirs.rt()),
            starts: AtomicUsize::new(0),
            units: Mutex::new(Vec::new()),
        })
    }

    fn dir(&self) -> PathBuf {
        let d = self.state.join("hosts").join(&self.server);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn record_path(&self, session: &str, inc: u64) -> PathBuf {
        self.dir().join(format!("{session}-{inc}.record.json"))
    }

    pub fn key_path(&self, session: &str, inc: u64) -> PathBuf {
        self.dir().join(format!("{session}-{inc}.key"))
    }

    /// Stop every host this value started.
    pub fn stop_all(&self) {
        for unit in self.units.lock().unwrap().iter() {
            let _ = self.launcher.stop(unit);
        }
    }

    pub fn is_live(&self, unit: &str) -> bool {
        self.launcher.state(unit) == HostState::Live
    }

    fn attach(&self, unit: &str, session: &str, inc: u64) -> Result<HostHandle, HostError> {
        let key = std::fs::read_to_string(self.key_path(session, inc))
            .map_err(|e| HostError(format!("no key file for {unit}: {e}")))?;
        let record_text = std::fs::read_to_string(self.record_path(session, inc))
            .map_err(|e| HostError(format!("no record file for {unit}: {e}")))?;
        let socket =
            pf_host::names::detached_dir(&self.rt, &self.server, session, inc).join("host.sock");
        let (conn, hello) = client::connect(&socket, key.trim())
            .map_err(|e| HostError(format!("cannot attach to {unit}: {e}")))?;
        let want = pf_host::sha256::sha256_hex(record_text.as_bytes());
        if hello.record_sha256 != want || hello.session != session || hello.incarnation != inc {
            return Err(HostError(format!(
                "the host of {unit} named another record"
            )));
        }
        let exit = hello.exit.as_ref().map(|e| HostExit {
            code: e.code,
            signal: e.signal,
            at: e.at,
        });
        let record: Value = serde_json::from_str(&record_text).unwrap();
        let sess = Arc::new(ConnSession {
            conn: Mutex::new(conn),
            launcher: self.launcher.clone(),
            unit: unit.to_string(),
            files: vec![self.key_path(session, inc), self.record_path(session, inc)],
        });
        Ok(HostHandle::new(
            unit.to_string(),
            inc as u32,
            record,
            hello.child_pid,
            exit,
            sess,
        ))
    }
}

struct ConnSession {
    conn: Mutex<Conn>,
    launcher: Detached,
    unit: String,
    files: Vec<PathBuf>,
}

fn host_err(e: ClientError) -> HostError {
    HostError(e.to_string())
}

impl HostSession for ConnSession {
    fn read(&self, from: u64) -> Result<(), HostError> {
        self.conn.lock().unwrap().read_from(from).map_err(host_err)
    }

    fn events(&self, wait: Duration) -> Result<Vec<HostEvent>, HostError> {
        let mut conn = self.conn.lock().unwrap();
        conn.set_timeout(wait.max(Duration::from_millis(1)));
        let mut out = Vec::new();
        loop {
            match conn.next_event() {
                Ok(Event::Out { at, data }) => out.push(HostEvent::Out { at, data }),
                Ok(Event::Gap { from, to }) => out.push(HostEvent::Gap { from, to }),
                Ok(Event::Exit(e)) => out.push(HostEvent::Exit(HostExit {
                    code: e.code,
                    signal: e.signal,
                    at: e.at,
                })),
                Ok(Event::Error { .. }) => {}
                Err(ClientError::Timeout) => return Ok(out),
                Err(e) if out.is_empty() => return Err(host_err(e)),
                Err(_) => return Ok(out),
            }
        }
    }

    fn ack(&self, to: u64) -> Result<(), HostError> {
        self.conn.lock().unwrap().ack(to).map_err(host_err)
    }

    fn input(&self, data: &[u8]) -> Result<(), HostError> {
        self.conn.lock().unwrap().input(data).map_err(host_err)
    }

    fn signal(&self, signal: HostSignal) -> Result<(), HostError> {
        let sig = match signal {
            HostSignal::Interrupt => SignalKind::Interrupt,
            HostSignal::Terminate => SignalKind::Terminate,
            HostSignal::Kill => SignalKind::Kill,
        };
        self.conn.lock().unwrap().signal(sig).map_err(host_err)
    }

    fn exit_ack(&self) -> Result<(), HostError> {
        let res = self.conn.lock().unwrap().exit_ack().map_err(host_err);
        for f in &self.files {
            let _ = std::fs::remove_file(f);
        }
        res
    }

    fn stop(&self) -> Result<(), HostError> {
        let res = self
            .launcher
            .stop(&self.unit)
            .map_err(|e| HostError(e.to_string()));
        for f in &self.files {
            let _ = std::fs::remove_file(f);
        }
        res
    }
}

impl SessionHosts for TestHosts {
    fn launch(&self, launch: HostLaunch) -> Result<HostHandle, HostError> {
        let session = session_name(&launch.session_key);
        // The next incarnation: the first number with no record file.
        let mut inc = 1u64;
        while self.record_path(&session, inc).exists() {
            inc += 1;
        }
        // An older incarnation ends first (Q9).
        for older in 1..inc {
            let unit = pf_host::names::unit_name(&self.server, &session, older).unwrap();
            let _ = self.launcher.stop(&unit);
        }
        let unit = pf_host::names::unit_name(&self.server, &session, inc).unwrap();
        let fill = |s: &str| s.replace("{unit}", &unit);
        for f in &launch.files {
            let path = PathBuf::from(fill(f));
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).unwrap();
            }
            if !path.exists() {
                std::fs::write(&path, b"").unwrap();
            }
        }
        let mut env: BTreeMap<String, String> = launch
            .env
            .iter()
            .map(|(k, v)| (k.clone(), fill(v)))
            .collect();
        for (name, value) in &launch.secrets {
            env.insert(name.clone(), masked(value));
        }
        let mut settings = launch.settings.clone();
        if let Some(map) = settings.as_object_mut() {
            for name in launch.secrets.keys() {
                if let Some(v) = map.get_mut(&format!("env.{name}")) {
                    let plain = v.as_str().unwrap_or_default().to_string();
                    *v = Value::String(masked(&plain));
                }
            }
        }
        let transport = match launch.transport {
            HostTransport::Pipes => json!({"kind": "pipes"}),
            HostTransport::Terminal { rows, cols } => {
                json!({"kind": "pty", "rows": rows, "cols": cols})
            }
        };
        let body = json!({
            "argv": launch.argv.iter().map(|a| fill(a)).collect::<Vec<_>>(),
            "cwd": launch.cwd,
            "env": env,
            "secret_names": launch.secrets.keys().collect::<Vec<_>>(),
            "transport": transport,
            "runtime": launch.runtime,
            "settings": settings,
            "plugins": [],
            "skills": [],
            "rules_sha256": "",
            "run_binding": launch.run_binding,
            "resume": launch.resume,
        });
        let (canonical, _hash) = NewLaunchRecord::seal(&body.to_string()).unwrap();
        let record_path = self.record_path(&session, inc);
        std::fs::write(&record_path, canonical.as_bytes()).unwrap();
        let key = pf_host::sha256::sha256_hex(
            format!("{unit}-{:?}", std::time::SystemTime::now()).as_bytes(),
        );
        std::fs::write(self.key_path(&session, inc), &key).unwrap();
        let mut spec = StartSpec::new(
            &self.server,
            &session,
            inc,
            record_path,
            self.state.clone(),
            host_bin(),
            key,
        );
        spec.secrets = launch.secrets.clone();
        spec.exit_hold = Duration::from_secs(30);
        self.starts.fetch_add(1, Ordering::SeqCst);
        let started = self
            .launcher
            .start(&spec)
            .map_err(|e| HostError(e.to_string()))?;
        self.units.lock().unwrap().push(started.unit.clone());
        self.attach(&started.unit, &session, inc)
    }

    fn rebind(&self, unit: &str) -> Rebind {
        let Some((session, inc)) = pf_host::names::parse_unit(&self.server, unit) else {
            return Rebind::Lost {
                exit: json!("the unit is not of this server"),
            };
        };
        match self.launcher.state(unit) {
            HostState::Gone => Rebind::Lost {
                exit: json!("ended, cause unknown"),
            },
            _ => match self.attach(unit, &session, inc) {
                Ok(h) => Rebind::Survived(h),
                Err(e) => Rebind::Unreachable { why: e.0 },
            },
        }
    }

    fn launched_record(&self, unit: &str) -> Result<Option<Value>, HostError> {
        let Some((session, inc)) = pf_host::names::parse_unit(&self.server, unit) else {
            return Ok(None);
        };
        match std::fs::read_to_string(self.record_path(&session, inc)) {
            Ok(t) => Ok(serde_json::from_str(&t).ok()),
            Err(_) => Ok(None),
        }
    }
}

/// Stops every host of a `TestHosts` when it drops, pass or fail (Approach 17).
pub struct Guard(pub Arc<TestHosts>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.stop_all();
    }
}
