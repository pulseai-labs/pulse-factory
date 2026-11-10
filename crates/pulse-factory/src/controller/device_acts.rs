//! The verbs a paired device uses on requests: `confirm_code`, and `answer`, `clear` and `ack` with a
//! code (`r1.s4.w1`; ADR-0005, ADR-0009, ADR-0012; `docs/contracts/api.md`, `docs/contracts/requests.md`).
//!
//! **Issuing a code changes no claim.** `confirm_code` is an admission under `@controller` with a command
//! ID and no effect. The code binds the claim generation current at issue, or none.
//!
//! **The spend moves the claim and acts, in one admission.** `device_answer`, `device_clear` and
//! `device_ack` run under `claim_lock`: `take` spends the code, `fresh` reads the subject again, and the
//! claim decision either keeps the device's own claim, fences the claim the code saw and puts the
//! device's claim at the next generation (the effects are the existing `Effect::PutClaim`s), or refuses
//! `claim_moved` when the claim moved since the issue to someone else. The engine act follows at once
//! under the resulting generation, still under `claim_lock`. No token exists for a device's claim.
//!
//! A refusal before the plan (a malformed call, an unknown request, a revoked device) is a plain answer
//! with no admission. A refusal of the plan is a refused admission with its ledger line.

use pf_api::host::REFUSALS;
use pf_api::types::{
    ConfirmCodeArgs, ConfirmIssued, ConfirmSubjectArg, ConfirmSubjectShown, ConfirmVerb,
    DeviceAckArgs, DeviceAnswerArgs, DeviceClearArgs, DeviceRecord, RequestActed, SCHEMA_VERSION,
};
use pf_engine::claim::{claim_effects, device_holder, holder_device, sha256_hex};
use pf_engine::port::store::{
    CONTROLLER_SCOPE, ClaimRow, DeviceRow, RequestKind, RequestRow, RequestStatus, Store,
};
use pf_ipc::{Request, Response};
use serde_json::{Value, json};

use super::commands::{Plan, check_command_id, field, now};
use super::confirm::{CODE_BYTES, CODE_PREFIX, CodeRefusal, RequestVerb, Subject};
use super::seat::{hex, random_bytes};
use super::{Controller, lock};

/// The bytes of the hash a device's claim holds in place of a token. Nobody keeps them.
const NO_TOKEN_BYTES: usize = 32;

/// A refusal with a word from the closed list: `result` is `{"refusal": "<word>"}`.
pub(super) fn refusal(word: &str, text: impl Into<String>) -> Response {
    let mut r = Response::err(text);
    r.result = json!({ "refusal": word });
    r
}

/// Split a stored refusal reason `<word>: <text>` back into its word and text. A reason with no word
/// from the list keeps its text whole.
fn split_reason(reason: &str) -> (Option<&'static str>, String) {
    for word in REFUSALS {
        if let Some(text) = reason
            .strip_prefix(word)
            .and_then(|rest| rest.strip_prefix(": "))
        {
            return (Some(word), text.to_string());
        }
    }
    (None, reason.to_string())
}

pub(super) fn refused_reason(reason: &str) -> Response {
    match split_reason(reason) {
        (Some(word), text) => refusal(word, text),
        (None, text) => Response::err(text),
    }
}

