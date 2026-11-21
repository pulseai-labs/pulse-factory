//! The decision block of a request (`docs/contracts/requests.md`, "The decision block").
//!
//! A park names its cause in code. The engine writes the block from the cause and its own facts
//! when it decides the park command, and stores it under the body key `decision`. The command
//! carries inputs only, so a replay of the command ID returns the stored request and derives
//! nothing. The block is data for a reader: it says why the work stopped, what each possible
//! answer does, and what a fresh session needs to pick the request up. Nothing here acts on an
//! option, and no summary holds a seat's words: a seat's text stays in `stop_text` and `gaps`.

use serde_json::{Map, Value, json};

use crate::admission::RunView;
use crate::port::store::{Cause, NodeRow, RequestRow};
use crate::workflow::Workflow;

/// The body key of the decision block.
pub const KEY_DECISION: &str = "decision";
/// The key of the options a stop file named, in a stop request's facts.
pub const KEY_STOP_OPTIONS: &str = "stop_options";
/// The key that says why a stop file's options could not be read.
pub const KEY_STOP_OPTIONS_ERROR: &str = "stop_options_error";
/// The key of the fact that a text answer reaches a seat, in a park's facts.
pub const KEY_TAKES_TEXT: &str = "takes_text";
/// The key of the stage an outcome cap sends the unit to, in the cap park's facts.
pub const KEY_CAP_TO: &str = "cap_to";
/// The key of an outcome cap's number, in the cap park's facts.
pub const KEY_CAP_MAX: &str = "cap_max";

/// The version of the block's shape.
pub const DECISION_VERSION: u64 = 1;
/// The longest summary, in characters.
pub const SUMMARY_MAX_CHARS: usize = 300;
/// The most options a block holds.
pub const OPTIONS_MAX: usize = 4;

/// The characters of a head SHA a summary names.
const SHA_CHARS: usize = 12;

/// What the block is built from. The facts are the park's `detail`, or a revised request's body.
pub(crate) struct Inputs<'a> {
    pub view: &'a RunView,
    pub workflow: &'a Workflow,
    pub unit: &'a NodeRow,
    pub cause: Cause,
    /// The stage the request names: the stage the unit parked in, or `merge`.
    pub stage: &'a str,
    pub facts: &'a Map<String, Value>,
}

fn text_of<'a>(facts: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    facts
        .get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
}

/// The block for `inputs`, as the JSON object stored under [`KEY_DECISION`].
pub(crate) fn build(inputs: &Inputs) -> Value {
    let Inputs {
        view,
        workflow,
        unit,
        cause,
        stage,
        facts,
    } = inputs;
    let stage_nodes: Vec<&NodeRow> = view
        .stages_of(&unit.id)
        .into_iter()
        .filter(|n| n.detail == *stage)
        .collect();
    let attempt = stage_nodes.len();
    let detail = view.unit_detail(unit).ok();
    let node_role = stage_nodes.last().and_then(|n| n.role.clone());
    let def_role = detail
        .as_ref()
        .and_then(|d| workflow.track(&d.track))
        .and_then(|t| t.stages.iter().find(|s| s.name == *stage))
        .map(|s| s.role.clone());
    let role = node_role.clone().or(def_role).unwrap_or_default();

    // The IDs: a fact that does not exist is left out.
    let mut refs = Map::new();
    if let Some(project) = view.run.as_ref().and_then(|r| r.project_id.clone()) {
        refs.insert("project".to_string(), json!(project));
    }
    refs.insert("run".to_string(), json!(unit.run_id));
    refs.insert("unit".to_string(), json!(unit.id));
    if !stage.is_empty() {
        refs.insert("stage".to_string(), json!(stage));
    }
    let pr = text_of(facts, "pr").map(str::to_string).or_else(|| {
        detail
            .as_ref()
            .and_then(|d| d.pr.as_ref())
            .map(|p| format!("{}#{}", p.repo, p.number))
    });
    if let Some(pr) = &pr {
        refs.insert("pr".to_string(), json!(pr));
    }

    let seat_asked = matches!(cause, Cause::StopFile | Cause::ParkFile);
    let asked_by = if seat_asked {
        json!({ "kind": "seat", "role": role, "unit": unit.id })
    } else {
        json!({ "kind": "controller" })
    };
    let about = matches!(
        cause,
        Cause::Stalled | Cause::NoDoneSignal | Cause::SeatLost | Cause::HostUnreachable
    ) && node_role.is_some();

    let takes_text = facts
        .get(KEY_TAKES_TEXT)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (options, recommended) = options_of(inputs, takes_text);

    let mut block = Map::new();
    block.insert("v".to_string(), json!(DECISION_VERSION));
    block.insert("cause".to_string(), json!(cause.as_str()));
    block.insert(
        "summary".to_string(),
        json!(summary_of(inputs, pr.as_deref())),
    );
    block.insert("options".to_string(), Value::Array(options));
    if let Some(recommended) = recommended {
        block.insert("recommended".to_string(), recommended);
    }
    block.insert("refs".to_string(), Value::Object(refs));
    block.insert("attempt".to_string(), json!(attempt));
    block.insert("asked_by".to_string(), asked_by);
    if about {
        block.insert(
            "about".to_string(),
            json!({ "role": role, "unit": unit.id }),
        );
    }
    if let Some(why) = text_of(facts, KEY_STOP_OPTIONS_ERROR)
        && *cause == Cause::StopFile
    {
        block.insert("options_error".to_string(), json!(why));
    }
    block.insert("takes_text".to_string(), json!(takes_text));
    Value::Object(block)
}

