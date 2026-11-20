//! What the kernel says about the process on the other end of a Unix socket, and the small
//! `libc` calls the host and the client share (ADR-0008: the crate's one direct `libc` use).
//!
//! These checks stop mistakes and stale connections, not a hostile process of the same user: any
//! process of the user can move into another cgroup (Spike A, F8; ADR-0005). Under the detached
//! launcher the cgroup check is vacuous, because the host, the controller and the seats share one
//! cgroup, so the attach key is the only peer check there.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// The credentials of a peer (`SO_PEERCRED`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// The effective user of this process.
pub fn own_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// `SO_PEERCRED` of a connected Unix socket.
pub fn peer_cred(fd: RawFd) -> io::Result<Peer> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for the call and `len` is the size of `cred`.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Peer {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// The PID of the peer through `SO_PEERPIDFD`: the kernel hands over a pidfd of the process that
/// made the connection, and `/proc/self/fdinfo` names its PID. It fails on a kernel without the
/// option (before Linux 6.5), and the caller then falls back to [`peer_cred`].
pub fn peer_pid_by_pidfd(fd: RawFd) -> io::Result<i32> {
    let mut pidfd: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `pidfd` and `len` are valid for the call and `len` is the size of `pidfd`.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&mut pidfd as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the kernel gave us a new descriptor that nothing else owns.
    let owned = unsafe { OwnedFd::from_raw_fd(pidfd) };
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", owned.as_raw_fd()))?;
    info.lines()
        .find_map(|l| l.strip_prefix("Pid:"))
        .and_then(|p| p.trim().parse::<i32>().ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| io::Error::other("the pidfd names no process in this PID namespace"))
}

/// The cgroup path in the text of `/proc/<pid>/cgroup` (the `0::` line of cgroup v2).
pub fn parse_cgroup(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

/// The cgroup of a process, or `None` when it cannot be read.
pub fn cgroup_of(pid: i32) -> Option<String> {
    parse_cgroup(&std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
}

/// Compare two byte strings in time that depends on their length only.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The start time of a process, in clock ticks since boot (field 22 of `/proc/<pid>/stat`), or
/// `None` when the process is gone or a zombie. A PID and its start time name one process.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may hold spaces: split after the last `)`.
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    if state == "Z" || state == "X" {
        return None;
    }
    fields.nth(18)?.parse().ok()
}

/// The direct children of a process, from `/proc/<pid>/stat` of every process. A host has one: the
/// session's child.
pub fn children_of(pid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(child) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{child}/stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(')').map(|p| p.1) else {
            continue;
        };
        // After the command name: the state, then the parent's PID.
        if rest
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse::<u32>().ok())
            == Some(pid)
        {
            out.push(child);
        }
    }
    out
}

/// Send `sig` to a whole process group.
pub fn kill_group(pgid: u32, sig: i32) {
    // SAFETY: `kill` with a negative PID signals a process group; a stale group gives ESRCH.
    unsafe {
        libc::kill(-(pgid as i32), sig);
    }
}

/// Send `sig` to one process.
pub fn kill_pid(pid: u32, sig: i32) {
    // SAFETY: `kill` of one PID; a stale PID gives ESRCH, which the caller does not need.
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

/// A pipe a thread writes to wake another thread that waits in `poll`.
pub struct Waker {
    read: OwnedFd,
    write: OwnedFd,
}

impl Waker {
    pub fn new() -> io::Result<Waker> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors `pipe2` writes.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `pipe2` gave us two new descriptors that nothing else owns.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        Ok(Waker { read, write })
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Make the pipe readable. A full pipe already holds a wake-up, so a failure is fine.
    pub fn wake(&self) {
        // SAFETY: a one-byte write from a valid buffer to a descriptor we own.
        unsafe {
            libc::write(self.write.as_raw_fd(), [1u8].as_ptr().cast(), 1);
        }
    }

    /// Empty the pipe.
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: a read into a valid buffer from a descriptor we own.
            let n =
                unsafe { libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
    }
}

/// What `poll` reports for one descriptor.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ready {
    pub readable: bool,
    pub writable: bool,
    pub hangup: bool,
}

/// Wait for `fd` and `wake` up to `timeout_ms` (-1 for no limit). Returns the state of each.
pub fn poll2(
    fd: RawFd,
    want_write: bool,
    want_read: bool,
    wake: RawFd,
    timeout_ms: i32,
) -> io::Result<(Ready, bool)> {
    let mut fds = [
        libc::pollfd {
            fd,
            events: (if want_read { libc::POLLIN } else { 0 })
                | (if want_write { libc::POLLOUT } else { 0 }),
            revents: 0,
        },
        libc::pollfd {
            fd: wake,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a valid array of two `pollfd` for the whole call.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if rc >= 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    let r = fds[0].revents;
    Ok((
        Ready {
            readable: r & libc::POLLIN != 0,
            writable: r & libc::POLLOUT != 0,
            hangup: r & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0,
        },
        fds[1].revents & libc::POLLIN != 0,
    ))
}
