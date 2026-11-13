// The Needs you cards (11d, r1.s5.w4), built from the shared `ui/` components and `NotBuilt`. A card is a
// request: the spine merge, a run question (`stop` or `gaps`), or a parked item. A fourth card is a chat that
// waits for its answer. Each act is one press (`useConfirmedAct`); the page and the dock draw the same cards,
// the dock in `compact` form (328 px: no fact grid; the reason as a quiet line and an Open button for the
// page).
import type { ChatSessionRecord, ProjectRecord, RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import { useId, useState, type ReactNode } from "react";
import { branchText, lastSegment, parseTime } from "../components/chat-format";
import { clock } from "../components/format";
import { useNavigation } from "../shell/navigation";
import { Avatar, Button, Chip, Icon, NotBuilt, StatePill, agentLook } from "../ui";
import { type ActKind, type ActLine, type Done } from "./useConfirmedAct";
import { age, cardKind, fieldOf, firstLine, prLabel, questionOf, shortSha, timeAge } from "./format";
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
  onPress(kind: ActKind, text?: string): void;
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
function Outcome(props: { env: CardEnv }) {
  const { line, done, notice, onRetry } = props.env;
  const retry = line?.phase === "ask_unreachable" || line?.phase === "spend_unreachable";
  return (
    <>
      {line?.message ? (
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
          <Button variant="primary" size="lg" icon="git-branch" disabled={env.inFlight} onClick={() => env.onPress("ack")}>
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
            <Button variant="primary" disabled={text.trim() === "" || env.inFlight} onClick={() => env.onPress("answer", text)}>
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
            <Button variant="primary" icon="play" disabled={env.inFlight} onClick={() => env.onPress("clear")}>
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

export function RequestCard(props: { request: RequestSummary; env: CardEnv }) {
  const { request, env } = props;
  const kind = cardKind(request);
  return (
    <article
      className={`ny-card${env.compact ? " ny-card-compact" : ""}`}
      aria-label={`Request ${request.id}`}
      data-kind={request.kind}
      data-card={kind}
    >
      {kind === "merge" ? (
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
