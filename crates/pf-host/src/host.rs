//! The host process (`pf-host run`): one per session. It starts the session's child on a PTY or
//! on pipes, keeps the output the controller has not acknowledged, relays input, resize and
//! signals, holds the child's exit status, and answers one attached controller on a Unix socket.
//!
//! The host knows nothing about what the child is. Threads: one reads the child's output, one
//! waits for the child to end, one writes the child's input, one accepts connections, and one
//! serves each connection. The main thread decides when the host ends.
//!
//! The attach checks (this module's `check_peer` and the key compare) stop mistakes and stale
//! connections, not a hostile process of the same user (Spike A, F8; ADR-0005). Under the detached
//! launcher the cgroup check is vacuous, because the host, the controller and the seats share one
//! cgroup, so the attach key is the only peer check there.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::{Value, json};

use crate::buffer::{Read as BufRead, ReplayBuffer};
use crate::exitfile;
use crate::peer::{self, Peer, Waker};
use crate::protocol::{
    ClientMsg, ExitInfo, FRAME_MAX, Hello, HostMsg, PROTO_MAX, PROTO_MIN, SignalKind, encode_host,
    take_client,
};
use crate::record::{Record, Transport};
use crate::sha256::sha256_hex;

/// How long the host waits for a child that was sent `SIGTERM` before it sends `SIGKILL`.
const TERM_GRACE: Duration = Duration::from_secs(5);
/// How long the host waits for the last output after the child's process ended.
const DRAIN_GRACE: Duration = Duration::from_millis(500);
/// The most bytes a connection keeps queued for a slow reader before it stops filling.
const WBUF_HIGH: usize = 1 << 20;
/// The most input bytes queued for the child at once.
const INPUT_QUEUE_MAX: usize = 4 << 20;
/// The most connections open at once. More are closed at once, uncounted.
const CONN_MAX: usize = 32;

/// Set by the `SIGTERM` handler.
static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

/// Which launcher started the host. It decides where a refusal is logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherKind {
    /// A transient systemd user unit. A refusal goes to stderr, the unit's journal.
    Systemd,
    /// A detached process. Its stderr has no reader, so a refusal goes to `<dir>/refusals.log`.
    Detached,
}

/// Everything `pf-host run` is given.
#[derive(Clone, Debug)]
pub struct RunArgs {
    /// The launch record file.
    pub record: PathBuf,
    pub session: String,
    pub incarnation: u64,
    /// The unit name, which names the exit file.
    pub unit: String,
    pub state_dir: PathBuf,
    /// The host's own directory: the socket, `host.pid` and the refusal log live in it.
    pub dir: PathBuf,
    /// The cgroup the controller is in. `None` skips the cgroup check.
    pub controller_cgroup: Option<String>,
    pub secrets: PathBuf,
    pub replay_bytes: usize,
    pub attach_wait: Duration,
    pub exit_hold: Duration,
    pub launcher: LauncherKind,
}

/// Why a host ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndReason {
    ExitAcked,
    HoldExpired,
    Terminated,
}

impl EndReason {
    fn text(self) -> &'static str {
        match self {
            EndReason::ExitAcked => "exit_acked",
            EndReason::HoldExpired => "hold_expired",
            EndReason::Terminated => "terminated",
        }
    }
}

struct Config {
    session: String,
    incarnation: u64,
    unit: String,
    state_dir: PathBuf,
    dir: PathBuf,
    controller_cgroup: Option<String>,
    attach_wait: Duration,
    exit_hold: Duration,
    launcher: LauncherKind,
    record_sha256: String,
    /// The attach key, in memory only.
    key: Vec<u8>,
    own_uid: u32,
    invocation_id: String,
    child_pid: u32,
}

struct State {
    buf: ReplayBuffer,
    output_eof: bool,
    /// The child's wait status: (code, signal). Set the moment the process ends.
    exit_raw: Option<(Option<i32>, Option<i32>)>,
    /// The exit, once the last output is in the buffer. A client sees it only now.
    exit: Option<ExitInfo>,
    finalized_at: Option<Instant>,
    exit_acked: bool,
    /// The id of the attached connection. 0 when none.
    holder: u64,
    next_id: u64,
    conns: HashMap<u64, Arc<Waker>>,
    refusals: u64,
}

