//! Who calls `settle`, and who calls the grant verbs from inside a chat's host unit (ADR-0005,
//! amended; `docs/contracts/requests.md`, "The caller check"; `session-trust`).
//!
//! `settle` is the one verb that acts for a chat, so the controller names the chat session that
//! calls from the caller's process, never from anything the caller says. The check reads the
//! caller's pidfd (`SO_PEERPIDFD`), walks its ancestry to a live chat process, finds that session's
//! live host row and, under the `systemd` launcher, compares the caller's cgroup with the host
//! unit's. At the end it checks that the pidfd still names the same process and that each pinned
//! process still has its start time, so a process that exited or a PID that was used again refuses.
//! It opens no connection to a host, writes no attach line and reads no claim token.
//!
//! Every read that fails refuses. These checks stop mistakes and stale callers, not a hostile
//! process of the same user (ADR-0005): under `detached` the ancestry is the whole check.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pf_engine::port::store::Store;
use pf_host::peer::{parse_cgroup, peer_pidfd, pidfd_pid, start_time};
use pf_ipc::{Peer, PeerSocket};
use std::os::fd::AsRawFd;

use super::Controller;
use super::requests::{
    CHAT_MARKER, MAX_ANCESTORS, environment_chat_marker, environment_has_seat_token,
    marker_session, parent_of,
};

/// What a test calls at a pause of the settle verb.
pub type Pause = Arc<dyn Fn() + Send + Sync>;

/// The faults and pauses a test injects into `settle` and its caller check. All are off unless a test
/// sets them, as [`super::hosts::HostFaults`] and [`super::proposals::StartFaults`] are. They are
/// product code, not `#[cfg(test)]`: an integration test cannot see a `#[cfg(test)]` item.
#[derive(Default)]
pub struct SettleFaults {
    /// Make the read of `SO_PEERPIDFD` fail, as a kernel without the option does.
    pub fail_pidfd: AtomicBool,
    /// Make the end check meet the top ancestor of the chain (the chat's process) with another start
    /// time than the walk pinned, as it does when that PID is used again by a new process. A test
    /// cannot make the kernel give a live process's PID to another process.
    pub reuse_top_ancestor_pid: AtomicBool,
    after_grant: Mutex<Option<Pause>>,
    before_end_check: Mutex<Option<Pause>>,
}

impl SettleFaults {
    /// A test point: call `pause` after `settle` has read the grant (step 6) and before it asks the
    /// engine. `None` clears it.
    pub fn set_after_grant(&self, pause: Option<Pause>) {
        *self
            .after_grant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pause;
    }

    /// A test point: call `pause` between the walk of the caller's ancestry and the end check of the
    /// caller check. `None` clears it.
    pub fn set_before_end_check(&self, pause: Option<Pause>) {
        *self
            .before_end_check
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pause;
    }

    pub(super) fn run_after_grant(&self) {
        let pause = self
            .after_grant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause();
        }
    }

    fn run_before_end_check(&self) {
        let pause = self
            .before_end_check
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause();
        }
    }
}

/// The refusal for a kernel that gives no pidfd, and for a fault a test injects in its place.
const NO_PIDFD: &str =
    "the kernel gives no pidfd of the caller (SO_PEERPIDFD), so settle cannot name it";

/// One process on the caller's chain, pinned by its PID and its start time.
struct Pinned {
    pid: u32,
    started: u64,
}

impl Controller {
    /// The faults and pauses of `settle`, for a test that flips one after the controller started.
    pub fn settle_faults(&self) -> &SettleFaults {
        &self.cfg.settle_faults
    }