/// A refusal of the plan: its word, when it has one, and its text.
pub(super) type Refused = (Option<&'static str>, String);

pub(super) fn plan_refused(why: Refused) -> Plan {
    Plan::Refuse(match why {
        (Some(word), text) => format!("{word}: {text}"),
        (None, text) => text,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Answer,
    Clear,
    Ack,
}

impl Act {
    fn kind(self) -> &'static str {
        match self {
            Act::Answer => "answer",
            Act::Clear => "clear",
            Act::Ack => "ack",
        }
    }

    fn admission_kind(self) -> &'static str {
        match self {
            Act::Answer => "device_answer",
            Act::Clear => "device_clear",
            Act::Ack => "device_ack",
        }
    }
}

/// The arguments of a device act, whichever verb.
struct ActArgs {
    request: String,
    rev: u32,
    text: Option<String>,
    to: Option<String>,
    code: String,
    command_id: String,
}

fn parse_act(act: Act, args: &Value) -> Result<ActArgs, String> {
    let bad = |e: serde_json::Error| format!("the arguments are not readable: {e}");
    Ok(match act {
        Act::Answer => {
            let a: DeviceAnswerArgs = serde_json::from_value(args.clone()).map_err(bad)?;
            ActArgs {
                request: a.request,
                rev: a.rev,
                text: Some(a.text),
                to: None,
                code: a.code,
                command_id: a.command_id,
            }
        }
        Act::Clear => {
            let a: DeviceClearArgs = serde_json::from_value(args.clone()).map_err(bad)?;
            ActArgs {
                request: a.request,
                rev: a.rev,
                text: None,
                to: a.to,
                code: a.code,
                command_id: a.command_id,
            }
        }
        Act::Ack => {
            let a: DeviceAckArgs = serde_json::from_value(args.clone()).map_err(bad)?;
            ActArgs {
                request: a.request,
                rev: a.rev,
                text: None,
                to: None,
                code: a.code,
                command_id: a.command_id,
            }
        }
    })
}

/// Is `text` 64 lower-case hex digits?
fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn body_text(row: &RequestRow, key: &str) -> Option<String> {
    serde_json::from_str::<Value>(&row.body)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

impl Controller {
    /// The one way into the device path. Every verb but the four device verbs goes to `handle`.
    pub fn handle_device(&self, device: &DeviceRecord, request: Request) -> Response {
        match request.verb.as_str() {
            "confirm_code" => self.device_confirm_code(device, &request.args),
            "answer" => self.acted(device, Act::Answer, &request.args),
            "clear" => self.acted(device, Act::Clear, &request.args),
            "ack" => self.acted(device, Act::Ack, &request.args),
            // A device arms the run of a start card with a confirm code (`r1.s4.w2`).
            "arm" => self.device_arm(device, &request.args),
            _ => {
                use pf_ipc::Handler;
                self.handle(request)
            }
        }
    }

    /// A device act, and then the frames of what it changed.
    fn acted(&self, device: &DeviceRecord, act: Act, args: &Value) -> Response {
        let response = self.device_act(device, act, args);
        self.publish_request_frames();
        response
    }

    /// The device, read again: a revoke that committed after the router's token check refuses here.
    pub(super) fn live_device(&self, id: &str) -> Result<DeviceRow, Response> {
        match self.store.devices() {
            Ok(rows) => match rows.into_iter().find(|d| d.id == id) {
                Some(d) if d.revoked_at.is_none() => Ok(d),
                _ => Err(refusal(
                    "device_revoked",
                    "the device was revoked; pair it again",
                )),
            },
            Err(e) => Err(Response::err(e.to_string())),
        }
    }

    fn current_claim(&self) -> Result<Option<ClaimRow>, String> {
        let claims = self.store.claims().map_err(|e| e.to_string())?;
        Ok(claims
            .into_iter()
            .filter(|c| c.fenced_at.is_none())
            .max_by_key(|c| c.generation))
    }

    // ------------------------------------------------------------------ confirm_code

    fn device_confirm_code(&self, device: &DeviceRecord, args: &Value) -> Response {
        let args: ConfirmCodeArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => {
                return Response::err(format!(
                    "confirm_code refused: the arguments are not readable: {e}"
                ));
            }
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        let (request_id, revision, verb, text_sha256, to) = match args.subject {
            ConfirmSubjectArg::Request {
                request,
                revision,
                verb,
                text_sha256,
                to,
            } => (request, revision, verb, text_sha256, to),
            ConfirmSubjectArg::Arm {
                proposal,
                plan_sha256,
            } => {
                return self.device_confirm_arm(device, &args.command_id, &proposal, &plan_sha256);
            }
        };
        // Fail closed: with no random bytes there is no code, and no admission.
        let bytes = match random_bytes(&self.cfg.random_device, CODE_BYTES) {
            Ok(b) => b,
            Err(why) => {
                return Response::err(format!("confirm_code refused: no code can be made: {why}"));
            }
        };
        let _one_at_a_time = lock(&self.claim_lock);
        let dev = match self.live_device(&device.id) {
            Ok(d) => d,
            Err(r) => return r,
        };
        let content = json!({
            "device": dev.id,
            "subject": {
                "kind": "request",
                "request": request_id,
                "revision": revision,
                "verb": match verb { ConfirmVerb::Answer => "answer", ConfirmVerb::Clear => "clear", ConfirmVerb::Ack => "ack" },
                "text_sha256": text_sha256,
                "to": to,
            },
        })
        .to_string();
        // A command the store has seen is decided by its record: a replay, a conflict, or the first refusal.
        let existing = matches!(
            self.store.admission(CONTROLLER_SCOPE, &args.command_id),
            Ok(Some(_))
        );
        if existing {
            return match self.run_command(
                "confirm_code",
                &args.command_id,
                content,
                Plan::Refuse(String::new()),
            ) {
                Ok((_, true)) => self.replayed_issue(&dev, &args.command_id, &request_id, revision),
                Ok((_, false)) => {
                    Response::err("confirm_code refused: the store answered a replay as new")
                }
                Err(why) => Response::err(why),
            };
        }
        let row = match self.store.request(&request_id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Response::err(format!(
                    "confirm_code refused: request {request_id} is unknown"
                ));
            }
            Err(e) => return Response::err(e.to_string()),
        };
        if row.status != RequestStatus::Open {
            return refusal(
                "subject_moved",
                format!("request {request_id} is not open any more"),
            );
        }
        if row.revision != revision {
            return refusal(
                "subject_moved",
                format!(
                    "request {request_id} is at revision {}, not {revision}",
                    row.revision
                ),
            );
        }
        let taken = super::requests::verbs_of(&row);
        let wanted = match verb {
            ConfirmVerb::Answer => "answer",
            ConfirmVerb::Clear => "clear",
            ConfirmVerb::Ack => "ack",
        };
        if !taken.contains(&wanted) {
            return Response::err(format!(
                "confirm_code refused: request {request_id} takes {}, not {wanted}",
                taken.join(" or ")
            ));
        }
        let verb = match verb {
            ConfirmVerb::Answer => {
                let Some(hash) = text_sha256.filter(|h| is_sha256_hex(h)) else {
                    return Response::err(
                        "confirm_code refused: an answer code needs text_sha256, 64 lower-case hex digits",
                    );
                };
                RequestVerb::Answer { text_sha256: hash }
            }
            ConfirmVerb::Clear => {
                if let Some(to) = &to
                    && let Err(why) = self.check_stage(&row, to)
                {
                    return Response::err(format!("confirm_code refused: {why}"));
                }
                RequestVerb::Clear { to }
            }
            ConfirmVerb::Ack => RequestVerb::Ack,
        };
        let head_sha = (row.kind == RequestKind::MergeAck)
            .then(|| body_text(&row, "head_sha"))
            .flatten();
        let current = match self.current_claim() {
            Ok(c) => c,
            Err(e) => return Response::err(e),
        };
        let generation = current.as_ref().map(|c| c.generation);
        let mut line = format!(
            "confirm_code {} request={}@{} verb={} generation={} command={}",
            field(&dev.id),
            field(&request_id),
            revision,
            wanted,
            generation.map_or_else(|| "none".to_string(), |g| g.to_string()),
            args.command_id
        );
        if row.kind == RequestKind::MergeAck {
            line.push_str(&format!(
                " pr={} base={} head={}",
                field(&body_text(&row, "pr").unwrap_or_default()),
                field(&body_text(&row, "base").unwrap_or_default()),
                field(head_sha.as_deref().unwrap_or_default()),
            ));
        }
        let plan = Plan::Admit {
            effects: Vec::new(),
            subject: request_id.clone(),
            line,
        };
        match self.run_command("confirm_code", &args.command_id, content, plan) {
            Ok((_, true)) => self.replayed_issue(&dev, &args.command_id, &request_id, revision),
            Ok((_, false)) => {
                let code = format!("{CODE_PREFIX}{}", hex(&bytes));
                let at = std::time::SystemTime::now()
                    .checked_add(self.cfg.confirm_ttl)
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .unwrap_or_default();
                let expires_at = pf_engine::engine::format_utc(at.as_secs(), at.subsec_millis());
                let answer = match self.issued_answer(
                    &dev,
                    &request_id,
                    revision,
                    false,
                    Some(code.clone()),
                    Some(expires_at),
                    generation,
                ) {
                    Ok(a) => a,
                    Err(r) => return r,
                };
                // The first record a replay of this command ID gives back: no code, `replayed` true.
                let first = ConfirmIssued {
                    replayed: true,
                    code: None,
                    ..answer.clone()
                };
                self.confirm.issue(
                    &dev.id,
                    sha256_hex(code.as_bytes()),
                    Subject::Request {
                        id: request_id.clone(),
                        revision,
                        verb,
                        head_sha,
                    },
                    generation,
                    self.cfg.confirm_ttl,
                    self.cfg.confirm_tombstone,
                    (args.command_id.clone(), first),
                );
                Response::ok(serde_json::to_value(answer).unwrap_or(Value::Null))
            }
            Err(why) => Response::err(why),
        }
    }

    /// The answer to a replay of a `confirm_code` command: the first answer, with no code. The table keeps
    /// it for as long as it keeps the code. Past that (a restart voided every code; or 10 minutes passed
    /// after the expiry) the first answer is gone, so the reply carries the facts the store still has and
    /// leaves `generation` and `expires_at` null.
    fn replayed_issue(
        &self,
        dev: &DeviceRow,
        command_id: &str,
        request_id: &str,
        revision: u32,
    ) -> Response {
        if let Some(first) = self.confirm.first_answer(&dev.id, command_id) {
            return Response::ok(serde_json::to_value(first).unwrap_or(Value::Null));
        }
        match self.issued_answer(dev, request_id, revision, true, None, None, None) {
            Ok(a) => Response::ok(serde_json::to_value(a).unwrap_or(Value::Null)),
            Err(r) => r,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn issued_answer(
        &self,
        dev: &DeviceRow,
        request_id: &str,
        revision: u32,
        replayed: bool,
        code: Option<String>,
        expires_at: Option<String>,
        generation: Option<u64>,
    ) -> Result<ConfirmIssued, Response> {
        let row = match self.store.request(request_id) {
            Ok(Some(r)) => r,
            _ => return Err(Response::err(format!("request {request_id} is unknown"))),
        };
        let holder = self.current_claim_record().ok().flatten();
        let issued = ConfirmIssued {
            schema_version: SCHEMA_VERSION,
            replayed,
            code,
            expires_at,
            device: dev.id.clone(),
            generation,
            holder,
            start: None,
            subject: ConfirmSubjectShown {
                request: row.id.clone(),
                revision,
                kind: row.kind.as_str().to_string(),
                run: row.run_id.clone(),
                unit: row.unit_id.clone(),
                reason: row.reason.clone(),
                pr: body_text(&row, "pr"),
                base: body_text(&row, "base"),
                head_branch: body_text(&row, "head_branch"),
                head_sha: body_text(&row, "head_sha"),
            },
        };
        Ok(issued)
    }

    /// Is `to` a stage of the track of the unit that the request parks?
    fn check_stage(&self, row: &RequestRow, to: &str) -> Result<(), String> {
        let unit = row
            .unit_id
            .as_deref()
            .ok_or_else(|| format!("request {} names no unit, so it has no stage", row.id))?;
        let handle = self.find(Some(&row.run_id))?;
        let view = handle.engine.view().map_err(|e| e.to_string())?;
        let node = view
            .node(unit)
            .ok_or_else(|| format!("unit {unit} is unknown"))?;
        let detail = view.unit_detail(node)?;
        let track = handle
            .engine
            .workflow()
            .track(&detail.track)
            .ok_or_else(|| format!("track {} is unknown", detail.track))?;
        if track.stages.iter().any(|s| s.name == to) {
            return Ok(());
        }
        let names: Vec<&str> = track.stages.iter().map(|s| s.name.as_str()).collect();
        Err(format!(
            "{to:?} is not a stage of track {} (its stages: {})",
            track.name,
            names.join(", ")
        ))
    }

    // ------------------------------------------------------------------ answer, clear, ack

    fn device_act(&self, device: &DeviceRecord, act: Act, args: &Value) -> Response {
        let verb = act.kind();
        let a = match parse_act(act, args) {
            Ok(a) => a,
            Err(why) => return Response::err(format!("{verb} refused: {why}")),
        };
        if let Err(why) = check_command_id(&a.command_id) {
            return Response::err(why);
        }
        let _one_at_a_time = lock(&self.claim_lock);
        let dev = match self.live_device(&device.id) {
            Ok(d) => d,
            Err(r) => return r,
        };
        let code_sha = sha256_hex(a.code.as_bytes());
        let call_verb = match act {
            Act::Answer => RequestVerb::Answer {
                text_sha256: sha256_hex(a.text.as_deref().unwrap_or_default().as_bytes()),
            },
            Act::Clear => RequestVerb::Clear { to: a.to.clone() },
            Act::Ack => RequestVerb::Ack,
        };
        let mut content = json!({
            "verb": verb,
            "request": a.request,
            "rev": a.rev,
            "code_sha256": code_sha,
            "device": dev.id,
        });
        if let RequestVerb::Answer { text_sha256 } = &call_verb {
            content["text_sha256"] = json!(text_sha256);
        }
        if act == Act::Clear {
            content["to"] = json!(a.to);
        }
        let content = content.to_string();
        let kind = act.admission_kind();
        let existing = matches!(
            self.store.admission(CONTROLLER_SCOPE, &a.command_id),
            Ok(Some(_))
        );
        let (plan, acting) = if existing {
            // The store decides: a replay answers the first record and acts on nothing.
            (Plan::Refuse(String::new()), None)
        } else {
            match self.plan_act(&dev, act, &a, &code_sha, call_verb) {
                Ok((plan, generation)) => (plan, Some(generation)),
                Err(why) => (plan_refused(why), None),
            }
        };
        match self.run_command(kind, &a.command_id, content, plan) {
            Err(reason) => refused_reason(&reason),
            Ok((_, true)) => replay_response(act, &dev.id),
            Ok((_, false)) => match acting {
                Some(generation) => self.run_act(act, &dev, &a, generation),
                None => Response::err(format!(
                    "{verb} refused: the store answered a replay as new"
                )),
            },
        }
    }

    /// The plan of a spend: the code, the subject and the claim, decided in this order.
    fn plan_act(
        &self,
        dev: &DeviceRow,
        act: Act,
        a: &ActArgs,
        code_sha: &str,
        call_verb: RequestVerb,
    ) -> Result<(Plan, u64), Refused> {
        let plain = |e: String| -> Refused { (None, e) };
        let call = Subject::Request {
            id: a.request.clone(),
            revision: a.rev,
            verb: call_verb,
            head_sha: None,
        };
        let issued = self
            .confirm
            .take(&dev.id, code_sha, &call, self.cfg.confirm_tombstone)
            .map_err(|r| (Some(r.word()), r.text().to_string()))?;
        issued
            .subject
            .fresh(self)
            .map_err(|why| (Some("subject_moved"), why))?;
        let Subject::Request {
            id,
            revision,
            head_sha,
            ..
        } = &issued.subject
        else {
            return Err((
                Some("code_subject"),
                CodeRefusal::Subject.text().to_string(),
            ));
        };
        // The claim.
        let claims = self.store.claims().map_err(|e| plain(e.to_string()))?;
        let current = claims
            .iter()
            .filter(|c| c.fenced_at.is_none())
            .max_by_key(|c| c.generation);
        let label = device_holder(&dev.id, &dev.name);
        let (generation, effects, from) = match current {
            Some(c) if holder_device(&c.holder) == Some(dev.id.as_str()) => {
                (c.generation, Vec::new(), "held".to_string())
            }
            cur if cur.map(|c| c.generation) == issued.generation => {
                let bytes =
                    random_bytes(&self.cfg.random_device, NO_TOKEN_BYTES).map_err(|why| {
                        plain(format!("the claim cannot be made: no random bytes ({why})"))
                    })?;
                let (generation, effects) =
                    claim_effects(&claims, &label, &sha256_hex(&bytes), &now()).map_err(plain)?;
                let from = cur.map_or_else(|| "none".to_string(), |c| c.generation.to_string());
                (generation, effects, from)
            }
            Some(c) => {
                return Err((
                    Some("claim_moved"),
                    format!("the claim moved to {}", c.holder),
                ));
            }
            None => {
                return Err((
                    Some("claim_moved"),
                    "the claim moved: no claim holds it now".to_string(),
                ));
            }
        };
        let mut line = format!(
            "{} {} {}@{} generation={} from={} admitted command={}",
            act.admission_kind(),
            field(&dev.id),
            field(id),
            revision,
            generation,
            from,
            a.command_id
        );
        if act == Act::Ack
            && let Ok(Some(row)) = self.store.request(id)
        {
            line.push_str(&format!(
                " pr={} base={} head={}",
                field(&body_text(&row, "pr").unwrap_or_default()),
                field(&body_text(&row, "base").unwrap_or_default()),
                field(head_sha.as_deref().unwrap_or_default()),
            ));
        }
        Ok((
            Plan::Admit {
                effects,
                subject: id.clone(),
                line,
            },
            generation,
        ))
    }

    /// The engine act, under the generation the admission made. An engine refusal after the admission
    /// leaves the claim with the device: it acted.
    fn run_act(&self, act: Act, dev: &DeviceRow, a: &ActArgs, generation: u64) -> Response {
        let verb = act.kind();
        let request = match self.store.request(&a.request) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Response::err(format!("{verb} refused: request {} is unknown", a.request));
            }
            Err(e) => return Response::err(e.to_string()),
        };
        let handle = match self.find(Some(&request.run_id)) {
            Ok(h) => h,
            Err(why) => return Response::err(format!("{verb} refused: {why}")),
        };
        let mut acted = RequestActed {
            schema_version: SCHEMA_VERSION,
            replayed: false,
            verb: verb.to_string(),
            device: dev.id.clone(),
            generation: Some(generation),
            request: None,
            merged: None,
            head_sha: None,
            pr: None,
            base: None,
            outcome: None,
            resumed: None,
            hook: None,
            to_stage: None,
        };
        match act {
            Act::Answer => {
                let text = a.text.as_deref().unwrap_or_default();
                match handle
                    .driver
                    .answer_request(&a.request, a.rev, generation, text)
                {
                    Ok(answered) => {
                        acted.resumed =
                            Some(request.unit_id.is_some() && answered.hook_failed.is_none());
                        acted.hook = Some(
                            match (answered.hook_ran, &answered.hook_failed) {
                                (false, _) => "none",
                                (true, None) => "passed",
                                (true, Some(_)) => "failed",
                            }
                            .to_string(),
                        );
                    }
                    Err(why) => return Response::err(format!("answer refused: {why}")),
                }
            }
            Act::Clear => {
                let Some(unit) = request.unit_id.as_deref() else {
                    return Response::err("clear refused: the request names no unit");
                };
                match handle
                    .driver
                    .clear_unit(unit, a.to.as_deref(), generation, &a.request)
                {
                    Ok(_) => {
                        acted.to_stage = self
                            .store
                            .request(&a.request)
                            .ok()
                            .flatten()
                            .and_then(|r| r.answer)
                            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                            .and_then(|v| {
                                v.get("to_stage")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            });
                    }
                    Err(why) => return Response::err(format!("clear refused: {why}")),
                }
            }
            Act::Ack => match handle.driver.ack_request(&a.request, a.rev, generation) {
                Ok(acked) => {
                    acted.merged = Some(acked.landed);
                    acted.head_sha = Some(acked.head_sha);
                    acted.outcome = Some(acked.outcome);
                }
                Err(why) => {
                    // A head that moved at the forge revises the request inside the merge step.
                    let moved = self
                        .store
                        .request(&a.request)
                        .ok()
                        .flatten()
                        .is_some_and(|r| r.revision > a.rev);
                    return if moved {
                        refusal("subject_moved", format!("ack refused: {why}"))
                    } else {
                        Response::err(format!("ack refused: {why}"))
                    };
                }
            },
        }
        if let Ok(Some(after)) = self.store.request(&a.request) {
            if act == Act::Ack {
                acted.pr = body_text(&after, "pr");
                acted.base = body_text(&after, "base");
            }
            acted.request = Some(self.request_summary_of(&handle, &after));
        }
        Response::ok(serde_json::to_value(acted).unwrap_or(Value::Null))
    }
}

/// The answer to a replay of a spend: nothing ran again, so every field after the device is null.
fn replay_response(act: Act, device: &str) -> Response {
    let acted = RequestActed {
        schema_version: SCHEMA_VERSION,
        replayed: true,
        verb: act.kind().to_string(),
        device: device.to_string(),
        generation: None,
        request: None,
        merged: None,
        head_sha: None,
        pr: None,
        base: None,
        outcome: None,
        resumed: None,
        hook: None,
        to_stage: None,
    };
    Response::ok(serde_json::to_value(acted).unwrap_or(Value::Null))
}
