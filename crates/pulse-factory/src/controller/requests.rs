//! The verbs of requests and the claim: `requests`, `show`, `claim`, `answer`, `clear` and `ack`
//! (`docs/contracts/cli.md`, `docs/contracts/requests.md`; ADR-0005, ADR-0006, ADR-0009).
//!
//! `requests` and `show` read the store and need no claim. `claim`, `answer`, `clear` and `ack`
//! change who may act and what runs, so each one first checks who calls: a process that holds
//! `PF_SEAT_TOKEN`, or descends from a seat the controller launched and still tracks, is refused.
//! A `/proc` read that fails refuses too. The claim token reaches no log, ledger line, status,
//! request body or error text: the controller hands it out once, and keeps only its hash.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use pf_api::types::{ClaimRecord, RequestDetail, RequestList, RequestSummary};
use pf_engine::claim::{check_claim_in, claim_of_hash, holder_device, sha256_hex};
use pf_engine::merge::blocked_by;
use pf_engine::port::agent::SessionRef;
use pf_engine::port::store::{ClaimRow, DeviceRow, RequestKind, RequestRow, RequestStatus, Store};
use pf_ipc::{Peer, Response};
use serde_json::{Value, json};

use super::seat::{hex, random_bytes};
use super::{Controller, RunHandle, lock};

pub const SCHEMA_VERSION: u32 = 1;

/// The most parents the caller check follows. A chain that is longer, or that does not reach the
/// first process, refuses.
const MAX_ANCESTORS: usize = 64;

/// The number of random bytes in a claim token.
const TOKEN_BYTES: usize = 32;

/// The SHA-256 of a claim token as a client presents it: 64 hex digits that stand for
/// [`TOKEN_BYTES`] bytes. The hash is over those bytes.
pub fn token_hash(token: &str) -> Option<String> {
    let token = token.trim();
    if token.len() != TOKEN_BYTES * 2 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..token.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&token[i..i + 2], 16).ok())
        .collect();
    Some(sha256_hex(&bytes?))
}

