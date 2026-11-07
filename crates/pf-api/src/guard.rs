//! The guards of the API (ADR-0012, `docs/contracts/api.md` "Guards"): the peer guard, which blocks a
//! peer address after repeated failures, and the range check of the listen address.
//!
//! The peer guard keeps two counters per peer address, each over a sliding window: failed pairing
//! codes and failed token calls. The failure that fills either counter blocks the peer on every
//! route for a fixed time. Each peer is counted alone. The state lives in memory, so a restart
//! clears every count and every block.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The time source of the guard. A test moves it by hand.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// The system clock.
pub fn system_clock() -> Clock {
    Arc::new(SystemTime::now)
}

/// The numbers of the peer guard.
#[derive(Clone, Debug)]
pub struct GuardConfig {
    /// The failure that makes this count, of one kind, inside the window blocks the peer: 5.
    pub max_failures: usize,
    /// How far back a failure counts: 10 minutes.
    pub window: Duration,
    /// How long a block lasts: 10 minutes.
    pub block: Duration,
    /// The most peers the guard keeps. The oldest entry goes when it is full: 4096.
    pub max_peers: usize,
    /// The least time between two `blocked` lines of one peer: 60 seconds.
    pub coalesce: Duration,
}

impl Default for GuardConfig {
    fn default() -> GuardConfig {
        GuardConfig {
            max_failures: 5,
            window: Duration::from_secs(600),
            block: Duration::from_secs(600),
            max_peers: 4096,
            coalesce: Duration::from_secs(60),
        }
    }
}

/// The two kinds of failure the guard counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A `/v1/pair` answer of 400 or 401.
    Code,
    /// A 401 on `/v1/status` or `/v1/verbs/<verb>`.
    Token,
}

/// A peer that is blocked now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    /// When the block ends, as UTC text.
    pub until: String,
    /// The whole seconds to wait, at least 1.
    pub retry_after: u64,
}

#[derive(Default)]
struct Peer {
    /// The order the entry was made in. The oldest entry goes first when the map is full.
    seq: u64,
    codes: VecDeque<Duration>,
    tokens: VecDeque<Duration>,
    blocked_until: Option<Duration>,
    /// When this peer's last `blocked` line was written, in this block.
    last_line: Option<Duration>,
    /// The 429s answered since that line.
    since_line: u64,
}

impl Peer {
    fn forget_old(&mut self, now: Duration, window: Duration) {
        for queue in [&mut self.codes, &mut self.tokens] {
            while queue
                .front()
                .is_some_and(|t| now.saturating_sub(*t) >= window)
            {
                queue.pop_front();
            }
        }
        if self.blocked_until.is_some_and(|until| now >= until) {
            self.blocked_until = None;
            self.last_line = None;
            self.since_line = 0;
        }
    }

    fn idle(&self) -> bool {
        self.codes.is_empty() && self.tokens.is_empty() && self.blocked_until.is_none()
    }
}

#[derive(Default)]
struct State {
    peers: HashMap<IpAddr, Peer>,
    /// `seq` to peer, oldest first.
    order: BTreeMap<u64, IpAddr>,
    next_seq: u64,
    last_sweep: Duration,
}

impl State {
    fn remove(&mut self, ip: &IpAddr) {
        if let Some(peer) = self.peers.remove(ip) {
            self.order.remove(&peer.seq);
        }
    }
}

/// The peer guard. One guard serves every connection of a router.
pub struct Guard {
    config: GuardConfig,
    clock: Clock,
    state: Mutex<State>,
}

impl Guard {
    pub fn new(config: GuardConfig, clock: Clock) -> Guard {
        Guard {
            config,
            clock,
            state: Mutex::new(State::default()),
        }
    }