/// One sentence the controller writes from the cause and its own facts. It names the unit, the
/// stage, the attempt, a cap's number or a PR, and never a seat's words.
fn summary_of(inputs: &Inputs, pr: Option<&str>) -> String {
    let unit = &inputs.unit.id;
    let stage = inputs.stage;
    let attempts = inputs
        .view
        .stages_of(unit)
        .into_iter()
        .filter(|n| n.detail == stage)
        .count();
    let pr_text = pr.map_or_else(|| "the PR".to_string(), |p| format!("the PR {p}"));
    let text = match inputs.cause {
        Cause::StopFile => format!("Unit {unit} stopped at stage {stage} and wrote a stop file."),
        Cause::ParkFile => {
            format!("Unit {unit} parked at stage {stage} because the seat wrote a park file.")
        }
        Cause::NoDoneSignal => format!(
            "The seat of unit {unit} ended its turn at stage {stage} with no done signal (attempt {attempts})."
        ),
        Cause::Stalled => format!(
            "The seat of unit {unit} stalled at stage {stage} (attempt {attempts}) after its one relaunch."
        ),
        Cause::LaunchRefused => format!(
            "The agent refused to launch stage {stage} of unit {unit} (attempt {attempts})."
        ),
        Cause::LaunchFailed => {
            format!("Launching stage {stage} of unit {unit} failed (attempt {attempts}).")
        }
        Cause::OutcomeCap => {
            let to = text_of(inputs.facts, KEY_CAP_TO).unwrap_or("another stage");
            match inputs.facts.get(KEY_CAP_MAX).and_then(Value::as_u64) {
                Some(max) => format!(
                    "Stage {stage} of unit {unit} sent the unit to {to} {max} times, which is its cap, so the unit parked."
                ),
                None => format!(
                    "Stage {stage} of unit {unit} reached its cap on the way to {to}, so the unit parked."
                ),
            }
        }
        Cause::HookFailed => format!("A command hook of stage {stage} of unit {unit} failed."),
        Cause::AnswerHookFailed => format!(
            "The answer hook of stage {stage} of unit {unit} failed, so the unit parked again."
        ),
        Cause::PinsChanged => format!(
            "A pinned copy changed after the run armed, so unit {unit} parked before stage {stage}."
        ),
        Cause::BasePushFailed => {
            format!("The push of the base branch before stage {stage} of unit {unit} failed.")
        }
        Cause::BaseMoved => {
            format!("The default branch moved during a push of unit {unit} at stage {stage}.")
        }
        Cause::RenderFailed => {
            format!("A path or a condition of stage {stage} of unit {unit} cannot render.")
        }
        Cause::DecisionsTooLarge => {
            format!("The decisions file of unit {unit} is over its size bound.")
        }
        Cause::NoPr => format!("Unit {unit} is ready to merge, but no PR is recorded for it."),
        Cause::MergeConflict => format!("{pr_text} of unit {unit} conflicts with its base."),
        Cause::MergeChecksRed => format!("The checks of {pr_text} of unit {unit} are red."),
        Cause::MergeRefused => {
            format!("The forge refused to merge {pr_text} of unit {unit}.")
        }
        Cause::MergeError => format!("The merge of {pr_text} of unit {unit} did not land."),
        Cause::SeatLost => format!(
            "The seat of unit {unit} is gone: its process ended at stage {stage} (attempt {attempts})."
        ),
        Cause::HostUnreachable => format!(
            "The host of the seat of unit {unit} did not answer at stage {stage} (attempt {attempts})."
        ),
        Cause::MergeWaitsAck => {
            let head = text_of(inputs.facts, "head_sha")
                .map(|h| h.chars().take(SHA_CHARS).collect::<String>());
            match head {
                Some(head) => format!(
                    "{pr_text} of unit {unit} is ready to merge. It waits for your ack of head {head}."
                ),
                None => {
                    format!("{pr_text} of unit {unit} is ready to merge. It waits for your ack.")
                }
            }
        }
    };
    text.chars().take(SUMMARY_MAX_CHARS).collect()
}

