//! Confirm codes (`r1.s4.w1`; ADR-0005, ADR-0009, ADR-0012; `docs/contracts/requests.md`, "Confirm
//! codes").
//!
//! A paired device asks for a code for one subject, then spends it once on the verb that acts on that
//! subject. The code proves the device, the freshness and the subject it was issued for. It does not
//! prove that a person pressed a button (Q2, a known limit).
//!
//! The table is in memory only: a restart voids every code (ADR-0007). It keeps the SHA-256 of each
//! code and never the code text. Every issued hash stays in the table as a tombstone after its use,
//! its void or its expiry, so a code that was cut short is never mistaken for a guess: only a hash the
//! table never held, or one it already dropped, is `code_unknown`, and only that voids the calling
//! device's other live codes.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use pf_api::types::ConfirmIssued;
use pf_engine::port::store::{RequestKind, RequestStatus, Store};
use serde_json::Value;

use super::{Controller, lock};

/// The most live codes one device holds. A fifth issue voids the device's oldest.
pub const MAX_LIVE_PER_DEVICE: usize = 4;

/// The most tombstones one device keeps. The oldest go first.
pub const MAX_TOMBSTONES_PER_DEVICE: usize = 64;

/// The text every code starts with.
pub const CODE_PREFIX: &str = "pfc1-";

/// The random bytes of a code: 128 bits.
pub const CODE_BYTES: usize = 16;

/// The verb a request code is for, with the parameters it binds (R12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestVerb {
    /// `text_sha256` is the SHA-256 hex of the answer text's UTF-8 bytes. The text never enters the table.
    Answer { text_sha256: String },
    /// The target stage, or `None` for the stage the unit parked in.
    Clear { to: Option<String> },
    /// The head SHA of the subject binds the merge.
    Ack,
}

impl RequestVerb {
    pub fn kind(&self) -> &'static str {
        match self {
            RequestVerb::Answer { .. } => "answer",
            RequestVerb::Clear { .. } => "clear",
            RequestVerb::Ack => "ack",
        }
    }
}

/// What a code is for. A later subject is a new variant, and nothing else changes shape: w2's start
/// card adds `Arm { proposal, plan_sha256 }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A request and the verb that will act on it. `head_sha` is the request body's `head_sha` at
    /// issue, for a `merge_ack` only.
    Request {
        id: String,
        revision: u32,
        verb: RequestVerb,
        head_sha: Option<String>,
    },
}