struct Shared {
    cfg: Config,
    st: Mutex<State>,
    cv: Condvar,
    input: Mutex<Sender<Vec<u8>>>,
    input_pending: Arc<AtomicUsize>,
    master: Option<Mutex<Box<dyn MasterPty + Send>>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wake_all(&self, st: &State) {
        for w in st.conns.values() {
            w.wake();
        }
    }

    /// Count and log one refusal. The line holds the peer PID and the check, never key text.
    fn refuse(&self, peer_pid: i32, check: &str) {
        self.lock().refusals += 1;
        let line = format!(
            "pf-host: refused session={} incarnation={} peer_pid={} check={} at={}\n",
            self.cfg.session,
            self.cfg.incarnation,
            peer_pid,
            check,
            exitfile::now_secs()
        );
        match self.cfg.launcher {
            LauncherKind::Systemd => {
                let _ = std::io::stderr().write_all(line.as_bytes());
            }
            LauncherKind::Detached => {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(self.cfg.dir.join("refusals.log"))
                {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    }

    /// The peer checks of the host's side: the user, then the cgroup.
    fn check_peer(&self, peer: &Peer) -> bool {
        if peer.uid != self.cfg.own_uid {
            return false;
        }
        match &self.cfg.controller_cgroup {
            None => true,
            Some(want) => peer::cgroup_of(peer.pid).as_deref() == Some(want.as_str()),
        }
    }

    fn exit_file(&self, reason: &str, extra: Option<&str>) -> Value {
        let st = self.lock();
        json!({
            "written_by": "host",
            "unit": self.cfg.unit,
            "session": self.cfg.session,
            "incarnation": self.cfg.incarnation,
            "child_pid": self.cfg.child_pid,
            "exit": st.exit.as_ref().map(|e| json!({"code": e.code, "signal": e.signal, "at": e.at})),
            "reason": reason,
            "error": extra,
            "end": st.buf.end(),
            "acked_through": st.buf.base(),
            "unread_bytes": st.buf.len(),
            "gaps": st.buf.gaps().iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
            "record_sha256": self.cfg.record_sha256,
            "refusals": st.refusals,
            "written_at": exitfile::now_secs(),
        })
    }
}

/// The secrets file: the attach key and the child's secret variables.
struct Secrets {
    attach_key: String,
    env: Vec<(String, String)>,
}

fn read_secrets(path: &PathBuf) -> Result<Secrets, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("cannot read the secrets file {}: {e}", path.display()))?;
    // The file is deleted before anything else can fail, so a secret never stays on disk.
    std::fs::remove_file(path)
        .map_err(|e| format!("cannot delete the secrets file {}: {e}", path.display()))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("the secrets file is not JSON: {e}"))?;
    let attach_key = v
        .get("attach_key")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .ok_or("the secrets file has no `attach_key`")?
        .to_string();
    let mut env = Vec::new();
    if let Some(map) = v.get("env") {
        let map = map
            .as_object()
            .ok_or("`env` of the secrets file is not an object")?;
        for (k, val) in map {
            let s = val
                .as_str()
                .ok_or_else(|| format!("secret variable {k} is not a string"))?;
            env.push((k.clone(), s.to_string()));
        }
    }
    Ok(Secrets { attach_key, env })
}

/// What starting the child gave back.
struct Child {
    pid: u32,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    master: Option<Box<dyn MasterPty + Send>>,
}

fn start_child(rec: &Record, secrets: &[(String, String)]) -> Result<Child, String> {
    let mut env = rec.env.clone();
    for (k, v) in secrets {
        env.insert(k.clone(), v.clone());
    }
    match rec.transport {
        Transport::Pipes => {
            let mut cmd = Command::new(&rec.argv[0]);
            cmd.args(&rec.argv[1..])
                .current_dir(&rec.cwd)
                .env_clear()
                .envs(&env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .process_group(0);
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("cannot start {}: {e}", rec.argv[0]))?;
            let stdin = child.stdin.take().ok_or("the child has no stdin pipe")?;
            let stdout = child.stdout.take().ok_or("the child has no stdout pipe")?;
            let pid = child.id();
            // The waiter thread reaps the child by PID; dropping the handle neither waits nor kills.
            drop(child);
            Ok(Child {
                pid,
                reader: Box::new(stdout),
                writer: Box::new(stdin),
                master: None,
            })
        }
        Transport::Pty { rows, cols } => {
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("cannot open a PTY: {e}"))?;
            let mut cb = CommandBuilder::new(&rec.argv[0]);
            cb.args(&rec.argv[1..]);
            cb.cwd(&rec.cwd);
            cb.env_clear();
            for (k, v) in &env {
                cb.env(k, v);
            }
            let child = pair
                .slave
                .spawn_command(cb)
                .map_err(|e| format!("cannot start {}: {e}", rec.argv[0]))?;
            drop(pair.slave);
            let pid = child.process_id().ok_or("the PTY child has no PID")?;
            drop(child);
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| format!("cannot read the PTY: {e}"))?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|e| format!("cannot write the PTY: {e}"))?;
            Ok(Child {
                pid,
                reader,
                writer,
                master: Some(pair.master),
            })
        }
    }
}

