//! The `settle` verb: the project's Orchestrator settles a park below the line under the project's
//! clear grant (ADR-0004, ADR-0005, amended; `docs/contracts/requests.md`, "Settle").
//!
//! The verb reads no claim token and never joins the device allowlist. The checks run in this order,
//! each refusal starting `settle refused: `:
//!
//! 1. the OS: only Linux admits a settle (the grant can still be given and ended elsewhere);
//! 2. the caller, from its process and its host ([`super::callers`]);
//! 3. the arguments;
//! 4. who: a live session of the `orchestrator` agent, a known request, a run with a project, and
//!    that project is the session's;
//! 5. no read of the admission here: a retry passes the checks again and reaches the engine, whose
//!    first step under its lock is the replay;
//! 6. the grant: a live row of the project's grant (a removed project has none);
//! 7. the act list: `authority.clear_acts`, resolved fresh;
//! 8. `close_stale`, the one act this server does not build;
//! 9. the pins, in [`super::driver::Driver::settle`];
//! 10. the reason, redacted once and then bounded; for `rerun_check`, the limit
//!     `authority.rerun_checks_per_head`, read fresh;
//! 11. `Engine::settle`, under the engine's lock;
//! 12. the effect of an act that has one, after an admitted commit that is not a replay: the unit
//!     is marked on the host service for a re-attach, the engine runs the effect and stores its
//!     result, and the mark is cleared;
//! 13. the answer.
//!
//! Every refusal after step 1 is audited through [`Controller::refuse_coalesced`], under an ID the
//! controller makes, never under the settle's own ID.