fn err(why: impl Into<String>) -> Response {
    Response::err(why)
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// The parent of a process, from `<proc>/<pid>/stat`. The name field can hold spaces and
/// parentheses, so the line is cut after its last `)`.
fn parent_of(proc_root: &Path, pid: u32) -> Result<u32, String> {
    let path = proc_root.join(pid.to_string()).join("stat");
    let line = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let after = line
        .rfind(')')
        .map(|i| &line[i + 1..])
        .ok_or_else(|| format!("{} is not a process stat line", path.display()))?;
    after
        .split_whitespace()
        .nth(1)
        .and_then(|ppid| ppid.parse().ok())
        .ok_or_else(|| format!("{} holds no parent process", path.display()))
}

/// Does the environment text of a process (`NAME=value` entries split by NUL) hold the seat token?
fn environment_has_seat_token(environ: &[u8]) -> bool {
    environ
        .split(|b| *b == 0)
        .any(|entry| entry.starts_with(b"PF_SEAT_TOKEN="))
}

/// The name of the variable the controller puts in the environment of every chat it opens. Its value
/// is the session ID. A child that a chat detaches from its process tree keeps the chat's
/// environment, so the variable marks it where the ancestry walk cannot. It is a speed bump against
/// a cooperating or accidental chat, not a boundary (`docs/contracts/requests.md`, "The caller
/// check").
pub const CHAT_MARKER: &str = "PULSE_FACTORY_CHAT";

/// The session a process runs under, when its environment text holds the chat marker.
fn environment_chat_marker(environ: &[u8]) -> Option<String> {
    let prefix = format!("{CHAT_MARKER}=");
    environ.split(|b| *b == 0).find_map(|entry| {
        entry
            .strip_prefix(prefix.as_bytes())
            .map(|v| String::from_utf8_lossy(v).into_owned())
    })
}

impl Controller {
    /// The processes of the seats the controller launched and still tracks: the sessions with an
    /// open row, whose adapter says which process they run.
    fn tracked_seat_pids(&self) -> HashSet<u32> {
        let mut pids = HashSet::new();
        for handle in self.handles() {
            let Ok(sessions) = self.store.sessions(&handle.info.run_id) else {
                continue;
            };
            for session in sessions.iter().filter(|s| !s.ended) {
                if let Some(pid) = session
                    .process_ref
                    .as_ref()
                    .and_then(|r| handle.agent.process_id(&SessionRef(r.clone())))
                {
                    pids.insert(pid);
                }
            }
        }
        pids
    }

    /// Refuse a caller that is, or descends from, a seat (ADR-0005), or that is, or descends from,
    /// one of the live chat processes (Q2 rule 2): a chat cannot drive its own chat or another one,
    /// and it cannot claim, answer, clear, ack, arm, pair or revoke. Every failed read refuses. So is
    /// a caller whose environment holds the chat marker, which a child detached from the chat
    /// keeps (`r1.s4.w1`, R10).
    pub(super) fn check_chat_caller(&self, verb: &str, peer: Peer) -> Result<(), String> {
        let chats: HashSet<u32> = self
            .live_chats()
            .iter()
            .filter_map(|l| l.port.process_id(&l.chat))
            .collect();
        self.check_caller_among(verb, peer, &chats)
    }

    fn check_caller_among(
        &self,
        verb: &str,
        peer: Peer,
        chat_pids: &HashSet<u32>,
    ) -> Result<(), String> {
        let refuse = |why: String| format!("{verb} refused: {why}");
        let pid = peer.pid.ok_or_else(|| {
            refuse("the controller cannot tell which process called, so it refuses".to_string())
        })?;
        let proc_root = &self.cfg.proc_root;
        let environ_path = proc_root.join(pid.to_string()).join("environ");
        let environ = std::fs::read(&environ_path).map_err(|e| {
            refuse(format!(
                "cannot read {} ({e}), so the controller cannot tell the caller is not a seat",
                environ_path.display()
            ))
        })?;
        if environment_has_seat_token(&environ) {
            return Err(refuse(
                "PF_SEAT_TOKEN is in the caller's environment, so it is a seat".to_string(),
            ));
        }
        if let Some(session) = environment_chat_marker(&environ) {
            return Err(refuse(format!(
                "{CHAT_MARKER} is in the caller's environment, so it runs under chat session {session}"
            )));
        }
        let seats = self.tracked_seat_pids();
        let mut current = pid;
        for _ in 0..MAX_ANCESTORS {
            if seats.contains(&current) {
                return Err(refuse(format!(
                    "the caller is, or descends from, seat process {current}"
                )));
            }
            if chat_pids.contains(&current) {
                return Err(refuse(format!(
                    "the caller is, or descends from, chat process {current}"
                )));
            }
            let parent = parent_of(proc_root, current).map_err(refuse)?;
            if parent <= 1 {
                return Ok(());
            }
            current = parent;
        }
        Err(refuse(format!(
            "the caller's parent chain is longer than {MAX_ANCESTORS} processes, so the controller cannot clear it"
        )))
    }

    /// The generation of the claim that owns the token the request carries. A fenced or unknown
    /// token is refused, and the refusal never repeats the token.
    fn claim_generation(&self, args: &Value) -> Result<u64, String> {
        let token = text(args, "token").ok_or("the request carries no claim token")?;
        let hash = token_hash(token).ok_or("the claim token is not 64 hex digits")?;
        let claims = self.store.claims().map_err(|e| e.to_string())?;
        let owner = claim_of_hash(&claims, &hash)
            .ok_or("the claim token matches no claim: claim again with `pulse-factory claim`")?;
        check_claim_in(&claims, owner.generation).map_err(|why| {
            // A paired device took the claim: say so, so the operator knows where it went.
            let current = claims.iter().find(|c| c.fenced_at.is_none());
            match current {
                Some(c) if holder_device(&c.holder).is_some() => format!(
                    "{why}: the claim moved to {} at {}; claim again with pulse-factory claim",
                    c.holder, c.claimed_at
                ),
                _ => why,
            }
        })?;
        Ok(owner.generation)
    }

    /// The run a claim is taken in: the one named, else the latest that is not at an end state.
    fn claim_run(&self, run: Option<&str>) -> Result<Arc<RunHandle>, String> {
        if run.is_some() {
            return self.find(run);
        }
        self.handles()
            .into_iter()
            .rev()
            .find(|h| {
                self.store
                    .run(&h.info.run_id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| r.state == pf_engine::port::store::RunState::Active)
            })
            .ok_or_else(|| "there is no run that takes a claim: every run has ended".to_string())
    }

    // ------------------------------------------------------------------ claim

    pub(super) fn verb_claim(&self, args: &Value, peer: Peer) -> Response {
        if let Err(why) = self.check_chat_caller("claim", peer) {
            return err(why);
        }
        let holder = text(args, "holder").unwrap_or_default();
        if holder.trim().is_empty() {
            return err("claim refused: the claim needs a holder label");
        }
        // A label that reads as a paired device is the controller's to make (`r1.s4.w1`, R9).
        if holder.trim().starts_with("device ") {
            return err("claim refused: the holder label \"device …\" is kept for paired devices");
        }
        let handle = match self.claim_run(text(args, "run")) {
            Ok(h) => h,
            Err(why) => return err(format!("claim refused: {why}")),
        };
        // Fail closed: with no random bytes there is no token, and so no claim.
        let token = match random_bytes(&self.cfg.random_device, TOKEN_BYTES) {
            Ok(bytes) => hex(&bytes),
            Err(why) => return err(format!("claim refused: no token can be made: {why}")),
        };
        let Some(hash) = token_hash(&token) else {
            return err("claim refused: the token has the wrong shape");
        };
        let _one_at_a_time = lock(&self.claim_lock);
        match handle.engine.claim(holder, &hash) {
            Ok(granted) => Response::ok(json!({
                "schema_version": SCHEMA_VERSION,
                "generation": granted.generation,
                "holder": holder.trim(),
                "run": handle.info.run_id,
                // The only time the token leaves the controller.
                "token": token,
            })),
            Err(e) => err(format!("claim refused: {e}")),
        }
    }

    // ------------------------------------------------------------------ answer and clear

    pub(super) fn verb_answer(&self, args: &Value, peer: Peer) -> Response {
        if let Err(why) = self.check_chat_caller("answer", peer) {
            return err(why);
        }
        let (Some(id), Some(rev), Some(body)) = (
            text(args, "request"),
            args.get("rev").and_then(Value::as_u64),
            text(args, "text"),
        ) else {
            return err("answer refused: it needs a request ID, --rev and --text");
        };
        let Ok(rev) = u32::try_from(rev) else {
            return err("answer refused: --rev is too large");
        };
        let request = match self.store.request(id) {
            Ok(Some(r)) => r,
            Ok(None) => return err(format!("answer refused: request {id} is unknown")),
            Err(e) => return err(e.to_string()),
        };
        let handle = match self.find(Some(&request.run_id)) {
            Ok(h) => h,
            Err(why) => return err(format!("answer refused: {why}")),
        };
        let _one_at_a_time = lock(&self.claim_lock);
        let generation = match self.claim_generation(args) {
            Ok(g) => g,
            Err(why) => return err(format!("answer refused: {why}")),
        };
        match handle.driver.answer_request(id, rev, generation, body) {
            Ok(answered) => Response::ok(json!({
                "schema_version": SCHEMA_VERSION,
                "request": id,
                "revision": rev,
                "run": request.run_id,
                "unit": request.unit_id,
                "generation": generation,
                // A failed answer hook parks the unit again: the answer is recorded and the
                // unit does not resume.
                "resumed": request.unit_id.is_some() && answered.hook_failed.is_none(),
                "hook": match (answered.hook_ran, &answered.hook_failed) {
                    (false, _) => "none",
                    (true, None) => "passed",
                    (true, Some(_)) => "failed",
                },
                "hook_failure": answered.hook_failed,
            })),
            Err(why) => err(format!("answer refused: {why}")),
        }
    }

    pub(super) fn verb_clear(&self, args: &Value, peer: Peer) -> Response {
        if let Err(why) = self.check_chat_caller("clear", peer) {
            return err(why);
        }
        let (Some(run), Some(unit)) = (text(args, "run"), text(args, "unit")) else {
            return err("clear refused: it needs a run and an item");
        };
        let handle = match self.find(Some(run)) {
            Ok(h) => h,
            Err(why) => return err(format!("clear refused: {why}")),
        };
        let _one_at_a_time = lock(&self.claim_lock);
        let generation = match self.claim_generation(args) {
            Ok(g) => g,
            Err(why) => return err(format!("clear refused: {why}")),
        };
        let view = match handle.engine.view() {
            Ok(v) => v,
            Err(e) => return err(format!("clear refused: {e}")),
        };
        let Some(request) = view.open_request_of(unit) else {
            return err(format!(
                "clear refused: unit {unit} of run {run} has no open request"
            ));
        };
        let to_stage = text(args, "to");
        match handle
            .driver
            .clear_unit(unit, to_stage, generation, &request.id)
        {
            Ok(_) => {
                let resumed_at = self
                    .store
                    .request(&request.id)
                    .ok()
                    .flatten()
                    .and_then(|r| r.answer)
                    .and_then(|a| serde_json::from_str::<Value>(&a).ok())
                    .and_then(|a| a.get("to_stage").cloned())
                    .unwrap_or(Value::Null);
                Response::ok(json!({
                    "schema_version": SCHEMA_VERSION,
                    "run": run,
                    "unit": unit,
                    "request": request.id,
                    "revision": request.revision,
                    "generation": generation,
                    "to_stage": resumed_at,
                }))
            }
            Err(why) => err(format!("clear refused: {why}")),
        }
    }

    /// `ack`: the claim holder acks a `merge_ack` request, and the spine PR merges with the acked
    /// head SHA pinned (ADR-0005). It takes the claim as `answer` does: the same token, the same
    /// caller check and the same refusals. The engine reads the PR again before it merges.
    pub(super) fn verb_ack(&self, args: &Value, peer: Peer) -> Response {
        if let Err(why) = self.check_chat_caller("ack", peer) {
            return err(why);
        }
        let (Some(id), Some(rev)) = (
            text(args, "request"),
            args.get("rev").and_then(Value::as_u64),
        ) else {
            return err("ack refused: it needs a request ID and --rev");
        };
        let Ok(rev) = u32::try_from(rev) else {
            return err("ack refused: --rev is too large");
        };
        let request = match self.store.request(id) {
            Ok(Some(r)) => r,
            Ok(None) => return err(format!("ack refused: request {id} is unknown")),
            Err(e) => return err(e.to_string()),
        };
        let handle = match self.find(Some(&request.run_id)) {
            Ok(h) => h,
            Err(why) => return err(format!("ack refused: {why}")),
        };
        let _one_at_a_time = lock(&self.claim_lock);
        let generation = match self.claim_generation(args) {
            Ok(g) => g,
            Err(why) => return err(format!("ack refused: {why}")),
        };
        match handle.driver.ack_request(id, rev, generation) {
            Ok(acked) => {
                let (pr, base) = match self.store.request(id) {
                    Ok(Some(r)) => {
                        let body = serde_json::from_str::<Value>(&r.body).unwrap_or(Value::Null);
                        (body["pr"].clone(), body["base"].clone())
                    }
                    _ => (Value::Null, Value::Null),
                };
                Response::ok(json!({
                    "schema_version": SCHEMA_VERSION,
                    "request": id,
                    "revision": rev,
                    "run": request.run_id,
                    "unit": acked.unit,
                    "generation": generation,
                    "verb": "ack",
                    "pr": pr,
                    "base": base,
                    "head_sha": acked.head_sha,
                    "merged": acked.landed,
                    "outcome": acked.outcome,
                    "after_merge_failure": acked.after_merge_failure,
                }))
            }
            Err(why) => err(format!("ack refused: {why}")),
        }
    }

    // ------------------------------------------------------------------ requests and show

    /// What a request holds back: the unit, and each later unit the merge order holds back.
    fn blocks(&self, handle: &RunHandle, request: &RequestRow) -> Vec<String> {
        let Some(unit) = &request.unit_id else {
            return Vec::new();
        };
        if request.status != RequestStatus::Open {
            return Vec::new();
        }
        match handle.engine.view() {
            Ok(view) => blocked_by(&view, handle.engine.workflow(), unit),
            Err(_) => vec![unit.clone()],
        }
    }

    /// The row of `requests` and of the stream: the fields of the CLI's JSON, the verbs that answer the
    /// request, and for a `merge_ack` its PR, base and head SHA.
    pub(super) fn request_summary_of(&self, handle: &RunHandle, r: &RequestRow) -> RequestSummary {
        let body = |key: &str| -> Option<String> {
            (r.kind == RequestKind::MergeAck)
                .then(|| serde_json::from_str::<Value>(&r.body).ok())
                .flatten()
                .and_then(|b| b.get(key).and_then(Value::as_str).map(str::to_string))
        };
        RequestSummary {
            id: r.id.clone(),
            revision: r.revision,
            kind: r.kind.as_str().to_string(),
            status: r.status.as_str().to_string(),
            run: r.run_id.clone(),
            unit: r.unit_id.clone(),
            stage: r.stage.clone(),
            blocks: self.blocks(handle, r),
            opened_at: r.opened_at.clone(),
            answered_at: r.answered_at.clone(),
            reason: r.reason.clone(),
            verbs: verbs_of(r).into_iter().map(str::to_string).collect(),
            pr: body("pr"),
            base: body("base"),
            head_sha: body("head_sha"),
        }
    }

    /// The claim as a device sees it: no token, the device it belongs to, and whether that device is gone.
    pub(super) fn claim_record(&self, claim: &ClaimRow, devices: &[DeviceRow]) -> ClaimRecord {
        let device = holder_device(&claim.holder).map(str::to_string);
        let revoked = device
            .as_deref()
            .is_some_and(|id| devices.iter().any(|d| d.id == id && d.revoked_at.is_some()));
        ClaimRecord {
            generation: claim.generation,
            holder: claim.holder.clone(),
            device,
            revoked,
            claimed_at: claim.claimed_at.clone(),
        }
    }

    /// The current claim, or `None` when no claim was ever made.
    pub(super) fn current_claim_record(&self) -> Result<Option<ClaimRecord>, String> {
        let claims = self.store.claims().map_err(|e| e.to_string())?;
        let devices = self.store.devices().map_err(|e| e.to_string())?;
        Ok(claims
            .iter()
            .filter(|c| c.fenced_at.is_none())
            .max_by_key(|c| c.generation)
            .map(|c| self.claim_record(c, &devices)))
    }

    /// Every request of every run (or of the one named), as summaries, newest last.
    pub(super) fn request_summaries(
        &self,
        run: Option<&str>,
        all: bool,
    ) -> Result<Vec<RequestSummary>, String> {
        let handles = match run {
            Some(id) => vec![self.find(Some(id))?],
            None => self.handles(),
        };
        let mut found: Vec<(u64, RequestSummary)> = Vec::new();
        for handle in &handles {
            let rows = self
                .store
                .requests(&handle.info.run_id)
                .map_err(|e| e.to_string())?;
            for r in rows
                .iter()
                .filter(|r| all || r.status == RequestStatus::Open)
            {
                found.push((request_number(&r.id), self.request_summary_of(handle, r)));
            }
        }
        // Newest last.
        found.sort_by_key(|(n, _)| *n);
        Ok(found.into_iter().map(|(_, v)| v).collect())
    }

    pub(super) fn verb_requests(&self, args: &Value) -> Response {
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
        let requests = match self.request_summaries(text(args, "run"), all) {
            Ok(r) => r,
            Err(why) => return err(why),
        };
        let claim = match self.current_claim_record() {
            Ok(c) => c,
            Err(why) => return err(why),
        };
        let list = RequestList {
            schema_version: SCHEMA_VERSION,
            open: requests.iter().filter(|r| r.status == "open").count() as u64,
            requests,
            claim,
        };
        Response::ok(serde_json::to_value(list).unwrap_or(Value::Null))
    }

    pub(super) fn verb_show(&self, args: &Value) -> Response {
        let Some(id) = text(args, "id") else {
            return err("show needs a request ID");
        };
        let request = match self.store.request(id) {
            Ok(Some(r)) => r,
            Ok(None) => return err(format!("request {id} is unknown")),
            Err(e) => return err(e.to_string()),
        };
        let handle = match self.find(Some(&request.run_id)) {
            Ok(h) => h,
            Err(why) => return err(why),
        };
        let parse = |t: &str| serde_json::from_str::<Value>(t).unwrap_or(Value::Null);
        let detail = RequestDetail {
            schema_version: SCHEMA_VERSION,
            summary: self.request_summary_of(&handle, &request),
            body: parse(&request.body),
            answer: request.answer.as_deref().map_or(Value::Null, parse),
            answer_generation: request.answer_generation,
        };
        Response::ok(serde_json::to_value(detail).unwrap_or(Value::Null))
    }
}

/// The verbs that answer a request: a unit's request takes `answer` and `clear`; a `merge_ack`
/// request takes `ack` and no other verb; a run-level request takes `answer`; a closed one takes none.
pub(super) fn verbs_of(request: &RequestRow) -> Vec<&'static str> {
    match (request.status, &request.unit_id) {
        (RequestStatus::Open, _) if request.kind == RequestKind::MergeAck => vec!["ack"],
        (RequestStatus::Open, Some(_)) => vec!["answer", "clear"],
        (RequestStatus::Open, None) => vec!["answer"],
        _ => Vec::new(),
    }
}

/// The number in `R-<n>`, for sorting. An ID that does not read sorts first.
fn request_number(id: &str) -> u64 {
    id.strip_prefix("R-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