/// Tell the service manager the host is ready: the socket is bound.
fn notify_ready(child_pid: u32) {
    use std::os::linux::net::SocketAddrExt;
    let Ok(path) = std::env::var("NOTIFY_SOCKET") else {
        return;
    };
    let addr = match path.strip_prefix('@') {
        Some(name) => std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()),
        None => std::os::unix::net::SocketAddr::from_pathname(&path),
    };
    let (Ok(addr), Ok(sock)) = (addr, UnixDatagram::unbound()) else {
        return;
    };
    let _ = sock.send_to_addr(
        format!("READY=1\nSTATUS=child {child_pid}\n").as_bytes(),
        &addr,
    );
}

/// Run a host to its end. Returns when the host should exit. `Err` is a start-up failure that has
/// already been written to the exit file.
pub fn run(args: &RunArgs) -> Result<(), String> {
    let fail = |why: String| -> String {
        let body = json!({
            "written_by": "host",
            "unit": args.unit,
            "session": args.session,
            "incarnation": args.incarnation,
            "reason": "start_failed",
            "error": why,
            "written_at": exitfile::now_secs(),
        });
        let _ = exitfile::write(&args.state_dir, &args.unit, &body, false);
        // A secrets file the host did not read yet must not stay on disk.
        let _ = std::fs::remove_file(&args.secrets);
        if args.launcher == LauncherKind::Detached {
            let _ = std::fs::remove_dir_all(&args.dir);
        }
        why
    };

    // The host's own directory at mode 0700, whatever made it.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&args.dir)
        .map_err(|e| fail(format!("cannot make {}: {e}", args.dir.display())))?;
    std::fs::set_permissions(&args.dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        fail(format!(
            "cannot set the mode of {}: {e}",
            args.dir.display()
        ))
    })?;

    let secrets = read_secrets(&args.secrets).map_err(&fail)?;
    let raw = std::fs::read(&args.record).map_err(|e| {
        fail(format!(
            "cannot read the record {}: {e}",
            args.record.display()
        ))
    })?;
    let record = Record::parse(&raw).map_err(|e| fail(format!("bad record: {e}")))?;
    let record_sha256 = sha256_hex(&raw);
    let child = start_child(&record, &secrets.env).map_err(&fail)?;
    drop(secrets.env);
    // From here a failure also ends the child's group: no host is left to find or stop it.
    let child_pgid = child.pid;
    let fail_and_end_child = |why: String| -> String {
        peer::kill_group(child_pgid, libc::SIGKILL);
        fail(why)
    };

    let (tx, rx) = channel::<Vec<u8>>();
    let input_pending = Arc::new(AtomicUsize::new(0));
    let cfg = Config {
        session: args.session.clone(),
        incarnation: args.incarnation,
        unit: args.unit.clone(),
        state_dir: args.state_dir.clone(),
        dir: args.dir.clone(),
        controller_cgroup: args.controller_cgroup.clone().filter(|c| !c.is_empty()),
        attach_wait: args.attach_wait,
        exit_hold: args.exit_hold,
        launcher: args.launcher,
        record_sha256,
        key: secrets.attach_key.into_bytes(),
        own_uid: peer::own_uid(),
        invocation_id: std::env::var("INVOCATION_ID").unwrap_or_default(),
        child_pid: child.pid,
    };
    let shared = Arc::new(Shared {
        cfg,
        st: Mutex::new(State {
            buf: ReplayBuffer::new(args.replay_bytes),
            output_eof: false,
            exit_raw: None,
            exit: None,
            finalized_at: None,
            exit_acked: false,
            holder: 0,
            next_id: 1,
            conns: HashMap::new(),
            refusals: 0,
        }),
        cv: Condvar::new(),
        input: Mutex::new(tx),
        input_pending: Arc::clone(&input_pending),
        master: child.master.map(Mutex::new),
    });

    install_signal_handlers();
    spawn_reader(Arc::clone(&shared), child.reader);
    spawn_waiter(Arc::clone(&shared));
    spawn_input_writer(rx, child.writer, input_pending);

    // `host.pid`: the PID and its start time, so a launcher can tell this process from a reused PID.
    let pid = std::process::id();
    let pid_text = format!("{pid} {}\n", peer::start_time(pid).unwrap_or(0));
    let pid_tmp = args.dir.join("host.pid.tmp");
    std::fs::write(&pid_tmp, pid_text)
        .and_then(|_| std::fs::rename(&pid_tmp, args.dir.join("host.pid")))
        .map_err(|e| fail_and_end_child(format!("cannot write host.pid: {e}")))?;

    let socket = args.dir.join("host.sock");
    let _ = std::fs::remove_file(&socket);
    // SAFETY: `umask` only sets the process's file-creation mask; no threads create files yet.
    let old = unsafe { libc::umask(0o177) };
    let bound = UnixListener::bind(&socket);
    // SAFETY: as above, restoring the previous mask.
    unsafe {
        libc::umask(old);
    }
    let listener =
        bound.map_err(|e| fail_and_end_child(format!("cannot bind {}: {e}", socket.display())))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| fail_and_end_child(format!("cannot set the mode of the socket: {e}")))?;
    notify_ready(child.pid);

    let accepting = Arc::clone(&shared);
    std::thread::spawn(move || accept_loop(accepting, listener));

    let reason = wait_for_end(&shared);
    finish(&shared, reason, &socket, args);
    Ok(())
}

