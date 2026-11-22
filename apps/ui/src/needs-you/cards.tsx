// The Needs you cards (11d, r1.s5.w4; 12a to 12e, r2.s11.w5), built from the shared `ui/` components and
// `NotBuilt`. A card is a request. When its body holds a decision block the card is a block card (`BlockCard`):
// the kind label, the controller's summary, who asked, the attempt, the IDs, one button for each option and the
// text field of the form the request takes. A request with no block (opened before the upgrade, a run-level
// request) keeps today's cards: the spine merge, a run question (`stop` or `gaps`), a parked item. A fourth card
// is a chat that waits for its answer. Each act is one press (`useConfirmedAct`); the page and the dock draw
// the same cards, the dock in `compact` form (328 px: the facts on one line).
import type {
  ChatSessionRecord,
  Decision,
  DecisionOption,
  ProjectRecord,
  RequestDetail,
  RequestSummary,
} from "@pulse-factory/api-client";
import { useId, useState, type ReactNode } from "react";
import { branchText, lastSegment, parseTime } from "../components/chat-format";
import { clock } from "../components/format";
import { useNavigation } from "../shell/navigation";
import { Avatar, Button, Chip, Icon, NotBuilt, StatePill, agentLook } from "../ui";
import { controlOf, type Act, type ActLine, type Done } from "./useConfirmedAct";
import { age, cardKind, fieldOf, firstLine, prLabel, questionOf, shortSha, timeAge } from "./format";
import {
  OPTIONS_ERROR_LINE,
  chipsOf,
  effectLine,
  effectText,
  factRows,
  foldedFacts,
  kindLabel,
  offeredOptions,
  optionTitle,
  optionWriter,
  parkFileWarning,
  recommendedOption,
  recommendedSource,
  runPlace,
  seatName,
  shownStopText,
  stopNoTextWarning,
  stopTakesNoText,
  textFormOf,
  unitOf,
  type Names,
  type TextForm,
} from "./decision";
import { hold } from "./held";

/** Everything a request card needs besides the request itself. */
export interface CardEnv {
  detail: RequestDetail | undefined;
  /** The line of a failed `show` for this request. */
  bodyError: string | null;
  /** The spine's name from the run list, when it is known. */
  spine: string | null;
  line: ActLine | undefined;
  done: Done | undefined;
  notice: string | undefined;
  inFlight: boolean;
  compact: boolean;
  /** The projects the connection holds: a block card names its project. `null` before the Projects page read them. */
  projects: ProjectRecord[] | null;
  /** The server's name as the title bar shows it, for the states of a press. */
  serverName: string;
  onAct(act: Act): void;
  onRetry(): void;
  onRefresh(): void;
}

function Who(props: { name: string; where: string | null }) {
  return (
    <>
      <span className="ny-name">{props.name}</span>
      {props.where !== null ? <span className="ny-where">{`· ${props.where}`}</span> : null}
    </>
  );
}

function When(props: { at: string; compact: boolean }) {
  return <span className={props.compact ? "ny-age dock-age" : "ny-age"}>{timeAge(props.at)}</span>;
}

/** The compact card's way to the full page (G8: the dock's Open button stays). */
function OpenPage() {
  const { openPage } = useNavigation();
  return (
    <Button size="sm" variant="ghost" onClick={() => openPage("needs_you")}>
      Open
    </Button>
  );
}