    /// Name the chat session that calls `settle`, or refuse with the cause alone (the verb puts
    /// `settle refused:` before it). In this order, each read failing closed:
    /// 1. the PID, from the pidfd of the socket;
    /// 2. a seat (`PF_SEAT_TOKEN`, or a tracked seat on the chain) refuses;
    /// 3. the chain: each process from the caller up is pinned by PID and start time, and the walk
    ///    ends at the first live chat process, which names the session. A marker that names another
    ///    session refuses, and a marker never names a caller;
    /// 4. a live host row of kind `chat` for that session;
    /// 5. under `systemd`, the caller's cgroup equals the host unit's cgroup; under `detached` the
    ///    ancestry is the whole check; any other launcher refuses;
    /// 6. the end: the pidfd still names the PID, and each pinned process still has its start time.
    pub(super) fn name_settle_caller(
        &self,
        peer: Peer,
        socket: Option<&PeerSocket>,
    ) -> Result<String, String> {
        let socket = socket.ok_or_else(|| {
            "the controller holds no socket descriptor of the caller, so settle cannot name it"
                .to_string()
        })?;
        // 1. the pidfd and its PID.
        if self.cfg.settle_faults.fail_pidfd.load(Ordering::SeqCst) {
            return Err(NO_PIDFD.to_string());
        }
        let pidfd = peer_pidfd(socket.as_raw_fd()).map_err(|_| NO_PIDFD.to_string())?;
        let pid = pidfd_pid(&pidfd)
            .map_err(|e| format!("the pidfd of the caller names no process ({e})"))?;
        let pid = u32::try_from(pid).map_err(|_| "the pidfd of the caller names no process")?;
        if let Some(cred) = peer.pid
            && cred != pid
        {
            return Err(format!(
                "the pidfd of the caller names process {pid}, and SO_PEERCRED names process {cred}"
            ));
        }

        // 2. a seat.
        let proc_root = &self.cfg.proc_root;
        let environ_path = proc_root.join(pid.to_string()).join("environ");
        let environ = std::fs::read(&environ_path).map_err(|e| {
            format!(
                "cannot read {} ({e}), so the controller cannot tell the caller is not a seat",
                environ_path.display()
            )
        })?;
        if environment_has_seat_token(&environ) {
            return Err(
                "PF_SEAT_TOKEN is in the caller's environment, so it is a seat".to_string(),
            );
        }
        let marker = environment_chat_marker(&environ);

        // 3. the chain.
        let seats = self.tracked_seat_pids();
        let chats: Vec<(u32, String)> = self
            .live_chats()
            .iter()
            .filter_map(|l| l.port.process_id(&l.chat).map(|p| (p, l.session.clone())))
            .collect();
        let mut pinned: Vec<Pinned> = Vec::new();
        let mut found: Option<(u32, String)> = None;
        let mut current = pid;
        let mut reached_top = false;
        for _ in 0..MAX_ANCESTORS {
            let started = start_time(current).ok_or_else(|| {
                format!(
                    "process {current} on the caller's chain has exited, so the caller is refused"
                )
            })?;
            pinned.push(Pinned {
                pid: current,
                started,
            });
            if seats.contains(&current) {
                return Err(format!(
                    "the caller is, or descends from, seat process {current}, so it is a seat"
                ));
            }
            if let Some((p, session)) = chats.iter().find(|(p, _)| *p == current) {
                found = Some((*p, session.clone()));
                break;
            }
            let parent = parent_of(proc_root, current)?;
            if parent <= 1 {
                reached_top = true;
                break;
            }
            current = parent;
        }
        let (chat_pid, session) = match (found, marker) {
            (Some((chat_pid, session)), Some(value)) => {
                if marker_session(&value) != Some(session.as_str()) {
                    return Err(format!(
                        "the chat marker names {}, but the caller runs under chat process {chat_pid} of {session}",
                        marker_session(&value).unwrap_or("chat:invalid")
                    ));
                }
                (chat_pid, session)
            }
            (Some(found), None) => found,
            (None, _) if !reached_top => {
                return Err(format!(
                    "the caller's parent chain is longer than {MAX_ANCESTORS} processes, so the controller cannot clear it"
                ));
            }
            (None, Some(_)) => {
                return Err(format!(
                    "the caller holds {CHAT_MARKER} but runs under no live chat process; the marker never names the caller"
                ));
            }
            (None, None) => {
                return Err(
                    "the caller runs under no chat session, and only a session of the project's Orchestrator settles"
                        .to_string(),
                );
            }
        };
        let _ = chat_pid;

        // 4. the session's live host.
        let hosts = self.store.live_hosts().map_err(|e| e.to_string())?;
        let host = hosts
            .iter()
            .find(|h| h.session_kind == "chat" && h.session_id == session && h.ended_at.is_none())
            .ok_or_else(|| format!("chat session {session} has no live host"))?;

        // 5. the launcher.
        match self.hosts.launcher_name() {
            "systemd" => {
                let own = parse_cgroup(
                    &std::fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup"))
                        .map_err(|e| format!("cannot read the caller's cgroup: {e}"))?,
                )
                .ok_or("the caller's cgroup file holds no cgroup v2 line")?;
                let unit = self.hosts.unit_cgroup(&host.unit)?;
                if own != unit {
                    return Err(format!(
                        "the caller's cgroup {own} is not the cgroup {unit} of the host unit {} of chat session {session}: the ancestry and the host's check must both name the same chat session",
                        host.unit
                    ));
                }
            }
            "detached" => {}
            other => {
                return Err(format!(
                    "the host launcher {other:?} is not one that settle can check"
                ));
            }
        }

        // 6. the end.
        self.cfg.settle_faults.run_before_end_check();
        if self
            .cfg
            .settle_faults
            .reuse_top_ancestor_pid
            .load(Ordering::SeqCst)
            && let Some(top) = pinned.last_mut()
        {
            top.started = top.started.wrapping_add(1);
        }
        let again = pidfd_pid(&pidfd)
            .map_err(|e| format!("the caller exited before the check ended ({e})"))?;
        if u32::try_from(again).ok() != Some(pid) {
            return Err("the caller's pidfd names another process than it did".to_string());
        }
        for p in &pinned {
            if start_time(p.pid) != Some(p.started) {
                return Err(format!(
                    "process {} on the caller's chain exited or was replaced during the check",
                    p.pid
                ));
            }
        }
        Ok(session)
    }