fn option(id: &str, label: &str, detail: &str, effect: &Value) -> Value {
    json!({ "id": id, "label": label, "detail": detail, "effect": effect })
}

/// The options of the cause, and the recommendation (`docs/contracts/requests.md`, the cause
/// table). One table, in engine words.
fn options_of(inputs: &Inputs, takes_text: bool) -> (Vec<Value>, Option<Value>) {
    let resume = json!({ "verb": "resume" });
    let resume_option = |detail: &str| option("resume", "Resume", detail, &resume);
    match inputs.cause {
        // The seat's own options, in file order. The text is all there is.
        Cause::StopFile => {
            let named = inputs
                .facts
                .get(KEY_STOP_OPTIONS)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut options = Vec::new();
            let mut recommended = None;
            for (i, o) in named.iter().take(OPTIONS_MAX).enumerate() {
                let Some(text) = o.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let id = format!("s{}", i + 1);
                options.push(option(
                    &id,
                    text,
                    "Answer the stop with this text.",
                    &json!({ "verb": "answer", "text": text }),
                ));
                if o.get("recommended").and_then(Value::as_bool) == Some(true)
                    && recommended.is_none()
                {
                    recommended = Some(json!({
                        "option": id,
                        "by": "seat",
                        "why": "The seat marked this option.",
                    }));
                }
            }
            (options, recommended)
        }
        // Only the workflow knows what clears its own file. With a hook, the typed answer is the
        // act; with none, the unit resumes once the file is gone.
        Cause::ParkFile if takes_text => (Vec::new(), None),
        Cause::ParkFile => (
            vec![resume_option(
                "Resume the unit at the stage it parked in. The park file must be gone first, or the unit parks again.",
            )],
            None,
        ),
        Cause::OutcomeCap => {
            let again_to = text_of(inputs.facts, KEY_CAP_TO);
            let go_on_at = inputs.stage;
            let again = match again_to {
                Some(to) => option(
                    "again",
                    &format!("Send it to {to} again"),
                    &format!(
                        "Resume the unit at stage {to}. A resume grants a new budget: the outcome counters start again."
                    ),
                    &json!({ "verb": "resume", "to": to }),
                ),
                None => option(
                    "again",
                    "Send it on again",
                    "Resume the unit. A resume grants a new budget: the outcome counters start again.",
                    &resume,
                ),
            };
            let go_on = option(
                "go_on",
                "Go on here",
                &format!(
                    "Resume the unit at stage {go_on_at}, where it parked. A resume grants a new budget: the outcome counters start again."
                ),
                &resume,
            );
            (vec![again, go_on], None)
        }
        Cause::MergeWaitsAck => (
            vec![option(
                "approve",
                "Approve the merge",
                "Ack the request: the PR merges with the head named here pinned.",
                &json!({ "verb": "ack" }),
            )],
            None,
        ),
        cause => {
            let detail = match cause {
                Cause::MergeConflict
                | Cause::MergeChecksRed
                | Cause::MergeRefused
                | Cause::MergeError
                | Cause::NoPr => "Resume the unit: the merge is tried again.",
                Cause::Stalled
                | Cause::SeatLost
                | Cause::HostUnreachable
                | Cause::LaunchRefused
                | Cause::LaunchFailed
                | Cause::NoDoneSignal => "Resume the unit: the stage launches again.",
                _ => "Resume the unit at the stage it parked in.",
            };
            let why = match cause {
                Cause::Stalled => Some("A stall is often transient: a new launch may go on."),
                Cause::SeatLost => Some("The seat's process is gone: a new launch replaces it."),
                Cause::HostUnreachable => {
                    Some("A host that does not answer may be back: a new launch may go on.")
                }
                Cause::MergeChecksRed => {
                    Some("A red check may be a flaky run: the merge is tried again.")
                }
                _ => None,
            };
            let recommended =
                why.map(|why| json!({ "option": "resume", "by": "controller", "why": why }));
            (vec![resume_option(detail)], recommended)
        }
    }
}

/// The block of a revised request, built at the new revision from the new facts. `None` when the
/// request holds no block, or holds one this binary cannot read: the revision then keeps what it
/// is given.
pub(crate) fn rebuilt(
    view: &RunView,
    workflow: &Workflow,
    request: &RequestRow,
    body: &Map<String, Value>,
) -> Option<Value> {
    let old: Value = serde_json::from_str(&request.body).ok()?;
    let cause = Cause::parse(old.get(KEY_DECISION)?.get("cause")?.as_str()?)?;
    let unit = view.node(request.unit_id.as_deref()?)?;
    Some(build(&Inputs {
        view,
        workflow,
        unit,
        cause,
        stage: &request.stage,
        facts: body,
    }))
}