impl Subject {
    /// Read the store again: is the subject still what the code was issued for? `Err` names what moved.
    pub(super) fn fresh(&self, controller: &Controller) -> Result<(), String> {
        match self {
            Subject::Request {
                id,
                revision,
                verb,
                head_sha,
            } => {
                let row = controller
                    .store
                    .request(id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("request {id} is unknown"))?;
                if row.status != RequestStatus::Open {
                    return Err(format!("request {id} is not open any more"));
                }
                if row.revision != *revision {
                    return Err(format!(
                        "request {id} is at revision {}, and the code was issued for revision {revision}",
                        row.revision
                    ));
                }
                if matches!(verb, RequestVerb::Ack) {
                    let body: Value = serde_json::from_str(&row.body).unwrap_or(Value::Null);
                    let now = body.get("head_sha").and_then(Value::as_str);
                    if row.kind != RequestKind::MergeAck || now != head_sha.as_deref() {
                        return Err(format!(
                            "the head of the PR of request {id} moved since the code was issued"
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

/// Why a code was refused: the closed list of `pf_api::host::REFUSALS`, the codes' half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeRefusal {
    Unknown,
    Foreign,
    Used,
    Voided,
    Expired,
    Subject,
}

impl CodeRefusal {
    pub fn word(self) -> &'static str {
        match self {
            CodeRefusal::Unknown => "code_unknown",
            CodeRefusal::Foreign => "code_foreign",
            CodeRefusal::Used => "code_used",
            CodeRefusal::Voided => "code_voided",
            CodeRefusal::Expired => "code_expired",
            CodeRefusal::Subject => "code_subject",
        }
    }

    /// The cause in words. It never repeats the code.
    pub fn text(self) -> &'static str {
        match self {
            CodeRefusal::Unknown => {
                "the confirm code is unknown; the device's other open codes are voided, so ask for a new one"
            }
            CodeRefusal::Foreign => {
                "the confirm code was issued to another device; it is spent, so ask for a new one"
            }
            CodeRefusal::Used => "the confirm code was used before; ask for a new one",
            CodeRefusal::Voided => {
                "the confirm code was voided (the device asked for more than 4 at once, or was revoked); ask for a new one"
            }
            CodeRefusal::Expired => "the confirm code expired; ask for a new one",
            CodeRefusal::Subject => {
                "the confirm code was issued for another request, verb or parameters; it is spent, so ask for a new one"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Live,
    Spent,
    Voided,
}

struct Entry {
    /// SHA-256 hex of the code text.
    hash: String,
    device: String,
    subject: Subject,
    /// The claim generation the code binds: `None` when no claim existed at issue.
    generation: Option<u64>,
    expires: Instant,
    state: State,
    /// The command ID that issued the code and the answer it got, less the code: a replay of that command
    /// answers this record, whatever moved since. It leaves the table with the entry.
    first: (String, ConfirmIssued),
}

impl Entry {
    fn live(&self, now: Instant) -> bool {
        self.state == State::Live && now < self.expires
    }
}

/// What a spent code gave back.
#[derive(Clone, Debug)]
pub struct Issued {
    pub subject: Subject,
    pub generation: Option<u64>,
}

#[derive(Default)]
struct Table {
    /// In the order the codes were issued.
    entries: Vec<Entry>,
    /// Added to the clock: the seam a test moves time with.
    skew: Duration,
}

/// The table of issued codes.
#[derive(Default)]
pub struct ConfirmCodes {
    table: Mutex<Table>,
}

impl ConfirmCodes {
    fn now(table: &Table) -> Instant {
        Instant::now() + table.skew
    }

    /// Drop each tombstone that is `tombstone` past its expiry, and the oldest tombstones of a device
    /// that holds more than [`MAX_TOMBSTONES_PER_DEVICE`].
    fn sweep(table: &mut Table, tombstone: Duration) {
        let now = Self::now(table);
        table
            .entries
            .retain(|e| e.live(now) || now < e.expires + tombstone);
        let mut devices: Vec<String> = table.entries.iter().map(|e| e.device.clone()).collect();
        devices.sort();
        devices.dedup();
        for device in devices {
            let dead = table
                .entries
                .iter()
                .filter(|e| e.device == device && !e.live(now))
                .count();
            let mut drop_n = dead.saturating_sub(MAX_TOMBSTONES_PER_DEVICE);
            table.entries.retain(|e| {
                if drop_n > 0 && e.device == device && !e.live(now) {
                    drop_n -= 1;
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Record a new code. A fifth live code of the device voids its oldest. `first` is the command ID and
    /// the answer the device got, with its code and `replayed` cleared, for a replay to give back.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &self,
        device: &str,
        hash: String,
        subject: Subject,
        generation: Option<u64>,
        ttl: Duration,
        tombstone: Duration,
        first: (String, ConfirmIssued),
    ) {
        let mut table = lock(&self.table);
        Self::sweep(&mut table, tombstone);
        let now = Self::now(&table);
        let live: Vec<usize> = table
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.device == device && e.live(now))
            .map(|(i, _)| i)
            .collect();
        if live.len() >= MAX_LIVE_PER_DEVICE {
            for &i in &live[..=live.len() - MAX_LIVE_PER_DEVICE] {
                table.entries[i].state = State::Voided;
            }
        }
        let expires = now + ttl;
        table.entries.push(Entry {
            hash,
            device: device.to_string(),
            subject,
            generation,
            expires,
            state: State::Live,
            first,
        });
    }

    /// The first answer of a command ID the device issued a code with, as it was given. `None` once the
    /// entry left the table (a restart, or 10 minutes past the code's expiry).
    pub fn first_answer(&self, device: &str, command_id: &str) -> Option<ConfirmIssued> {
        lock(&self.table)
            .entries
            .iter()
            .find(|e| e.device == device && e.first.0 == command_id)
            .map(|e| e.first.1.clone())
    }

    /// Spend a code for `device` on a call that acts on `call` (a request subject with the call's own
    /// verb and parameters). Any presentation that matches a code spends it, whatever follows. An unknown
    /// code voids every live code of the calling device.
    pub fn take(
        &self,
        device: &str,
        hash: &str,
        call: &Subject,
        tombstone: Duration,
    ) -> Result<Issued, CodeRefusal> {
        let mut table = lock(&self.table);
        Self::sweep(&mut table, tombstone);
        let now = Self::now(&table);
        let Some(at) = table.entries.iter().position(|e| e.hash == hash) else {
            for e in table
                .entries
                .iter_mut()
                .filter(|e| e.device == device && e.live(now))
            {
                e.state = State::Voided;
            }
            return Err(CodeRefusal::Unknown);
        };
        let entry = &mut table.entries[at];
        if entry.device != device {
            entry.state = State::Spent;
            return Err(CodeRefusal::Foreign);
        }
        match entry.state {
            State::Spent => return Err(CodeRefusal::Used),
            State::Voided => return Err(CodeRefusal::Voided),
            State::Live => {}
        }
        if now >= entry.expires {
            return Err(CodeRefusal::Expired);
        }
        // Passed: spent at once, whatever the verb finds next.
        entry.state = State::Spent;
        if !same_target(&entry.subject, call) {
            return Err(CodeRefusal::Subject);
        }
        Ok(Issued {
            subject: entry.subject.clone(),
            generation: entry.generation,
        })
    }

    /// Void every live code of a device (a revoke).
    pub fn void_device(&self, device: &str) {
        let mut table = lock(&self.table);
        let now = Self::now(&table);
        for e in table
            .entries
            .iter_mut()
            .filter(|e| e.device == device && e.live(now))
        {
            e.state = State::Voided;
        }
    }

    /// Move the table's clock forward. A test seam: it stands in for waiting.
    pub fn advance(&self, by: Duration) {
        lock(&self.table).skew += by;
    }

    /// How many entries the table holds, live or not. For tests of the sweep.
    pub fn len(&self) -> usize {
        lock(&self.table).entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Is the call the act the code binds? The request, its revision, the verb and the verb's parameters
/// must all be equal. The head SHA is not in the call: `Subject::fresh` checks it against the store.
fn same_target(code: &Subject, call: &Subject) -> bool {
    match (code, call) {
        (
            Subject::Request {
                id: a,
                revision: ra,
                verb: va,
                ..
            },
            Subject::Request {
                id: b,
                revision: rb,
                verb: vb,
                ..
            },
        ) => a == b && ra == rb && va == vb,
    }
}

impl Controller {
    /// Move the clock of the confirm codes forward. A test seam, as `Config.confirm_ttl` is.
    pub fn advance_confirm_clock(&self, by: Duration) {
        self.confirm.advance(by);
    }

    /// The table of confirm codes, for tests.
    pub fn confirm_codes(&self) -> &ConfirmCodes {
        &self.confirm
    }
}