    fn now(&self) -> Duration {
        (self.clock)()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop every entry whose window and block have passed. It runs at most once a minute, so a
    /// flood of requests does not make each one walk the map.
    fn sweep(&self, state: &mut State, now: Duration) {
        if now.saturating_sub(state.last_sweep) < Duration::from_secs(60) {
            return;
        }
        state.last_sweep = now;
        let window = self.config.window;
        let gone: Vec<IpAddr> = state
            .peers
            .iter_mut()
            .filter_map(|(ip, peer)| {
                peer.forget_old(now, window);
                peer.idle().then_some(*ip)
            })
            .collect();
        for ip in gone {
            state.remove(&ip);
        }
    }

    /// The block this peer is under now, if any. The router calls this first on every request.
    pub fn blocked(&self, peer: IpAddr) -> Option<Blocked> {
        let now = self.now();
        let mut state = self.lock();
        self.sweep(&mut state, now);
        let entry = state.peers.get_mut(&peer)?;
        entry.forget_old(now, self.config.window);
        let until = entry.blocked_until?;
        let wait = until.saturating_sub(now);
        Some(Blocked {
            until: utc(until),
            retry_after: wait.as_secs() + u64::from(wait.subsec_nanos() > 0),
        })
    }

    /// Count one failure of this peer. It returns the end of the block, as UTC text, when this
    /// failure starts one.
    pub fn fail(&self, peer: IpAddr, kind: Failure) -> Option<String> {
        let now = self.now();
        let mut state = self.lock();
        self.sweep(&mut state, now);
        if !state.peers.contains_key(&peer) {
            // The map is full: the oldest entry goes.
            while state.peers.len() >= self.config.max_peers.max(1) {
                let Some((_, oldest)) = state.order.pop_first() else {
                    break;
                };
                state.peers.remove(&oldest);
            }
            let seq = state.next_seq;
            state.next_seq += 1;
            state.order.insert(seq, peer);
            state.peers.insert(
                peer,
                Peer {
                    seq,
                    ..Peer::default()
                },
            );
        }
        let entry = state.peers.get_mut(&peer)?;
        entry.forget_old(now, self.config.window);
        if entry.blocked_until.is_some() {
            // A blocked peer's requests are refused before they can fail.
            return None;
        }
        let queue = match kind {
            Failure::Code => &mut entry.codes,
            Failure::Token => &mut entry.tokens,
        };
        queue.push_back(now);
        if queue.len() < self.config.max_failures {
            return None;
        }
        let until = now + self.config.block;
        entry.blocked_until = Some(until);
        // The block starts with clean counters and a clean line count.
        entry.codes.clear();
        entry.tokens.clear();
        entry.last_line = None;
        entry.since_line = 0;
        Some(utc(until))
    }

    /// Count one 429 of a blocked peer. It returns `Some(n)` when a `blocked` line is due, and `n` is
    /// the number of 429s the line counts, itself included. The first 429 of a block is due at
    /// once. A later one is due when the coalescing time has passed since the last line.
    pub fn count_blocked(&self, peer: IpAddr) -> Option<u64> {
        let now = self.now();
        let mut state = self.lock();
        let entry = state.peers.get_mut(&peer)?;
        entry.since_line += 1;
        let due = entry
            .last_line
            .is_none_or(|at| now.saturating_sub(at) >= self.config.coalesce);
        if !due {
            return None;
        }
        entry.last_line = Some(now);
        Some(std::mem::take(&mut entry.since_line))
    }

    /// The number of peers the guard keeps.
    pub fn len(&self) -> usize {
        self.lock().peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn utc(at: Duration) -> String {
    pf_engine::engine::format_utc(at.as_secs(), at.subsec_millis())
}

/// Whether the controller may listen on this address: loopback (`127.0.0.0/8`, `::1`) or the
/// tailnet (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`). An IPv4-mapped IPv6 address counts as its
/// IPv4 form. The port is not part of the check.
pub fn listen_allowed(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 127 || (o[0] == 100 && o[1] & 0xc0 == 0x40)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback() || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
        }
    }
}