use pf_api::types::{RequestSettled, SCHEMA_VERSION, SettleAnswer};
use pf_engine::claim::{
    Act, EffectKind, SettleAsk, SettleError, SettleFacts, VERB_SETTLE, effect_of, live_grant,
};
use pf_engine::port::store::Cause;
use pf_engine::port::store::{ORCHESTRATOR_AGENT, Store};
use pf_engine::status::{one_line_reason, redact};
use pf_ipc::{Peer, PeerSocket, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Controller;
use super::commands::{Plan, field};
use super::device_acts::refusal;
use super::requests::decision_of;
use super::settings::AUTHORITY_CLEAR_ACTS;

/// The most characters of a reason: it is one line, bounded.
pub const REASON_MAX_CHARS: usize = 300;

/// The most characters of an argument that a refusal text repeats.
const ECHO_MAX_CHARS: usize = 40;

/// The kind of the controller's audited refusal of a settle.
const KIND: &str = "settle";

/// The kind of the admission that stores a run's armed launch count.
pub const RUN_LAUNCH_BOUND: &str = "run_launch_bound";

/// The arguments of `settle`.
#[derive(Deserialize)]
struct SettleArgs {
    request_id: String,
    revision: u32,
    act: String,
    #[serde(default)]
    to_stage: Option<String>,
    reason: String,
}

/// Why a settle did not commit: a refusal with its text, or a `subject_moved` refusal.
enum Refused {
    Text(String),
    Moved(String),
}

impl Refused {
    fn text(&self) -> &str {
        match self {
            Refused::Text(t) | Refused::Moved(t) => t,
        }
    }
}

fn text(why: impl Into<String>) -> Refused {
    Refused::Text(why.into())
}

/// A caller-chosen value, cut and with every control character replaced, for a refusal text.
fn echo(value: &str) -> String {
    let clipped: String = value
        .chars()
        .take(ECHO_MAX_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if value.chars().count() > ECHO_MAX_CHARS {
        format!("{clipped}...")
    } else {
        clipped
    }
}

/// What a stored answer of a settle says, or `None` when the answer is not a settle's.
pub(super) fn settled_of(answer: &str) -> Option<RequestSettled> {
    let v: Value = serde_json::from_str(answer).ok()?;
    if v.get("verb")?.as_str()? != VERB_SETTLE {
        return None;
    }
    let grant = v.get("grant")?;
    Some(RequestSettled {
        act: v.get("act")?.as_str()?.to_string(),
        to_stage: v
            .get("to_stage")
            .and_then(Value::as_str)
            .map(str::to_string),
        reason: v.get("reason")?.as_str()?.to_string(),
        session: grant.get("session")?.as_str()?.to_string(),
        project: grant.get("project")?.as_str()?.to_string(),
        generation: grant.get("generation")?.as_u64()?,
        agent: grant.get("agent")?.as_str()?.to_string(),
    })
}

/// The class a stored settle answer recorded, `below` or `above`.
pub(super) fn recorded_line(answer: &str) -> Option<String> {
    let v: Value = serde_json::from_str(answer).ok()?;
    if v.get("verb")?.as_str()? != VERB_SETTLE {
        return None;
    }
    v.get("line")?.get("class")?.as_str().map(str::to_string)
}

impl Controller {
    /// `settle`, for the socket with the caller's process and a duplicate of its connection.
    pub(super) fn verb_settle(
        &self,
        args: &Value,
        peer: Peer,
        socket: Option<PeerSocket>,
    ) -> Response {
        // 1. the OS. Not audited: the refusal is the server's, whoever calls.
        if self.cfg.os != "linux" {
            return Response::err(format!(
                "settle refused: this server runs on {}; the clear grant's acts are admitted on Linux only (the grant can still be given and ended here)",
                self.cfg.os
            ));
        }
        // 2. the caller.
        let session = match self.name_settle_caller(peer, socket.as_ref()) {
            Ok(session) => session,
            Err(why) => {
                let caller = peer
                    .pid
                    .map_or("unknown".to_string(), |p| format!("pid:{p}"));
                return self.settle_refused(args, &caller, Refused::Text(why));
            }
        };
        match self.settle_for(args, &session) {
            Ok(answer) => Response::ok(serde_json::to_value(answer).unwrap_or(Value::Null)),
            Err(why) => self.settle_refused(args, &session, why),
        }
    }

    /// Audit a refusal and build its answer.
    fn settle_refused(&self, args: &Value, caller: &str, why: Refused) -> Response {
        let bucket = self.settle_bucket(args);
        let content = json!({ "verb": VERB_SETTLE, "request": bucket }).to_string();
        self.refuse_coalesced(
            KIND,
            bucket,
            content,
            caller,
            &format!("settle refused: {}", why.text()),
        );
        match why {
            Refused::Text(t) => Response::err(format!("settle refused: {t}")),
            Refused::Moved(t) => refusal("subject_moved", format!("settle refused: {t}")),
        }
    }

    /// The bucket of a refusal: the request ID when that request exists in a run the controller
    /// holds, else `other`, so a made-up ID opens no new window.
    fn settle_bucket(&self, args: &Value) -> String {
        let Some(id) = args.get("request_id").and_then(Value::as_str) else {
            return "other".to_string();
        };
        match self.store.request(id) {
            Ok(Some(r)) if self.find(Some(&r.run_id)).is_ok() => id.to_string(),
            _ => "other".to_string(),
        }
    }

    /// Steps 3 to 12, for the session that the caller check named.
    fn settle_for(&self, args: &Value, session: &str) -> Result<SettleAnswer, Refused> {
        // 3. the arguments.
        let args: SettleArgs = serde_json::from_value(args.clone())
            .map_err(|e| text(format!("the arguments are not readable: {e}")))?;
        let act = Act::parse(&args.act).ok_or_else(|| {
            text(format!(
                "the act {:?} is none of {}",
                echo(&args.act),
                Act::ALL.map(Act::as_str).join(", ")
            ))
        })?;
        match (act, &args.to_stage) {
            (Act::Redispatch, None) => {
                return Err(text(
                    "redispatch needs --to <stage>: a redispatch with no --to would be a clear",
                ));
            }
            (other, Some(_)) if other != Act::Redispatch => {
                return Err(text(format!(
                    "--to belongs to redispatch only: {} resumes at the stage the unit parked in",
                    other.as_str()
                )));
            }
            _ => {}
        }
        let n = args.reason.chars().count();
        if n == 0 || n > REASON_MAX_CHARS {
            return Err(text(format!(
                "the reason is 1 to {REASON_MAX_CHARS} characters, not {n}"
            )));
        }
        if args.reason.chars().any(char::is_control) {
            return Err(text("the reason is one line with no control character"));
        }
        if args.reason.trim().is_empty() {
            return Err(text("the reason holds no text"));
        }

        // 4. who.
        let row = self
            .store
            .chat_session(session)
            .map_err(|e| text(e.to_string()))?
            .ok_or_else(|| text(format!("session {session} is not in the store")))?;
        if row.ended_at.is_some() {
            return Err(text(format!("session {session} has ended")));
        }
        if row.agent_id != ORCHESTRATOR_AGENT {
            return Err(text(format!(
                "session {session} runs agent {}, and only a session of the {ORCHESTRATOR_AGENT} agent settles",
                echo(&row.agent_id)
            )));
        }
        let request = self
            .store
            .request(&args.request_id)
            .map_err(|e| text(e.to_string()))?
            .ok_or_else(|| text(format!("request {} is unknown", echo(&args.request_id))))?;
        let handle = self
            .find(Some(&request.run_id))
            .map_err(|why| text(why.to_string()))?;
        let run = self
            .store
            .run(&request.run_id)
            .map_err(|e| text(e.to_string()))?
            .ok_or_else(|| text(format!("run {} is unknown", request.run_id)))?;
        let Some(project_id) = run.project_id else {
            return Err(text(format!(
                "run {} has no project (it was armed from the CLI), so no grant covers its requests",
                request.run_id
            )));
        };
        if project_id != row.project_id {
            return Err(text(format!(
                "request {} belongs to project {project_id}, and session {session} belongs to project {}",
                request.id, row.project_id
            )));
        }

        // 5. no read of the admission here: a retry reaches `Engine::settle`, whose replay lookup is
        // its first step.

        // 6. the grant.
        let rows = self.store.clear_grants().map_err(|e| text(e.to_string()))?;
        let project = self
            .store
            .project(&project_id)
            .map_err(|e| text(e.to_string()))?
            .ok_or_else(|| text(format!("project {project_id} is not in the store")))?;
        let Some(grant) = live_grant(&rows, &project) else {
            let removed = if project.removed_at.is_some() {
                " (the project is removed)"
            } else {
                ""
            };
            return Err(text(format!(
                "project {project_id} holds no live clear grant{removed}"
            )));
        };
        let generation = grant.generation;
        let agent = grant.agent_id.clone();
        self.cfg.settle_faults.run_after_grant();

        // 7. the act list.
        let (allowed, from) = self
            .resolved_clear_acts(&project_id, ORCHESTRATOR_AGENT)
            .map_err(text)?;
        if !allowed.iter().any(|a| a == act.as_str()) {
            return Err(text(format!(
                "the act {} is not in {AUTHORITY_CLEAR_ACTS} (from {from})",
                act.as_str()
            )));
        }

        // 8. `close_stale`: the one act this server does not build.
        let cause = decision_of(&request.body).map(|d| d.cause);
        if act == Act::CloseStale {
            return Err(text(
                "close_stale has no object: a revision replaces its request in the same row, so no stale request exists",
            ));
        }

        // 10. the reason, redacted once on the raw value, then bounded.
        let reason: String = redact(&args.reason)
            .chars()
            .take(REASON_MAX_CHARS)
            .collect();

        // `rerun_check` counts the re-runs of one head against this limit, read fresh.
        let rerun_limit = if act == Act::RerunCheck {
            Some(
                self.resolved_rerun_checks_per_head(&project_id, ORCHESTRATOR_AGENT)
                    .map_err(text)?,
            )
        } else {
            None
        };

        // 9 and 11. the pins, then the engine.
        let ask = SettleAsk {
            request_id: request.id.clone(),
            revision: args.revision,
            act,
            to_stage: args.to_stage.clone(),
            facts: SettleFacts {
                reason,
                session: session.to_string(),
                project: project_id.clone(),
                generation,
                agent,
                line: Value::Null,
                head_sha: None,
                rerun_limit,
            },
        };
        let forge = (
            handle.info.forge_repo.as_str(),
            handle.info.default_branch.as_str(),
        );
        let admission = handle.driver.settle(ask, forge).map_err(|e| match e {
            SettleError::Moved(why) => Refused::Moved(why),
            SettleError::Refused(why) => Refused::Text(why),
            SettleError::Conflict => text(format!(
                "revision {} of {} was settled with another act",
                args.revision, request.id
            )),
            SettleError::Engine(e) => text(e.to_string()),
        })?;

        // 12. the effect, for an act that has one. A replay runs nothing: the first call did, or the
        // start-up reconcile closes it.
        if !admission.replayed {
            let stored_cause = cause.as_deref().and_then(Cause::parse);
            if let Some(kind) = effect_of(act, stored_cause) {
                self.run_settle_effect(
                    &handle,
                    &request.run_id,
                    request.unit_id.as_deref().unwrap_or_default(),
                    &request.id,
                    args.revision,
                    kind,
                );
            }
        }

        // 13. the answer: the stored record, so a replay shows the first reason.
        let stored = self
            .store
            .request(&request.id)
            .map_err(|e| text(e.to_string()))?
            .and_then(|r| r.answer)
            .and_then(|a| settled_of(&a))
            .ok_or_else(|| {
                text("the settle was admitted, but its request holds no settle record")
            })?;
        Ok(SettleAnswer {
            schema_version: SCHEMA_VERSION,
            request_id: request.id,
            revision: args.revision,
            replayed: admission.replayed,
            settled: stored,
        })
    }

    /// Run the effect of a settle that committed, and store its result. A re-attach marks the unit
    /// on the host service for the call, so its rebind is the grant's re-attach and not tier 1. An
    /// error of the effect call is one line of `controller.ledger`; the start-up reconcile then
    /// closes the settle with the result `unknown`.
    fn run_settle_effect(
        &self,
        handle: &super::RunHandle,
        run_id: &str,
        unit_id: &str,
        request_id: &str,
        revision: u32,
        kind: EffectKind,
    ) {
        let name = format!("{request_id}@{revision}");
        let faults = &self.cfg.settle_faults;
        faults.run_after_commit();
        if faults.skip_effect.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let reattach = kind == EffectKind::Reattach;
        if reattach {
            self.hosts.mark_reattach(run_id, unit_id, &name);
        }
        let done = handle.driver.settle_effect(request_id, revision);
        if reattach {
            self.hosts.clear_reattach(run_id, unit_id);
        }
        if let Err(e) = done {
            let why = one_line_reason(&e.to_string());
            let id = format!("settle-effect-{request_id}-{revision}-{}", unique_suffix());
            let line = format!("settle_effect {name} error: {why} command={id}");
            let _ = self.run_command(
                "settle_effect",
                &id,
                json!({"request": request_id, "revision": revision}).to_string(),
                Plan::Admit {
                    effects: Vec::new(),
                    subject: request_id.to_string(),
                    line,
                },
            );
        }
    }

    /// Store a run's armed launch count, once, in an admission under `@controller` (`arm`, after the
    /// run row exists and before it answers). The command ID is `run-launch-bound-<run>`.
    pub(super) fn set_run_launch_bound(&self, run_id: &str, bound: u64) -> Result<(), String> {
        let command_id = format!("run-launch-bound-{run_id}");
        let content = json!({ "run": run_id, "most_launches": bound }).to_string();
        let line = format!(
            "{RUN_LAUNCH_BOUND} {} most_launches={bound} command={command_id}",
            field(run_id)
        );
        self.run_command(
            RUN_LAUNCH_BOUND,
            &command_id,
            content,
            Plan::Admit {
                effects: vec![pf_engine::port::store::Effect::SetRunLaunchBound {
                    run_id: run_id.to_string(),
                    bound,
                }],
                subject: run_id.to_string(),
                line,
            },
        )
        .map(|_| ())
        .map_err(|why| format!("the run's launch count cannot be stored: {why}"))
    }
}

/// A suffix that no two calls share, for the ID of a controller note.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{}", N.fetch_add(1, Ordering::SeqCst))
}
