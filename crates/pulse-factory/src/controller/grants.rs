//! The clear grant: the verbs `grant_clear`, `grant_revoke` and `grant_list` (ADR-0004, ADR-0005,
//! ADR-0009, amended; `docs/contracts/cli.md`, `grant`).
//!
//! The clear grant is a stored row for each project and generation. Only the operator gives or ends
//! it. `grant_clear` and `grant_revoke` change who may act, so each passes the caller check that
//! `arm` passes (the dispatch in `mod.rs` runs it): a seat and a chat are refused. They read no
//! claim token and move no claim. Each is one controller command through `commands.rs`, under
//! `@controller`, with the caller's command ID and one ledger line.
//!
//! A revoke commits even when the controller's commands are stopped (`run_command` lets the kind
//! [`GRANT_REVOKE`] through, as it does a device revoke): the operator must be able to end the grant
//! in the state where nothing else can be recorded. `halt` revokes nothing. `project remove` writes
//! no revoke: a removed project's grant is not live for any reader, because every reader reads
//! through [`pf_engine::claim::live_grant`].
//!
//! `settle` acts under the grant (`r2.s3.w2`), and a grant that has just been given wakes the project's
//! Orchestrator for the requests below the line that waited (`r2.s3.w4`).

use pf_api::types::{ClearGrant, GrantChanged, GrantList, SCHEMA_VERSION};
use pf_engine::claim::{live_grant, open_grant};
use pf_engine::port::store::{
    AdmissionRow, ClearGrantRow, Effect, NewClearGrant, ORCHESTRATOR_AGENT, Store,
};
use pf_ipc::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Controller;
use super::commands::{GRANT_CLEAR, GRANT_REVOKE, Plan, check_command_id, field, now};

/// The `granted_by` and `revoked_by` of a grant that the operator's CLI gave or ended.
pub const BY_CLI: &str = "cli";

/// A project ID names a stored row, so it is short. A longer text is no ID.
const PROJECT_ID_MAX_CHARS: usize = 64;

/// The arguments of `grant_clear` and `grant_revoke`.
#[derive(Deserialize)]
struct GrantArgs {
    project_id: String,
    command_id: String,
}

/// The arguments of `grant_list`.
#[derive(Default, Deserialize)]
struct ListArgs {
    #[serde(default)]
    project_id: Option<String>,
}

/// The content of a `grant_clear` or a `grant_revoke` admission: the verb's arguments without the
/// command ID. The store compares these bytes, so the field order is fixed.
#[derive(Serialize)]
struct GrantContent<'a> {
    project_id: &'a str,
}

/// The wire form of one stored row. `live` is the caller's answer from [`live_grant`].
pub(super) fn grant_record(row: &ClearGrantRow, live: bool) -> ClearGrant {
    ClearGrant {
        project_id: row.project_id.clone(),
        agent_id: row.agent_id.clone(),
        generation: row.generation,
        granted_by: row.granted_by.clone(),
        granted_at: row.granted_at.clone(),
        revoked_by: row.revoked_by.clone(),
        revoked_at: row.revoked_at.clone(),
        live,
    }
}

/// The subject of a grant admission: the project and the generation the command wrote or ended, so a
/// replay names the same row as the first call did.
fn subject(project_id: &str, generation: u64) -> String {
    format!("{project_id}:{generation}")
}

fn parse_subject(subject: &str) -> Option<(&str, u64)> {
    let (project, generation) = subject.rsplit_once(':')?;
    Some((project, generation.parse().ok()?))
}

impl Controller {
    /// The live clear grants: for each live project, the one row with no revoke. A removed project's
    /// grant is never here.
    pub(super) fn live_grant_records(&self) -> Result<Vec<ClearGrant>, String> {
        let rows = self.store.clear_grants().map_err(|e| e.to_string())?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let projects = self.store.projects().map_err(|e| e.to_string())?;
        Ok(projects
            .iter()
            .filter_map(|p| live_grant(&rows, p))
            .map(|g| grant_record(g, true))
            .collect())
    }

