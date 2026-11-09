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

use pf_engine::claim::{check_claim_in, claim_of_hash, sha256_hex};
use pf_engine::merge::blocked_by;
use pf_engine::port::agent::SessionRef;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus, Store};
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

    /// Refuse a caller that is, or descends from, a seat (ADR-0005). Every failed read refuses.
    pub(super) fn check_caller(&self, verb: &str, peer: Peer) -> Result<(), String> {
        self.check_caller_among(verb, peer, &HashSet::new())
    }

    /// The same check, and a caller that is, or descends from, one of the live chat processes
    /// is refused too (ADR-0005, Q2 rule 2): a chat cannot drive its own chat or another one.
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
        check_claim_in(&claims, owner.generation)?;
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
        if let Err(why) = self.check_caller("claim", peer) {
            return err(why);
        }
        let holder = text(args, "holder").unwrap_or_default();
        if holder.trim().is_empty() {
            return err("claim refused: the claim needs a holder label");
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
        if let Err(why) = self.check_caller("answer", peer) {
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
        if let Err(why) = self.check_caller("clear", peer) {
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
        if let Err(why) = self.check_caller("ack", peer) {
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

    fn request_json(&self, handle: &RunHandle, r: &RequestRow) -> Value {
        json!({
            "id": r.id,
            "revision": r.revision,
            "kind": r.kind.as_str(),
            "status": r.status.as_str(),
            "run": r.run_id,
            "unit": r.unit_id,
            "stage": r.stage,
            "blocks": self.blocks(handle, r),
            "opened_at": r.opened_at,
            "answered_at": r.answered_at,
            "reason": r.reason,
        })
    }

    pub(super) fn verb_requests(&self, args: &Value) -> Response {
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
        let handles = match text(args, "run") {
            Some(id) => match self.find(Some(id)) {
                Ok(h) => vec![h],
                Err(why) => return err(why),
            },
            None => self.handles(),
        };
        let mut found: Vec<(u64, Value)> = Vec::new();
        for handle in &handles {
            let rows = match self.store.requests(&handle.info.run_id) {
                Ok(rows) => rows,
                Err(e) => return err(e.to_string()),
            };
            for r in rows
                .iter()
                .filter(|r| all || r.status == RequestStatus::Open)
            {
                found.push((request_number(&r.id), self.request_json(handle, r)));
            }
        }
        // Newest last.
        found.sort_by_key(|(n, _)| *n);
        let requests: Vec<Value> = found.into_iter().map(|(_, v)| v).collect();
        Response::ok(json!({
            "schema_version": SCHEMA_VERSION,
            "open": requests.iter().filter(|r| r["status"] == "open").count(),
            "requests": requests,
        }))
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
        // The verbs that answer it: a unit's request takes `answer` and `clear`. A `merge_ack`
        // request takes `ack` and no other verb.
        let verbs: Vec<&str> = match (request.status, &request.unit_id) {
            (RequestStatus::Open, _) if request.kind == RequestKind::MergeAck => vec!["ack"],
            (RequestStatus::Open, Some(_)) => vec!["answer", "clear"],
            (RequestStatus::Open, None) => vec!["answer"],
            _ => Vec::new(),
        };
        let mut shown = self.request_json(&handle, &request);
        let fields = shown.as_object_mut().expect("a request is an object");
        fields.insert("body".to_string(), parse(&request.body));
        fields.insert(
            "answer".to_string(),
            request.answer.as_deref().map_or(Value::Null, parse),
        );
        fields.insert(
            "answer_generation".to_string(),
            json!(request.answer_generation),
        );
        fields.insert("verbs".to_string(), json!(verbs));
        fields.insert("schema_version".to_string(), json!(SCHEMA_VERSION));
        Response::ok(shown)
    }
}

/// The number in `R-<n>`, for sorting. An ID that does not read sorts first.
fn request_number(id: &str) -> u64 {
    id.strip_prefix("R-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
