// What a Needs you card reads from a request's decision block (r2.s11.w5, Approach 1 to 7). Every function
// here is pure: the card draws what it returns. The controller wrote the block (`summary_of`,
// `options_of`, `declared_options`); the app never builds or rewrites a summary and never derives a fact from
// the reason text. A string of the block, the body or the answer is data and renders as text only (G2).
import type { AskedBy, Decision, DecisionOption, RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import type { IconName } from "../ui";

// ---- the kind label (12c's legend) ----

export type LabelTone = "amber" | "blue" | "red" | "violet" | "mint";

export interface KindLabel {
  /** The text, in upper case as the frames draw it. */
  text: string;
  icon: IconName;
  tone: LabelTone;
}

const KIND_LABELS: Record<string, KindLabel> = {
  stop: { text: "QUESTION", icon: "chat", tone: "amber" },
  gaps: { text: "GAPS", icon: "layers", tone: "blue" },
  red_gate: { text: "RED GATE", icon: "alert", tone: "red" },
  park: { text: "PARKED", icon: "pause", tone: "violet" },
  stall: { text: "STALLED", icon: "clock", tone: "violet" },
  seat_lost: { text: "SEAT LOST", icon: "bot", tone: "red" },
  merge_failed: { text: "MERGE FAILED", icon: "git-branch", tone: "red" },
  merge_ack: { text: "MERGE APPROVAL", icon: "git-branch", tone: "mint" },
};

/** One label for each request kind. Another kind shows its stored text in upper case, with the PARKED look. */
export function kindLabel(kind: string): KindLabel {
  return KIND_LABELS[kind] ?? { text: kind.toUpperCase(), icon: "pause", tone: "violet" };
}

// ---- names ----

/** The stored role made readable: `_` and `-` become spaces, and the first letter is a capital. */
export function readable(role: string): string {
  const spaced = role.replace(/[_-]/g, " ");
  return spaced === "" ? spaced : spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/** `<Role> <unit>` of the seat that asked, or `null` when the controller asked. */
export function seatName(asked: AskedBy | undefined): string | null {
  return asked !== undefined && asked.kind === "seat" ? `${readable(asked.role)} ${asked.unit}` : null;
}

/** Who wrote an option, or who recommends one: the stored word, with the default of a block that predates `by`. */
function writerOf(by: string | undefined, decision: Decision): "seat" | "workflow" | "controller" {
  if (by === "seat" || by === "workflow" || by === "controller") return by;
  return decision.cause === "stop_file" ? "seat" : "controller";
}

/** `from <Role> <unit>`, `from the workflow` or `from the controller`: the end of an option's line. */
export function optionWriter(option: DecisionOption, decision: Decision): string {
  const by = writerOf(option.by, decision);
  if (by === "workflow") return "from the workflow";
  if (by === "controller") return "from the controller";
  const seat = seatName(decision.asked_by);
  return seat === null ? "from the seat" : `from ${seat}`;
}

/** The source of a recommendation: `<Role> <unit>`, `the workflow` or `the controller`. */
export function recommendedSource(by: string, decision: Decision): string {
  const who = writerOf(by, decision);
  if (who === "workflow") return "the workflow";
  if (who === "controller") return "the controller";
  return seatName(decision.asked_by) ?? "the seat";
}

/** `PR #9` from `<repo>#<n>`, else the stored text as it is. */
export function prChip(pr: string): string {
  const match = /#(\d+)$/.exec(pr);
  return match === null ? pr : `PR #${match[1]}`;
}

// ---- the head, the facts and the chips ----

export interface IdChip {
  label: string;
  value: string;
  /** The stored text, as the chip's `title`. */
  title?: string;
}

/** One chip for each ID that `refs` holds, in order: project, run, unit, stage, PR. */
export function chipsOf(decision: Decision, projectName: string | null): IdChip[] {
  const { refs } = decision;
  const chips: IdChip[] = [];
  if (refs.project !== undefined) chips.push({ label: "project", value: projectName ?? refs.project });
  chips.push({ label: "run", value: refs.run });
  if (refs.unit !== undefined) chips.push({ label: "unit", value: refs.unit });
  if (refs.stage !== undefined) chips.push({ label: "stage", value: refs.stage });
  if (refs.pr !== undefined) {
    const value = prChip(refs.pr);
    chips.push({ label: value === refs.pr ? "PR" : "", value, title: refs.pr });
  }
  return chips;
}

export interface Names {
  /** The run's spine name from `runList`, else `null`. */
  spine: string | null;
  /** The name of the project `refs.project` names, else `null`. */
  project: string | null;
}

/** `<spine> · <project>`: the run's name, then the project's, with the run ID where no project exists. */
export function runPlace(request: RequestSummary, names: Names): { name: string; where: string | null } {
  if (names.spine !== null) return { name: names.spine, where: names.project ?? request.run };
  return { name: request.run, where: names.project };
}

export function askedByLine(decision: Decision, requestId: string): string | null {
  const asked = decision.asked_by;
  if (asked.kind !== "seat") return null;
  return `${readable(asked.role)} · ${asked.unit} · ${requestId}`;
}

/** The facts of a block card, in order. A row whose value does not exist is left out. */
export function factRows(request: RequestSummary, decision: Decision, names: Names): Array<[label: string, value: string]> {
  const rows: Array<[string, string]> = [];
  const attempt = decision.attempt > 0 ? String(decision.attempt) : null;
  const stage = decision.refs.stage ?? request.stage;
  const asked = askedByLine(decision, request.id);
  if (decision.cause === "stop_file") {
    // N3: a question has no Cause row.
    if (asked !== null) rows.push(["Asked by", asked]);
    if (attempt !== null) rows.push(["Attempt", attempt]);
    const place = runPlace(request, names);
    rows.push(["Run", place.where === null ? place.name : `${place.name} · ${place.where}`]);
    return rows;
  }
  rows.push(["Cause", decision.cause]);
  if (decision.cause === "park_file") {
    if (asked !== null) rows.push(["Asked by", asked]);
    if (attempt !== null) rows.push(["Attempt", attempt]);
    if (stage !== "") rows.push(["Stage", stage]);
    return rows;
  }
  if (attempt !== null) rows.push(["Attempt", attempt]);
  rows.push(["Parked by", "the controller"]);
  if (decision.about !== undefined) rows.push(["About", `${readable(decision.about.role)} · ${decision.about.unit}`]);
  if (stage !== "") rows.push(["Stage", stage]);
  return rows;
}

/** The facts folded into one line, as the dock draws them (12d). */
export function foldedFacts(rows: Array<[string, string]>): string {
  return rows.map(([label, value]) => `${label} ${value}`).join(" · ");
}

// ---- the question ----

/**
 * The stop text without its trailing options block: it ends before the last line that reads `Options:`, the
 * line that opens the block (`docs/contracts/requests.md`, "A stop's options"). A text with no such line stays whole.
 */
export function cutOptionsBlock(text: string): string {
  const lines = text.split("\n");
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    if ((lines[i] ?? "").trim() === "Options:") return lines.slice(0, i).join("\n").trimEnd();
  }
  return text;
}

/** The seat's options are in the block: the block holds one written by the seat, and its options could be read. */
export function holdsSeatOptions(decision: Decision): boolean {
  return decision.options_error === undefined && decision.options.some((o) => writerOf(o.by, decision) === "seat");
}

/** The stop text as the card shows it (Approach 2): cut before the options block when the buttons draw them. */
export function shownStopText(text: string, decision: Decision): string {
  return holdsSeatOptions(decision) && decision.takes_text ? cutOptionsBlock(text) : text;
}

// ---- the options (Approach 3 and 4) ----

export interface EffectLine {
  before: string;
  /** The stage, drawn as a chip. */
  chip: string | null;
  after: string;
}

/** What an option does, one line, written by the app from the option's effect and the cause. */
export function effectLine(option: DecisionOption, request: RequestSummary, decision: Decision): EffectLine {
  const effect = option.effect;
  const unit = request.unit ?? decision.refs.unit ?? request.run;
  if (effect.verb === "answer") {
    return effect.to === undefined
      ? { before: "Sends an answer", chip: null, after: "" }
      : { before: "Answers, then resumes at ", chip: effect.to, after: "" };
  }
  if (effect.verb === "ack") return { before: "Approves the merge", chip: null, after: "" };
  const stage = effect.to ?? request.stage;
  if (decision.cause === "outcome_cap") {
    return { before: `Resumes ${unit} at `, chip: stage, after: "; the outcome counters start again" };
  }
  if (decision.cause === "no_done_signal" || decision.cause === "launch_failed" || decision.cause === "launch_refused") {
    return { before: `Resumes ${unit} at `, chip: stage, after: "; the stage launches again" };
  }
  if (request.stage === "merge") return { before: "Tries the merge again", chip: null, after: "" };
  return { before: `Resumes ${unit} at `, chip: stage, after: "" };
}

/** The plain text of an effect line, for a description. */
export function effectText(line: EffectLine): string {
  return `${line.before}${line.chip ?? ""}${line.after}`;
}

/** The title of an option's button: a workflow's answer text first (`Sends: <text>`), then the stored detail. */
export function optionTitle(option: DecisionOption): string {
  const parts: string[] = [];
  if (option.by === "workflow" && option.effect.verb === "answer") parts.push(`Sends: ${option.effect.text}`);
  if (option.detail !== "") parts.push(option.detail);
  return parts.join("\n");
}

/**
 * The options a card may offer: only an act the server would take (Approach 4). A `resume` option needs `clear`
 * in `verbs`. An `answer` option needs `takes_text` and `answer` in `verbs`. A merge approval's `ack` option is
 * the merge card's own button, so it is never drawn as an option.
 */
export function offeredOptions(request: RequestSummary, decision: Decision): DecisionOption[] {
  if (request.kind === "merge_ack" || decision.options_error !== undefined) return [];
  return decision.options.filter((o) => {
    if (o.effect.verb === "resume") return request.verbs.includes("clear");
    if (o.effect.verb === "answer") return decision.takes_text && request.verbs.includes("answer");
    return false;
  });
}

/** A `stop_file` request of a unit that takes no text keeps one act: today's `clear`, with no option and no stage. */
export function stopTakesNoText(request: RequestSummary, decision: Decision): boolean {
  return (
    decision.cause === "stop_file" && request.unit !== null && !decision.takes_text && request.verbs.includes("clear")
  );
}

/** The one option that is recommended, when the block names one the card draws. A merge approval never shows it. */
export function recommendedOption(request: RequestSummary, decision: Decision, shown: DecisionOption[]): string | null {
  if (request.kind === "merge_ack" || decision.recommended === undefined) return null;
  const id = decision.recommended.option;
  return shown.some((o) => o.id === id) ? id : null;
}

/** The warning of a seat's park file that offers a `resume` option (above its options). */
export function parkFileWarning(request: RequestSummary, decision: Decision, shown: DecisionOption[]): string | null {
  if (decision.cause !== "park_file" || !shown.some((o) => o.effect.verb === "resume")) return null;
  const unit = request.unit ?? decision.refs.unit ?? request.run;
  return `Remove the park file first. If it is still there when ${unit} resumes, the unit parks again.`;
}

export const OPTIONS_ERROR_LINE =
  "The seat wrote options that couldn’t be read, so none are shown. Answer in your own words.";

export function stopNoTextWarning(request: RequestSummary, decision: Decision): string {
  const unit = request.unit ?? decision.refs.unit ?? request.run;
  return `No answer can reach the seat: this stage declares no answer hook. Remove the stop file first. If it is still there when ${unit} resumes, the unit stops again.`;
}

// ---- the text field (Approach 5) ----

export type TextForm =
  /** A seat asked: the text is a typed answer to its file, with Send. */
  | { form: "typed"; label: string; placeholder: string }
  /** The controller asked: the text is a note for the next seat, with no Send. The card words the field. */
  | { form: "note" };

/** The text field a card shows, or `null` (Q5: only when `takes_text` is true). */
export function textFormOf(request: RequestSummary, decision: Decision, shown: DecisionOption[]): TextForm | null {
  if (!decision.takes_text) return null;
  if (decision.asked_by.kind === "seat") {
    if (!request.verbs.includes("answer")) return null;
    return request.kind === "gaps"
      ? { form: "typed", label: "Your answer", placeholder: "Answer each gap. The planner gets your text as written." }
      : { form: "typed", label: "Another answer", placeholder: "Type your own answer. The seat gets it word for word." };
  }
  // A note rides the pressed `resume` option as a typed answer: with no such option, no press could send it.
  if (!request.verbs.includes("answer") || !shown.some((o) => o.effect.verb === "resume")) return null;
  return { form: "note" };
}

// ---- the answered row (Approach 7) ----

/** The unit a request is about, for the words of a resume. */
export function unitOf(request: RequestSummary, decision: Decision | undefined): string {
  return request.unit ?? decision?.refs.unit ?? request.run;
}

/** What a stored answer did, in the words of an answered row. `null` when the answer holds nothing to say. */
export function storedAnswerWords(
  request: RequestSummary,
  detail: RequestDetail | undefined,
  decision: Decision | undefined,
): string | null {
  const answer = detail?.answer;
  if (typeof answer !== "object" || answer === null) return null;
  const a = answer as Record<string, unknown>;
  const unit = unitOf(request, decision);
  const stage = typeof a.to_stage === "string" && a.to_stage !== "" ? a.to_stage : request.stage;
  const option = typeof a.option === "object" && a.option !== null ? (a.option as Record<string, unknown>) : null;
  const optionId = option !== null && typeof option.id === "string" ? option.id : null;
  if (a.verb === "clear") return `Resumed ${unit} at ${stage}`;
  if (a.verb === "answer" && typeof a.text === "string") {
    if (optionId !== null) {
      const held = decision?.options.find((o) => o.id === optionId);
      return `Pressed ${held?.label ?? a.text}`;
    }
    if (decision !== undefined && decision.asked_by.kind !== "seat") {
      return `Resumed ${unit} at ${stage} · note: ${a.text}`;
    }
    return `Typed ${a.text}`;
  }
  return null;
}