fn install_signal_handlers() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe. `sigaction` is
    // given a zeroed struct with the handler and an empty mask set.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_term as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        // A hang-up of the launcher's terminal must not end the host.
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
}

fn spawn_reader(shared: Arc<Shared>, mut reader: Box<dyn Read + Send>) {
    std::thread::spawn(move || {
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut st = shared.lock();
                    if st.exit.is_some() {
                        // Output after the exit was reported would break the cursor `exit.at`.
                        break;
                    }
                    st.buf.push(&chunk[..n]);
                    shared.wake_all(&st);
                    shared.cv.notify_all();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                // `EIO` is how a PTY master reports that the child side closed.
                Err(_) => break,
            }
        }
        let mut st = shared.lock();
        st.output_eof = true;
        shared.cv.notify_all();
    });
}

fn spawn_waiter(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let pid = shared.cfg.child_pid as libc::pid_t;
        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: `status` is valid for the call; `pid` is our own child, reaped only here.
            let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
            if rc == pid {
                break;
            }
            if rc < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
        let raw = if libc::WIFEXITED(status) {
            (Some(libc::WEXITSTATUS(status)), None)
        } else if libc::WIFSIGNALED(status) {
            (None, Some(libc::WTERMSIG(status)))
        } else {
            (None, None)
        };
        let mut st = shared.lock();
        st.exit_raw = Some(raw);
        shared.cv.notify_all();
        // The process ended; its last output may still be in the pipe. Wait for the end of the
        // stream, but not for ever: a process the child left behind may hold it open.
        let deadline = Instant::now() + DRAIN_GRACE;
        while !st.output_eof {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            st = shared
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let at = st.buf.end();
        st.exit = Some(ExitInfo {
            code: raw.0,
            signal: raw.1,
            at,
        });
        st.finalized_at = Some(Instant::now());
        shared.wake_all(&st);
        shared.cv.notify_all();
    });
}

fn spawn_input_writer(
    rx: Receiver<Vec<u8>>,
    mut writer: Box<dyn Write + Send>,
    pending: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || {
        for chunk in rx {
            // A failed write means the child no longer reads: that chunk is dropped, and so is the
            // next one, the same way.
            let _ = writer.write_all(&chunk).and_then(|_| writer.flush());
            pending.fetch_sub(chunk.len(), Ordering::SeqCst);
        }
    });
}