function Facts(props: { rows: Array<[label: string, value: ReactNode]> }) {
  return (
    <dl className="ny-facts">
      {props.rows.map(([label, value]) => (
        <div className="ny-fact" key={label}>
          <dt>{label}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** What the press did, or why it did not: the mint row of 11d, the notice of a replay, the line of a refusal. */
function Outcome(props: { env: CardEnv; block?: boolean }) {
  const { line, done, notice, onRetry } = props.env;
  const retry = line?.phase === "ask_unreachable" || line?.phase === "spend_unreachable";
  return (
    <>
      {!props.block && line?.message ? (
        <p className="ny-act-error" role="alert">
          <span>{line.message}</span>
          {retry ? (
            <Button size="sm" onClick={onRetry}>
              Retry
            </Button>
          ) : null}
        </p>
      ) : null}
      {notice ? <p className="ny-notice">{notice}</p> : null}
      {done ? (
        <div className="ny-done">
          <span className="ny-done-icon">
            <Icon name="check" size={14} />
          </span>
          <span>Your answer</span>
          <span className="ny-done-what">{done.what}</span>
          {done.detail ? <span className="ny-done-detail">{done.detail}</span> : null}
          <span className="ny-spacer" />
          <span className="ny-done-who">{`${done.device} · now`}</span>
        </div>
      ) : null}
    </>
  );
}

function BodyError(props: { env: CardEnv }) {
  const { bodyError, onRefresh } = props.env;
  if (bodyError === null) return null;
  return (
    <p className="ny-act-error" role="alert">
      <span>{bodyError}</span>
      <Button size="sm" onClick={onRefresh}>
        Refresh
      </Button>
    </p>
  );
}

function Quiet(props: { request: RequestSummary; env: CardEnv }) {
  if (!props.env.compact) return null;
  const { request } = props;
  const line = [request.unit, firstLine(request.reason)].filter((s): s is string => s !== null && s !== "").join(" · ");
  return line === "" ? null : <p className="ny-quiet">{line}</p>;
}

// ---- the spine merge ----

function MergeCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  const { detail, spine, compact } = env;
  const pr = fieldOf("pr", request, detail);
  const base = fieldOf("base", request, detail);
  const sha = fieldOf("head_sha", request, detail);
  const branch = fieldOf("head_branch", request, detail);
  const from = branch ?? spine ?? request.run;
  const closed = env.done !== undefined;
  return (
    <>
      <div className="ny-main">
        <div className="ny-head">
          <span className="ny-icon ny-icon-mint">
            <Icon name="git-branch" size={13} />
          </span>
          <span className="ny-eyebrow">Spine merge</span>
          {spine !== null ? <span className="ny-where">{`· ${spine}`}</span> : null}
          <span className="ny-spacer" />
          {pr !== null ? (
            <span className="ny-pill" data-testid="merge-pr">
              {prLabel(pr)}
            </span>
          ) : null}
          <When at={request.opened_at} compact={compact} />
          {compact ? <OpenPage /> : null}
        </div>
        <p className="ny-question ny-question-merge">{base !== null ? `Merge ${from} into ${base}?` : `Merge ${from}?`}</p>
        <Quiet request={request} env={env} />
        <div className="ny-fromto">
          <span className="ny-line" aria-hidden="true" />
          <div className="ny-row">
            <span className="ny-node ny-node-from" aria-hidden="true" />
            <span className="ny-row-label">From</span>
            <span className="ny-branch" data-testid="merge-from">
              {from}
            </span>
            {sha !== null ? (
              <span className="ny-sha" title={sha} data-testid="merge-sha">
                {shortSha(sha)}
              </span>
            ) : null}
          </div>
          <div className="ny-row">
            <span className="ny-node ny-node-into" aria-hidden="true" />
            <span className="ny-row-label">Into</span>
            <span className="ny-branch" data-testid="merge-into">
              {base ?? ""}
            </span>
          </div>
        </div>
        {request.blocks.length > 0 ? (
          <div className="ny-holding">
            <span className="ny-holding-icon">
              <Icon name="pause" size={14} />
            </span>
            <span className="ny-holding-label">Holding up</span>
            <span className="ny-holding-what">{request.blocks.join(", ")}</span>
          </div>
        ) : null}
        <BodyError env={env} />
      </div>
      {request.verbs.includes("ack") && !closed ? (
        <div className="ny-foot ny-foot-surface">
          <span className="ny-spacer" />
          <Button size="lg" disabled={env.inFlight} onClick={() => hold(request.id, request.revision)}>
            Hold
          </Button>
          <Button variant="primary" size="lg" icon="git-branch" disabled={env.inFlight} onClick={() => env.onAct({ kind: "ack" })}>
            Approve merge
          </Button>
        </div>
      ) : null}
      <Outcome env={env} />
    </>
  );
}

// ---- a run question ----

function RunHead(props: { request: RequestSummary; env: CardEnv; tag?: ReactNode }) {
  const { request, env } = props;
  return (
    <div className="ny-head">
      <span className="ny-icon ny-icon-run">
        <Icon name="activity" size={12} />
      </span>
      <Who name={env.spine ?? request.run} where={env.spine !== null ? request.run : null} />
      {props.tag}
      <span className="ny-spacer" />
      <When at={request.opened_at} compact={env.compact} />
      {env.compact ? <OpenPage /> : null}
    </div>
  );
}

function RunQuestionCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  const [text, setText] = useState("");
  const fieldId = useId();
  const { stop, gaps } = questionOf(env.detail?.body);
  const takesAnswer = request.verbs.includes("answer") && env.done === undefined;
  return (
    <>
      <div className="ny-main">
        <RunHead request={request} env={env} />
        {stop !== null ? (
          <p className="ny-question" data-testid="stop-text">
            {stop}
          </p>
        ) : null}
        {gaps.length > 0 ? (
          <ul className="ny-gaps" aria-label={`Gaps of ${request.id}`}>
            {gaps.map((g, i) => (
              <li key={i}>
                <span className="ny-gap-section">{g.section}</span>
                <span className="ny-gap-question">{g.question}</span>
                <Chip tone={g.severity === "blocking" ? "red" : "neutral"}>{g.severity}</Chip>
              </li>
            ))}
          </ul>
        ) : null}
        <Quiet request={request} env={env} />
        {env.compact ? null : (
          <Facts
            rows={[
              ["Blocks", request.blocks.length > 0 ? request.blocks.join(", ") : (request.unit ?? "none")],
              ["Stage", request.stage],
              ["Reason", request.reason],
            ]}
          />
        )}
        <BodyError env={env} />
      </div>
      {takesAnswer ? (
        <div className="ny-foot ny-foot-col">
          <textarea
            id={fieldId}
            className="ny-field"
            aria-label="Your answer"
            rows={2}
            placeholder="Type your answer. The Implementer gets it word for word."
            value={text}
            readOnly={env.inFlight}
            onChange={(e) => setText(e.target.value)}
          />
          <div className="ny-foot-row">
            <NotBuilt as="button" label="Open run view" />
            <span className="ny-spacer" />
            <Button variant="primary" disabled={text.trim() === "" || env.inFlight} onClick={() => env.onAct({ kind: "answer", text })}>
              Send
            </Button>
          </div>
        </div>
      ) : null}
      <Outcome env={env} />
    </>
  );
}

// ---- a parked item ----

function ParkedCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  const unit = request.unit ?? request.run;
  return (
    <>
      <div className="ny-main">
        <RunHead request={request} env={env} tag={<StatePill state="needs_you">Parked</StatePill>} />
        <p className="ny-question">{`${unit} is parked: ${request.reason}`}</p>
        <Quiet request={request} env={env} />
        {env.compact ? null : (
          <Facts
            rows={[
              ["Blocks", request.blocks.length > 0 ? request.blocks.join(", ") : unit],
              ["Cause", request.reason],
              ["Stage", request.stage],
            ]}
          />
        )}
        <BodyError env={env} />
      </div>
      {request.verbs.includes("clear") && env.done === undefined ? (
        <div className="ny-foot ny-foot-col">
          <p className="ny-foot-line">{`Fix the cause on the server, then resume ${unit}.`}</p>
          <NotBuilt as="field" label="Note for the Implementer" description="What you fixed, for example: added DATABASE_URL to the runner env" />
          <div className="ny-foot-row">
            <NotBuilt as="button" label="Open run view" />
            <span className="ny-spacer" />
            <Button variant="primary" icon="play" disabled={env.inFlight} onClick={() => env.onAct({ kind: "clear" })}>
              {`Resume ${unit}`}
            </Button>
          </div>
        </div>
      ) : null}
      <Outcome env={env} />
    </>
  );
}

/** A request with no verb the page offers (a run-level request that takes none): its facts, no act. */
function OtherCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  return (
    <div className="ny-main">
      <RunHead request={request} env={env} />
      <p className="ny-question">{firstLine(request.reason)}</p>
      <Quiet request={request} env={env} />
      {env.compact ? null : (
        <Facts
          rows={[
            ["Kind", request.kind],
            ["Stage", request.stage],
          ]}
        />
      )}
    </div>
  );
}

// ---- the block cards (r2.s11.w5; 12a to 12e) ----

const NOTE_LABEL = "Note for the next seat";
const NOTE_PLACEHOLDER = "Optional. It goes with the option you press.";

function projectNameOf(env: CardEnv, decision: Decision): string | null {
  const id = decision.refs.project;
  if (id === undefined) return null;
  return env.projects?.find((p) => p.id === id)?.name ?? id;
}

function namesOf(env: CardEnv, decision: Decision | undefined): Names {
  return { spine: env.spine, project: decision === undefined ? null : projectNameOf(env, decision) };
}

/** The kind label that leads every block card and every answered row. */
export function KindTag(props: { kind: string }) {
  const label = kindLabel(props.kind);
  return (
    <span className={`ny-tag ny-tag-${label.tone}`} data-testid="kind-label">
      <Icon name={label.icon} size={12} />
      {label.text}
    </span>
  );
}

/** The head of a block card: the label, the run and project, the time and age, the request ID (12a); in the dock `<R-n> · <age>` and `<spine> · <project>` (12d). */
function BlockHead(props: { request: RequestSummary; names: Names; compact: boolean }) {
  const { request, names, compact } = props;
  const place = runPlace(request, names);
  const run = (
    <span className="ny-run">
      <span className="ny-icon-bare">
        <Icon name={request.kind === "merge_ack" || request.kind === "merge_failed" ? "git-branch" : "activity"} size={12} />
      </span>
      <Who name={place.name} where={place.where !== null && place.where !== place.name ? place.where : null} />
    </span>
  );
  if (compact) {
    return (
      <div className="ny-head ny-head-block ny-head-dock">
        <KindTag kind={request.kind} />
        <span className="ny-spacer" />
        <span className="ny-age dock-age">{`${request.id} · ${age(request.opened_at)}`}</span>
        <div className="ny-head-run">{run}</div>
      </div>
    );
  }
  return (
    <div className="ny-head ny-head-block">
      <KindTag kind={request.kind} />
      {run}
      <span className="ny-spacer" />
      <span className="ny-age">{timeAge(request.opened_at)}</span>
      <span className="ny-id">{request.id}</span>
    </div>
  );
}

function IdChips(props: { decision: Decision; projectName: string | null }) {
  const chips = chipsOf(props.decision, props.projectName);
  return (
    <div className="ny-chips" aria-label="IDs">
      {chips.map((c) => (
        <span className="ny-idchip" key={`${c.label}${c.value}`} title={c.title}>
          {c.label !== "" ? <span className="ny-idchip-label">{c.label}</span> : null}
          {c.value}
        </span>
      ))}
    </div>
  );
}

/** The ring of a press in flight: it turns, and it stands still under `prefers-reduced-motion`. */
function Ring() {
  return <span className="ny-ring" aria-hidden="true" />;
}

/** The refused and not-connected states of a press (12e). The working state is on the pressed control. */
function ActState(props: { env: CardEnv }) {
  const { line, serverName, onRetry } = props.env;
  if (line === undefined || line.phase === "asking" || line.phase === "spending") return null;
  if (line.phase === "ask_unreachable" || line.phase === "spend_unreachable") {
    const text =
      line.phase === "ask_unreachable"
        ? `Not connected to ${serverName}. Nothing was sent. You can press again once it’s back.`
        : `Not connected to ${serverName}. The answer may not have arrived. Retry sends the same one again.`;
    return (
      <div className="ny-state ny-state-down" role="alert">
        <span className="ny-state-icon">
          <Icon name="refresh" size={14} />
        </span>
        <span className="ny-state-text">{text}</span>
        <Button size="sm" onClick={onRetry}>
          Retry now
        </Button>
      </div>
    );
  }
  return (
    <div className="ny-state ny-state-refused" role="alert">
      <span className="ny-state-icon">
        <Icon name="alert" size={14} />
      </span>
      <span className="ny-state-text">
        <b>{`${serverName} refused it.`}</b>
        {` ${line.message ?? ""}`}
      </span>
      {line.again ? (
        <Button size="sm" onClick={onRetry}>
          Try again
        </Button>
      ) : null}
    </div>
  );
}

/** What the pressed control of a card is doing: the working text, and the others waiting. */
function working(line: ActLine | undefined, control: string): boolean {
  return (line?.phase === "asking" || line?.phase === "spending") && line.control === control;
}

function OptionButton(props: {
  option: DecisionOption;
  request: RequestSummary;
  decision: Decision;
  recommended: boolean;
  env: CardEnv;
  onPress(): void;
}) {
  const { option, request, decision, recommended, env } = props;
  const descId = useId();
  const line = effectLine(option, request, decision);
  const title = optionTitle(option);
  const busy = working(env.line, controlOf({ kind: "option", option, unit: "" }));
  return (
    <button
      type="button"
      className={`ny-opt${recommended ? " ny-opt-rec" : ""}${busy ? " ny-opt-working" : ""}`}
      data-option={option.id}
      title={title === "" ? undefined : title}
      aria-describedby={title === "" ? undefined : descId}
      disabled={env.inFlight}
      onClick={props.onPress}
    >
      {recommended && decision.recommended !== undefined ? (
        <span className="ny-opt-recline">
          <Icon name="check" size={12} />
          <span>
            {`Recommended by ${recommendedSource(decision.recommended.by, decision)}`}
            <span className="ny-opt-why">{` · ${decision.recommended.why}`}</span>
          </span>
        </span>
      ) : null}
      <span className="ny-opt-label">{option.label}</span>
      <span className="ny-opt-meta">
        {busy ? (
          <span className="ny-opt-does ny-opt-sending">
            <Ring />
            {`Sending to ${env.serverName}…`}
          </span>
        ) : (
          <span className="ny-opt-does" aria-label={effectText(line)}>
            <Icon name={option.effect.verb === "answer" && option.effect.to === undefined ? "chat" : "play"} size={12} />
            <span>
              {line.before}
              {line.chip !== null ? <code className="ny-stage">{line.chip}</code> : null}
              {line.after}
            </span>
          </span>
        )}
        <span className="ny-opt-by">{optionWriter(option, decision)}</span>
      </span>
      {title === "" ? null : (
        <span id={descId} className="ui-sr-only">
          {title}
        </span>
      )}
    </button>
  );
}

function TextBox(props: {
  form: TextForm;
  env: CardEnv;
  value: string;
  onChange(value: string): void;
  onSend(): void;
}) {
  const { form, env, value, onChange, onSend } = props;
  const id = useId();
  const typed = form.form === "typed";
  const label = form.form === "typed" ? form.label : NOTE_LABEL;
  const placeholder = form.form === "typed" ? form.placeholder : NOTE_PLACEHOLDER;
  const sending = working(env.line, "send");
  return (
    <div className="ny-textbox">
      <label className="ny-textbox-label" htmlFor={id}>
        {label}
      </label>
      <div className="ny-textbox-row">
        <textarea
          id={id}
          className="ny-field"
          rows={2}
          placeholder={placeholder}
          value={value}
          readOnly={env.inFlight}
          onChange={(e) => onChange(e.target.value)}
        />
        {typed ? (
          <Button variant="primary" disabled={value.trim() === "" || env.inFlight} onClick={onSend}>
            {sending ? `Sending to ${env.serverName}…` : "Send"}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function Warning(props: { children: ReactNode; title?: string }) {
  return (
    <p className="ny-warn" title={props.title}>
      <span className="ny-warn-icon">
        <Icon name="alert" size={14} />
      </span>
      <span>{props.children}</span>
    </p>
  );
}

/** The question of a block card: the seat's stop text, the gaps, or a red gate's reason. */
function BlockQuestion(props: { request: RequestSummary; decision: Decision; env: CardEnv }) {
  const { request, decision, env } = props;
  const { stop, gaps } = questionOf(env.detail?.body);
  const seat = seatName(decision.asked_by);
  return (
    <>
      {stop !== null ? (
        <div className="ny-asks">
          {seat !== null ? <span className="ny-asks-eyebrow">{`${seat} asks`}</span> : null}
          <p className="ny-question" data-testid="stop-text">
            {shownStopText(stop, decision)}
          </p>
        </div>
      ) : null}
      {request.kind === "red_gate" ? (
        <div className="ny-gate">
          <span className="ny-gate-label">The gate&rsquo;s reason</span>
          <code className="ny-gate-reason" title={request.reason}>
            {firstLine(request.reason)}
          </code>
        </div>
      ) : null}
      {gaps.length > 0 ? (
        <ul className="ny-gaps ny-gaps-numbered" aria-label={`Gaps of ${request.id}`}>
          {gaps.map((g, i) => (
            <li key={i}>
              <span className="ny-gap-section">{g.section}</span>
              <span className="ny-gap-question">{g.question}</span>
              <Chip tone={g.severity === "blocking" ? "red" : "neutral"}>{g.severity}</Chip>
            </li>
          ))}
        </ul>
      ) : null}
    </>
  );
}

function BlockQuestionCard(props: { request: RequestSummary; decision: Decision; env: CardEnv }) {
  const { request, decision, env } = props;
  const names = namesOf(env, decision);
  const unit = unitOf(request, decision);
  const [text, setText] = useState("");
  const open = env.done === undefined;
  const shown = open ? offeredOptions(request, decision) : [];
  const keepsOneAct = open && stopTakesNoText(request, decision);
  const form = open ? textFormOf(request, decision, shown) : null;
  const rec = recommendedOption(request, decision, shown);
  const warning = open ? parkFileWarning(request, decision, shown) : null;
  const rows = factRows(request, decision, names);

  const press = (option: DecisionOption) => {
    if (form?.form === "note" && option.effect.verb === "resume" && text.trim() !== "") {
      env.onAct({ kind: "note", option, text, unit });
    } else {
      env.onAct({ kind: "option", option, unit });
    }
  };
  const resumeWorking = working(env.line, "resume");

  return (
    <>
      <div className="ny-main">
        <BlockHead request={request} names={names} compact={env.compact} />
        <p className="ny-summary" data-testid="summary">
          {decision.summary}
        </p>
        <BlockQuestion request={request} decision={decision} env={env} />
        {env.compact ? (
          <p className="ny-facts-line" data-testid="facts-line">
            {foldedFacts(rows)}
          </p>
        ) : (
          <Facts rows={rows} />
        )}
        <IdChips decision={decision} projectName={names.project} />
        <BodyError env={env} />
      </div>
      {open && (shown.length > 0 || keepsOneAct || form !== null || decision.options_error !== undefined || warning !== null) ? (
        <div className="ny-foot ny-foot-col ny-foot-block">
          {decision.options_error !== undefined ? <Warning title={decision.options_error}>{OPTIONS_ERROR_LINE}</Warning> : null}
          {warning !== null ? <Warning>{warning}</Warning> : null}
          {keepsOneAct ? <Warning>{stopNoTextWarning(request, decision)}</Warning> : null}
          {shown.length > 0 || keepsOneAct ? (
            <div className="ny-options" role="group" aria-label={`Options of ${request.id}`}>
              {shown.map((option) => (
                <OptionButton
                  key={option.id}
                  option={option}
                  request={request}
                  decision={decision}
                  recommended={rec === option.id}
                  env={env}
                  onPress={() => press(option)}
                />
              ))}
              {keepsOneAct ? (
                <button
                  type="button"
                  className={`ny-opt${resumeWorking ? " ny-opt-working" : ""}`}
                  data-option="resume"
                  disabled={env.inFlight}
                  onClick={() => env.onAct({ kind: "resume", unit })}
                >
                  <span className="ny-opt-label">Resume</span>
                  <span className="ny-opt-meta">
                    {resumeWorking ? (
                      <span className="ny-opt-does ny-opt-sending">
                        <Ring />
                        {`Sending to ${env.serverName}…`}
                      </span>
                    ) : (
                      <span className="ny-opt-does">
                        <Icon name="play" size={12} />
                        <span>
                          {`Resumes ${unit} at `}
                          <code className="ny-stage">{request.stage}</code>
                        </span>
                      </span>
                    )}
                  </span>
                </button>
              ) : null}
            </div>
          ) : null}
          {form !== null ? (
            <TextBox form={form} env={env} value={text} onChange={setText} onSend={() => env.onAct({ kind: "send", text, unit })} />
          ) : null}
          <ActState env={env} />
        </div>
      ) : open && env.line !== undefined ? (
        <div className="ny-foot ny-foot-col ny-foot-block">
          <ActState env={env} />
        </div>
      ) : null}
      <Outcome env={env} block />
    </>
  );
}

function BlockMergeCard(props: { request: RequestSummary; decision: Decision; env: CardEnv }) {
  const { request, decision, env } = props;
  const { detail, spine } = env;
  const names = namesOf(env, decision);
  const base = fieldOf("base", request, detail);
  const sha = fieldOf("head_sha", request, detail);
  const branch = fieldOf("head_branch", request, detail);
  const from = branch ?? spine ?? request.run;
  const closed = env.done !== undefined;
  const approve = decision.options.find((o) => o.effect.verb === "ack");
  const approving = working(env.line, "approve");
  return (
    <>
      <div className="ny-main">
        <BlockHead request={request} names={names} compact={env.compact} />
        <p className="ny-question ny-question-merge">{base !== null ? `Merge ${from} into ${base}?` : `Merge ${from}?`}</p>
        <p className="ny-summary" data-testid="summary">
          {decision.summary}
        </p>
        <IdChips decision={decision} projectName={names.project} />
        <div className="ny-merge-grid">
          <div className="ny-merge-left">
            <div className="ny-fromto">
              <span className="ny-line" aria-hidden="true" />
              <div className="ny-row">
                <span className="ny-node ny-node-from" aria-hidden="true" />
                <span className="ny-row-label">From</span>
                <span className="ny-branch" data-testid="merge-from">
                  {from}
                </span>
                {sha !== null ? (
                  <span className="ny-sha" title={sha} data-testid="merge-sha">
                    {shortSha(sha)}
                  </span>
                ) : null}
              </div>
              <div className="ny-row">
                <span className="ny-node ny-node-into" aria-hidden="true" />
                <span className="ny-row-label">Into</span>
                <span className="ny-branch" data-testid="merge-into">
                  {base ?? ""}
                </span>
              </div>
            </div>
            {request.blocks.length > 0 ? (
              <div className="ny-holding">
                <span className="ny-holding-icon">
                  <Icon name="pause" size={14} />
                </span>
                <span className="ny-holding-label">Holding up</span>
                <span className="ny-holding-what">{request.blocks.join(", ")}</span>
              </div>
            ) : null}
            <NotBuilt as="button" label="Review the PR" icon="search" />
            <NotBuilt as="button" label="Open on GitHub" icon="arrow-right" />
          </div>
          {request.verbs.includes("ack") && !closed ? (
            <div className="ny-approve">
              <span className="ny-approve-icon">
                <Icon name="git-branch" size={16} />
              </span>
              <div className="ny-approve-body">
                <span className="ny-approve-title">Approve the merge</span>
                <span className="ny-approve-sub">Here, on your phone, or from the CLI</span>
                <div className="ny-approve-buttons">
                  <Button size="lg" disabled={env.inFlight} onClick={() => hold(request.id, request.revision)}>
                    Hold
                  </Button>
                  <Button
                    variant="primary"
                    size="lg"
                    icon={approving ? undefined : "git-branch"}
                    disabled={env.inFlight}
                    onClick={() => env.onAct({ kind: "approve", ...(approve !== undefined ? { option: approve.id } : {}) })}
                  >
                    {approving ? "Approving…" : "Approve merge"}
                  </Button>
                </div>
              </div>
            </div>
          ) : null}
        </div>
        <BodyError env={env} />
      </div>
      {!closed && env.line !== undefined ? (
        <div className="ny-foot ny-foot-col ny-foot-block">
          <ActState env={env} />
        </div>
      ) : null}
      <Outcome env={env} block />
    </>
  );
}

/** A card whose body has not arrived: its head, the summary of the row when it has one, and no act. */
function PendingCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  const names: Names = { spine: env.spine, project: null };
  return (
    <div className="ny-main">
      <BlockHead request={request} names={names} compact={env.compact} />
      {request.summary !== undefined ? (
        <p className="ny-summary" data-testid="summary">
          {request.summary}
        </p>
      ) : null}
    </div>
  );
}

export function BlockCard(props: { request: RequestSummary; decision: Decision; env: CardEnv }) {
  return props.request.kind === "merge_ack" ? <BlockMergeCard {...props} /> : <BlockQuestionCard {...props} />;
}

export function RequestCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  const kind = cardKind(request);
  const decision = env.detail?.decision;
  // A card draws from its body: with a decision block it is a block card; with a body and no block it is
  // today's card; before the body arrives it draws its head and no act. A failed `show` keeps today's card
  // and its error line with Refresh.
  const waiting = env.detail === undefined && env.bodyError === null;
  return (
    <article
      className={`ny-card${env.compact ? " ny-card-compact" : ""}`}
      aria-label={`Request ${request.id}`}
      data-kind={request.kind}
      data-card={decision !== undefined ? "block" : kind}
      data-request={request.id}
    >
      {decision !== undefined ? (
        <BlockCard request={request} decision={decision} env={env} />
      ) : waiting ? (
        <PendingCard request={request} env={env} />
      ) : kind === "merge" ? (
        <MergeCard request={request} env={env} />
      ) : kind === "run_question" ? (
        <RunQuestionCard request={request} env={env} />
      ) : kind === "parked" ? (
        <ParkedCard request={request} env={env} />
      ) : (
        <OtherCard request={request} env={env} />
      )}
    </article>
  );
}

// ---- a chat that waits ----

/**
 * A live session whose state is `needs_you`. Its question text and option buttons are in the chat: a device
 * reads a chat's items only on the stream that follows it (ADR-0012), so the card shows who waits and where,
 * and Open session. This is an accepted gap with that reason.
 */
export function ChatQuestionCard(props: {
  session: ChatSessionRecord;
  agent: string;
  projects: ProjectRecord[] | null;
  compact: boolean;
}) {
  const { session, agent, projects, compact } = props;
  const { openSession } = useNavigation();
  const at = parseTime(session.last_turn_at);
  const place = session.branch !== null || session.detached ? branchText(session, projects) : lastSegment(session.worktree_dir);
  return (
    <article className={`ny-card${compact ? " ny-card-compact" : ""}`} aria-label={`Chat ${session.id}`} data-card="chat">
      <div className="ny-main">
        <div className="ny-head">
          <Avatar agent={agentLook(session.agent_id)} size={22} state="needs_you" />
          <Who name={agent} where={session.project_name} />
          <span className="ny-spacer" />
          {at !== null && session.last_turn_at !== null ? (
            <span className={compact ? "ny-age dock-age" : "ny-age"}>{`${clock(at)} · ${age(session.last_turn_at)}`}</span>
          ) : null}
        </div>
        <p className="ny-question">Waiting for your answer in the chat</p>
        {compact ? null : <Facts rows={[["Session", place]]} />}
      </div>
      <div className="ny-foot">
        <span className="ny-spacer" />
        <Button variant="ghost" size="sm" onClick={() => openSession(session.id)}>
          Open session
          <Icon name="chevron-right" size={14} />
        </Button>
      </div>
    </article>
  );
}