    /// Parse the arguments of a verb that gives or ends a grant, and check what needs no store.
    fn grant_args(&self, args: &Value) -> Result<GrantArgs, Response> {
        let args: GrantArgs = serde_json::from_value(args.clone())
            .map_err(|e| Response::err(format!("the arguments are not readable: {e}")))?;
        check_command_id(&args.command_id).map_err(Response::err)?;
        check_project_id(&args.project_id).map_err(Response::err)?;
        Ok(args)
    }

    /// `grant_clear`: give a live project its clear grant at the next generation.
    pub(super) fn verb_grant_clear(&self, args: &Value) -> Response {
        let args = match self.grant_args(args) {
            Ok(a) => a,
            Err(response) => return response,
        };
        let id = args.project_id.as_str();
        let content = serde_json::to_string(&GrantContent { project_id: id }).unwrap_or_default();
        let plan = match self.plan_grant_clear(id, &args.command_id) {
            Ok(plan) => plan,
            Err(why) => return Response::err(why),
        };
        match self.run_command(GRANT_CLEAR, &args.command_id, content, plan) {
            Ok((admission, replayed)) => {
                let response = self.grant_answer(&admission, replayed);
                // A grant that has just been given wakes the project's Orchestrator for the
                // requests below the line that waited for it (`r2.s3.w4`, call 3). A replay gives
                // nothing new.
                if !replayed {
                    self.wake_check(id);
                }
                response
            }
            Err(why) => Response::err(why),
        }
    }

    fn plan_grant_clear(&self, id: &str, command_id: &str) -> Result<Plan, String> {
        let Some(project) = self.store.project(id).map_err(|e| e.to_string())? else {
            return Ok(Plan::Refuse(format!("project {id} is not in the store")));
        };
        if project.removed_at.is_some() {
            return Ok(Plan::Refuse(format!("project {id} is removed")));
        }
        let rows = self.store.clear_grants().map_err(|e| e.to_string())?;
        if let Some(live) = live_grant(&rows, &project) {
            return Ok(Plan::Refuse(format!(
                "project {id} already holds the clear grant (generation {})",
                live.generation
            )));
        }
        let next = rows
            .iter()
            .filter(|g| g.project_id == id)
            .map(|g| g.generation)
            .max()
            .unwrap_or(0)
            + 1;
        Ok(Plan::Admit {
            effects: vec![Effect::GrantClear(NewClearGrant {
                project_id: id.to_string(),
                generation: next,
                agent_id: ORCHESTRATOR_AGENT.to_string(),
                granted_by: BY_CLI.to_string(),
                granted_at: now(),
            })],
            subject: subject(id, next),
            line: format!(
                "grant_clear {} agent={ORCHESTRATOR_AGENT} generation={next} by={BY_CLI} command={command_id}",
                field(id)
            ),
        })
    }

    /// `grant_revoke`: end the project's grant. A removed project with an open row is admitted: its
    /// grant already reads as not live, and the revoke closes the row.
    pub(super) fn verb_grant_revoke(&self, args: &Value) -> Response {
        let args = match self.grant_args(args) {
            Ok(a) => a,
            Err(response) => return response,
        };
        let id = args.project_id.as_str();
        let content = serde_json::to_string(&GrantContent { project_id: id }).unwrap_or_default();
        let plan = match self.plan_grant_revoke(id, &args.command_id) {
            Ok(plan) => plan,
            Err(why) => return Response::err(why),
        };
        self.grant_command(GRANT_REVOKE, &args.command_id, content, plan)
    }

    fn plan_grant_revoke(&self, id: &str, command_id: &str) -> Result<Plan, String> {
        if self.store.project(id).map_err(|e| e.to_string())?.is_none() {
            return Ok(Plan::Refuse(format!("project {id} is not in the store")));
        }
        let rows = self.store.clear_grants().map_err(|e| e.to_string())?;
        let Some(open) = open_grant(&rows, id) else {
            return Ok(Plan::Refuse(format!("project {id} holds no clear grant")));
        };
        let generation = open.generation;
        Ok(Plan::Admit {
            effects: vec![Effect::RevokeClearGrant {
                project_id: id.to_string(),
                generation,
                revoked_by: BY_CLI.to_string(),
                revoked_at: now(),
            }],
            subject: subject(id, generation),
            line: format!(
                "grant_revoke {} generation={generation} by={BY_CLI} command={command_id}",
                field(id)
            ),
        })
    }