/// Block until the host should end, and say why.
fn wait_for_end(shared: &Arc<Shared>) -> EndReason {
    let mut st = shared.lock();
    loop {
        if TERM.load(Ordering::SeqCst) {
            return EndReason::Terminated;
        }
        if let (Some(_), Some(at)) = (&st.exit, st.finalized_at) {
            if st.exit_acked && st.buf.base() >= st.buf.end() {
                return EndReason::ExitAcked;
            }
            if at.elapsed() >= shared.cfg.exit_hold {
                return EndReason::HoldExpired;
            }
        }
        st = shared
            .cv
            .wait_timeout(st, Duration::from_millis(100))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// End the child if it still runs (a `SIGTERM` to the host), write the exit file, remove the
/// socket and, under the detached launcher, the host's directory.
fn finish(shared: &Arc<Shared>, reason: EndReason, socket: &PathBuf, args: &RunArgs) {
    let pgid = shared.cfg.child_pid;
    if reason == EndReason::Terminated {
        peer::kill_group(pgid, libc::SIGTERM);
        let mut st = shared.lock();
        let deadline = Instant::now() + TERM_GRACE;
        while st.exit_raw.is_none() && Instant::now() < deadline {
            st = shared
                .cv
                .wait_timeout(st, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if st.exit_raw.is_none() {
            drop(st);
            peer::kill_group(pgid, libc::SIGKILL);
            st = shared.lock();
            let deadline = Instant::now() + Duration::from_secs(3);
            while st.exit_raw.is_none() && Instant::now() < deadline {
                st = shared
                    .cv
                    .wait_timeout(st, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        // The leader is gone; end whatever it left in the group.
        peer::kill_group(pgid, libc::SIGKILL);
        let deadline = Instant::now() + DRAIN_GRACE * 3;
        while st.exit.is_none() && Instant::now() < deadline {
            st = shared
                .cv
                .wait_timeout(st, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    let body = shared.exit_file(reason.text(), None);
    let _ = exitfile::write(&shared.cfg.state_dir, &shared.cfg.unit, &body, false);
    let _ = std::fs::remove_file(socket);
    if args.launcher == LauncherKind::Detached {
        let _ = std::fs::remove_dir_all(&args.dir);
    }
}

fn accept_loop(shared: Arc<Shared>, listener: UnixListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Ok(waker) = Waker::new() else { continue };
        let waker = Arc::new(waker);
        let id = {
            let mut st = shared.lock();
            if st.conns.len() >= CONN_MAX {
                continue;
            }
            let id = st.next_id;
            st.next_id += 1;
            st.conns.insert(id, Arc::clone(&waker));
            id
        };
        let peer = peer::peer_cred(stream.as_raw_fd());
        let sh = Arc::clone(&shared);
        std::thread::spawn(move || {
            serve_conn(&sh, stream, peer.ok(), id, &waker);
            let mut st = sh.lock();
            st.conns.remove(&id);
            if st.holder == id {
                st.holder = 0;
            }
        });
    }
}

/// One connection's state.
struct Conn {
    id: u64,
    stream: UnixStream,
    rbuf: Vec<u8>,
    wbuf: Vec<u8>,
    wpos: usize,
    attached: bool,
    /// The cursor of the next byte to send; `None` until the client sends `read`.
    sent: Option<u64>,
    exit_sent: bool,
    /// Set when the connection must close once its queue is written.
    closing: bool,
    /// The client closed its side. The frames it sent before are still handled.
    eof: bool,
    peer_pid: i32,
}

impl Conn {
    fn queue(&mut self, msg: &HostMsg, data: &[u8]) {
        let bytes = encode_host(msg, data);
        self.wbuf.extend_from_slice(&bytes);
    }

    fn pending(&self) -> usize {
        self.wbuf.len() - self.wpos
    }

    fn error(&mut self, code: &str, text: &str) {
        self.queue(
            &HostMsg::Error {
                code: code.to_string(),
                text: text.to_string(),
            },
            &[],
        );
    }

    /// Write what the socket takes. False when the peer is gone.
    fn flush(&mut self) -> bool {
        while self.wpos < self.wbuf.len() {
            match self.stream.write(&self.wbuf[self.wpos..]) {
                Ok(0) => return false,
                Ok(n) => self.wpos += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        if self.wpos == self.wbuf.len() {
            self.wbuf.clear();
            self.wpos = 0;
        } else if self.wpos > (1 << 20) {
            self.wbuf.drain(..self.wpos);
            self.wpos = 0;
        }
        true
    }

    /// Refuse: count and log it, answer with an `error`, and close once it is sent.
    fn refuse(&mut self, shared: &Shared, check: &str, text: &str) {
        shared.refuse(self.peer_pid, check);
        self.error(check, text);
        self.closing = true;
    }
}

fn serve_conn(
    shared: &Arc<Shared>,
    stream: UnixStream,
    peer: Option<Peer>,
    id: u64,
    waker: &Arc<Waker>,
) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let mut c = Conn {
        id,
        stream,
        rbuf: Vec::new(),
        wbuf: Vec::new(),
        wpos: 0,
        attached: false,
        sent: None,
        exit_sent: false,
        closing: false,
        eof: false,
        peer_pid: peer.map_or(0, |p| p.pid),
    };
    // Before a valid attach the host says its protocol range and nothing else.
    c.queue(
        &HostMsg::Version {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
        },
        &[],
    );
    match peer {
        Some(p) if shared.check_peer(&p) => {}
        _ => c.refuse(shared, "peer", "the peer is not the controller"),
    }
    let attach_deadline = Instant::now() + shared.cfg.attach_wait;
    let mut closing_since: Option<Instant> = None;
    loop {
        if c.closing && closing_since.is_none() {
            closing_since = Some(Instant::now());
        }
        if !c.closing {
            process_frames(shared, &mut c);
        }
        if c.attached && !c.closing {
            check_superseded(shared, &mut c);
        }
        if c.attached && !c.closing {
            pump(shared, &mut c);
        }
        if !c.flush() {
            return;
        }
        if c.eof && !c.closing {
            return;
        }
        if c.closing
            && (c.pending() == 0
                || closing_since.is_some_and(|t| t.elapsed() > Duration::from_millis(500)))
        {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        // More to send and room for it: go round again without waiting.
        if c.attached && !c.closing && c.pending() < WBUF_HIGH {
            let more = {
                let st = shared.lock();
                c.sent.is_some_and(|s| s < st.buf.end())
            };
            if more {
                continue;
            }
        }
        let timeout = if !c.attached && !c.closing {
            let left = attach_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                c.refuse(shared, "attach_timeout", "no valid attach in time");
                continue;
            }
            left.as_millis().min(i32::MAX as u128) as i32 + 1
        } else if c.closing {
            50
        } else {
            -1
        };
        let want_read = !c.closing;
        match peer::poll2(
            c.stream.as_raw_fd(),
            c.pending() > 0,
            want_read,
            waker.fd(),
            timeout,
        ) {
            Ok((ready, woken)) => {
                if woken {
                    waker.drain();
                }
                if ready.readable || (ready.hangup && !c.closing) {
                    read_some(&mut c);
                }
            }
            Err(_) => return,
        }
    }
}

/// Read what the socket has into the frame buffer. Sets `eof` when the client closed its side.
fn read_some(c: &mut Conn) {
    let mut chunk = [0u8; 64 * 1024];
    let mut total = 0usize;
    loop {
        match c.stream.read(&mut chunk) {
            Ok(0) => {
                c.eof = true;
                return;
            }
            Ok(n) => {
                c.rbuf.extend_from_slice(&chunk[..n]);
                total += n;
                if total >= 1 << 20 {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                c.eof = true;
                return;
            }
        }
    }
}

/// A newer attach took this connection's place: tell it and close it. A superseded connection
/// changes nothing, whatever it sends.
fn check_superseded(shared: &Arc<Shared>, c: &mut Conn) -> bool {
    if shared.lock().holder == c.id {
        return false;
    }
    c.error("superseded", "another controller attached");
    c.closing = true;
    true
}

/// Handle every whole frame in the read buffer.
fn process_frames(shared: &Arc<Shared>, c: &mut Conn) {
    while !c.closing {
        let (msg, body) = match take_client(&mut c.rbuf) {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                if c.attached {
                    c.error(e.code, &e.text);
                    c.closing = true;
                } else {
                    c.refuse(shared, "attach", &e.text);
                }
                return;
            }
        };
        if !c.attached {
            handle_attach(shared, c, msg);
            continue;
        }
        if check_superseded(shared, c) {
            return;
        }
        handle_attached(shared, c, msg, body);
    }
}

fn handle_attach(shared: &Arc<Shared>, c: &mut Conn, msg: ClientMsg) {
    let ClientMsg::Attach { proto, key } = msg else {
        c.refuse(shared, "attach", "the first message must be attach");
        return;
    };
    if !(PROTO_MIN..=PROTO_MAX).contains(&proto) {
        c.refuse(
            shared,
            "version",
            &format!("the host speaks protocol {PROTO_MIN} to {PROTO_MAX}, not {proto}"),
        );
        return;
    }
    if !peer::constant_time_eq(key.as_bytes(), &shared.cfg.key) {
        c.refuse(shared, "key", "the attach key is wrong or missing");
        return;
    }
    let hello = {
        let mut st = shared.lock();
        st.holder = c.id;
        shared.wake_all(&st);
        Hello {
            proto,
            session: shared.cfg.session.clone(),
            incarnation: shared.cfg.incarnation,
            invocation_id: shared.cfg.invocation_id.clone(),
            child_pid: shared.cfg.child_pid,
            record_sha256: shared.cfg.record_sha256.clone(),
            exit: st.exit.clone(),
        }
    };
    c.queue(&HostMsg::Hello(hello), &[]);
    c.attached = true;
}

fn handle_attached(shared: &Arc<Shared>, c: &mut Conn, msg: ClientMsg, body: Vec<u8>) {
    match msg {
        ClientMsg::Attach { .. } => c.error("bad_message", "already attached"),
        ClientMsg::Read { from } => {
            let st = shared.lock();
            if from > st.buf.end() {
                let end = st.buf.end();
                drop(st);
                c.error("bad_cursor", &format!("the output ends at {end}"));
            } else {
                drop(st);
                c.sent = Some(from);
                c.exit_sent = false;
            }
        }
        ClientMsg::Ack { to } => {
            shared.lock().buf.ack(to);
            shared.cv.notify_all();
        }
        ClientMsg::Input { .. } => {
            if shared.lock().exit_raw.is_some() {
                c.error("exited", "the child ended");
            } else if shared.input_pending.load(Ordering::SeqCst) + body.len() > INPUT_QUEUE_MAX {
                c.error("input_full", "the child does not read its input");
            } else {
                shared.input_pending.fetch_add(body.len(), Ordering::SeqCst);
                let tx = shared.input.lock().unwrap_or_else(|e| e.into_inner());
                let _ = tx.send(body);
            }
        }
        ClientMsg::Resize { rows, cols } => match &shared.master {
            None => c.error("not_pty", "the child is on pipes"),
            Some(master) => {
                let size = PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                };
                let res = master
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .resize(size);
                if let Err(e) = res {
                    c.error("resize_failed", &e.to_string());
                }
            }
        },
        ClientMsg::Signal { sig } => {
            let sig: SignalKind = sig;
            if shared.lock().exit_raw.is_none() {
                peer::kill_group(shared.cfg.child_pid, sig.number());
            }
        }
        ClientMsg::Snapshot => c.error("unsupported", "version 1 has no screen snapshot"),
        ClientMsg::ExitAck => {
            let mut st = shared.lock();
            if st.exit.is_some() {
                st.exit_acked = true;
                shared.cv.notify_all();
            } else {
                drop(st);
                c.error("no_exit", "the child has not ended");
            }
        }
    }
}

/// Queue output for a connection that is streaming, up to the write high-water mark.
fn pump(shared: &Arc<Shared>, c: &mut Conn) {
    let Some(mut sent) = c.sent else { return };
    let st = shared.lock();
    if st.holder != c.id {
        return;
    }
    while c.pending() < WBUF_HIGH {
        match st.buf.read(sent, FRAME_MAX) {
            BufRead::Gap { from, to } => {
                c.queue(&HostMsg::Gap { from, to }, &[]);
                sent = to;
            }
            BufRead::Bytes(bytes) if bytes.is_empty() => break,
            BufRead::Bytes(bytes) => {
                c.queue(
                    &HostMsg::Out {
                        at: sent,
                        n: bytes.len(),
                    },
                    &bytes,
                );
                sent += bytes.len() as u64;
            }
        }
    }
    if sent == st.buf.end()
        && !c.exit_sent
        && let Some(exit) = &st.exit
    {
        c.queue(&HostMsg::Exit(exit.clone()), &[]);
        c.exit_sent = true;
    }
    c.sent = Some(sent);
}