    /// The grant verbs under `systemd`: refuse a caller that runs in the cgroup of a live chat host
    /// unit, whatever its ancestry and its environment say (audit C13). The operator's CLI runs in
    /// no chat host unit. Under `detached` the caller is admitted: the cgroup is shared there, and
    /// the refusal does not exist (a stated residue). Every read that fails refuses: the caller's
    /// cgroup, the live hosts and each host unit's cgroup. `unit_cgroup` gives one error for a unit
    /// with no cgroup and for a user manager that cannot answer, so the check cannot tell the two
    /// apart, and it never lets either pass.
    pub(super) fn check_grant_caller_cgroup(&self, verb: &str, peer: Peer) -> Result<(), String> {
        if self.hosts.launcher_name() != "systemd" {
            return Ok(());
        }
        let refuse = |why: String| format!("{verb} refused: {why}");
        let pid = peer.pid.ok_or_else(|| {
            refuse("the controller cannot tell which process called, so it refuses".to_string())
        })?;
        let own = self
            .caller_cgroup(pid)
            .ok_or_else(|| refuse("cannot read the caller's cgroup, so it refuses".to_string()))?;
        let hosts = self.store.live_hosts().map_err(|e| refuse(e.to_string()))?;
        for host in hosts
            .iter()
            .filter(|h| h.session_kind == "chat" && h.ended_at.is_none())
        {
            match self.hosts.unit_cgroup(&host.unit) {
                Ok(unit) if unit == own => {
                    return Err(refuse(format!(
                        "the caller runs in the cgroup of chat host unit {}, so it runs under a chat",
                        host.unit
                    )));
                }
                Ok(_) => {}
                Err(why) => return Err(refuse(why)),
            }
        }
        Ok(())
    }

    /// The cgroup of a process, from the controller's proc root.
    fn caller_cgroup(&self, pid: u32) -> Option<String> {
        let text = std::fs::read_to_string(self.cfg.proc_root.join(pid.to_string()).join("cgroup"))
            .ok()?;
        parse_cgroup(&text)
    }
}