    fn grant_command(
        &self,
        kind: &'static str,
        command_id: &str,
        content: String,
        plan: Plan,
    ) -> Response {
        match self.run_command(kind, command_id, content, plan) {
            Ok((admission, replayed)) => self.grant_answer(&admission, replayed),
            Err(why) => Response::err(why),
        }
    }

    /// The answer for a stored admission: the row it wrote or ended.
    fn grant_answer(&self, admission: &AdmissionRow, replayed: bool) -> Response {
        let Some((project_id, generation)) = admission.unit_id.as_deref().and_then(parse_subject)
        else {
            return Response::err("the admission names no grant");
        };
        let rows = match self.store.clear_grants() {
            Ok(rows) => rows,
            Err(e) => return Response::err(e.to_string()),
        };
        let Some(row) = rows
            .iter()
            .find(|g| g.project_id == project_id && g.generation == generation)
        else {
            return Response::err(format!(
                "generation {generation} of the clear grant of {project_id} is not in the store"
            ));
        };
        let live = match self.store.project(project_id) {
            Ok(Some(project)) => {
                live_grant(&rows, &project).is_some_and(|g| g.generation == generation)
            }
            Ok(None) => false,
            Err(e) => return Response::err(e.to_string()),
        };
        Response::ok(
            serde_json::to_value(GrantChanged {
                schema_version: SCHEMA_VERSION,
                replayed,
                grant: grant_record(row, live),
            })
            .unwrap_or(Value::Null),
        )
    }

    /// `grant_list`: every row, by project, and in a project newest generation first. It is a read,
    /// open to every caller as `requests` is: no admission, no ledger line, no store write.
    pub(super) fn verb_grant_list(&self, args: &Value) -> Response {
        let args: ListArgs = if args.is_null() {
            ListArgs::default()
        } else {
            match serde_json::from_value(args.clone()) {
                Ok(a) => a,
                Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
            }
        };
        if let Some(id) = &args.project_id
            && let Err(why) = check_project_id(id)
        {
            return Response::err(why);
        }
        let rows = match self.store.clear_grants() {
            Ok(rows) => rows,
            Err(e) => return Response::err(e.to_string()),
        };
        let mut grants = Vec::new();
        let mut at = 0;
        while at < rows.len() {
            // The rows of one project are together, oldest generation first.
            let end = rows[at..]
                .iter()
                .position(|g| g.project_id != rows[at].project_id)
                .map_or(rows.len(), |n| at + n);
            let group = &rows[at..end];
            at = end;
            let project_id = group[0].project_id.as_str();
            if args
                .project_id
                .as_deref()
                .is_some_and(|id| id != project_id)
            {
                continue;
            }
            let project = match self.store.project(project_id) {
                Ok(p) => p,
                Err(e) => return Response::err(e.to_string()),
            };
            let live = project
                .as_ref()
                .and_then(|p| live_grant(&rows, p))
                .map(|g| g.generation);
            grants.extend(
                group
                    .iter()
                    .rev()
                    .map(|g| grant_record(g, live == Some(g.generation))),
            );
        }
        Response::ok(
            serde_json::to_value(GrantList {
                schema_version: SCHEMA_VERSION,
                grants,
            })
            .unwrap_or(Value::Null),
        )
    }
}

fn check_project_id(id: &str) -> Result<(), String> {
    let n = id.chars().count();
    if n == 0 || n > PROJECT_ID_MAX_CHARS {
        return Err(format!(
            "a project ID is 1 to {PROJECT_ID_MAX_CHARS} characters"
        ));
    }
    Ok(())
}
