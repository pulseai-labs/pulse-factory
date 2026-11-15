// The message stream of a chat (11a): a day divider before the first item of each day, the operator's
// bubble with its time below, and the agent's turns. A turn has a header (the avatar, the agent, the time),
// one "Worked for" disclosure that folds its tool calls, its replies as Markdown, and its cards. Every item
// stays one `article` with its label and its `seq` in `data-seq`. Replies are set as React elements
// (`markdown.tsx`), never as HTML. The answer items (and a start card's result) render inside their cards.
import type { ChatItemBody, ChatItemRecord, QuestionRecord, ToolTextRecord } from "@pulse-factory/api-client";
import { useState } from "react";
import { Column } from "../shell/Column";
import { Avatar, Chip, Icon, agentLook } from "../ui";
import { durationText, timeText } from "./chat-format";
import { ApprovalCard, QuestionCard, type AnswerFn } from "./Cards";
import { Markdown } from "./markdown";
import { StartCard } from "./StartCard";
import { ToolText } from "./ToolText";
import { dayKey, dayLabel, firstOf, groupTurns, type Block } from "./turns";

type Body<K extends ChatItemBody["kind"]> = Extract<ChatItemBody, { kind: K }>;

function Entry(props: { label: string; kind: string; seq: number; children: React.ReactNode }) {
  return (
    <article className={`entry entry-${props.kind}`} aria-label={props.label} data-seq={props.seq}>
      {props.children}
    </article>
  );
}

/** The one-line target of a tool call: the first of the usual fields of its input, else its first line. */
export function toolTarget(input: ToolTextRecord | null): string {
  if (input === null) return "";
  const text = input.text.trim();
  try {
    const parsed: unknown = JSON.parse(text);
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
      const fields = parsed as Record<string, unknown>;
      for (const key of ["command", "file_path", "path", "pattern", "url", "query", "description"]) {
        const value = fields[key];
        if (typeof value === "string" && value !== "") return value.split("\n")[0] ?? "";
      }
      const first = Object.values(fields).find((v) => typeof v === "string" && v !== "");
      if (typeof first === "string") return first.split("\n")[0] ?? "";
      return "";
    }
  } catch {
    // Not JSON: the text itself is the target.
  }
  return text.split("\n")[0] ?? "";
}

function ToolRow(props: { seq: number | null; tool: string; input: ToolTextRecord | null; output: ToolTextRecord | null; isError: boolean; running: boolean }) {
  const { seq, tool, input, output, isError, running } = props;
  const target = toolTarget(input);
  return (
    <article className="tool-row" aria-label="Tool call" data-seq={seq ?? undefined}>
      <div className="tool-line">
        <span className="tool-name">{tool}</span>
        {target === "" ? null : <span className="tool-target">{target}</span>}
        {running ? <Chip>running</Chip> : null}
        {isError ? <Chip tone="red">error</Chip> : null}
      </div>
      {input === null && output === null ? null : (
        <details className="tool-more">
          <summary>Details</summary>
          <ToolText label="Input" value={input} />
          <ToolText label="Output" value={output} />
        </details>
      )}
    </article>
  );
}

function DayDivider(props: { label: string }) {
  return (
    <div className="day-divider">
      <span className="rule" />
      {props.label}
      <span className="rule" />
    </div>
  );
}

/** A centred system line: the idle release, and every other notice, in the same form. */
function NoticeLine(props: { item: ChatItemRecord; body: Body<"notice"> }) {
  const { item, body } = props;
  const text =
    body.code === "idle_release"
      ? `Session released after 30 idle minutes · ${timeText(item.created_at)} · the next message resumes it`
      : body.text;
  return (
    <Entry label="Notice" kind="notice" seq={item.seq}>
      <span className="rule" />
      <span className="notice-pill">
        <Icon name="clock" size={12} />
        {text}
      </span>
      <span className="rule" />
    </Entry>
  );
}

interface Shared {
  items: ChatItemRecord[];
  bySeq: Map<number, ChatItemRecord>;
  answers: Map<number, ChatItemRecord>;
  running: Record<string, string>;
  readOnly: boolean;
  onAnswer: AnswerFn;
}

/** One item that is not a tool call or a turn result, as its article or card. */
function ItemView(props: { item: ChatItemRecord } & Shared) {
  const { item, bySeq, answers, readOnly, onAnswer } = props;
  const body = item.body;
  switch (body.kind) {
    case "operator_message":
      return (
        <Entry label="Operator message" kind="operator" seq={item.seq}>
          <p className="bubble">{body.text}</p>
          <time className="entry-time">{timeText(item.created_at)}</time>
        </Entry>
      );
    case "assistant_text":
      return (
        <Entry label="Reply" kind="reply" seq={item.seq}>
          <Markdown text={body.text} />
        </Entry>
      );
    case "approval_request":
      return (
        <ApprovalCard
          request={item}
          body={body}
          answer={answers.get(item.seq) ?? null}
          readOnly={readOnly}
          onAnswer={onAnswer}
        />
      );
    case "question_request":
      return (
        <QuestionCard
          request={item}
          requestId={body.request_id}
          questions={body.items as QuestionRecord[]}
          answer={answers.get(item.seq) ?? null}
          readOnly={readOnly}
          onAnswer={onAnswer}
        />
      );
    case "approval_answer":
    case "question_answer":
      // Inside its card. An answer whose request this app does not hold has no card to go in.
      if (item.answers_seq !== null && bySeq.has(item.answers_seq)) return null;
      return (
        <Entry label="Answer" kind="notice" seq={item.seq}>
          <p className="entry-text">{`An answer to a request this app has not seen (${body.outcome})`}</p>
        </Entry>
      );
    case "start_card": {
      const answer = answers.get(item.seq);
      return (
        <StartCard
          seq={item.seq}
          card={body}
          proposedAt={item.created_at}
          result={answer && answer.body.kind === "start_result" ? answer.body : null}
          resultAt={answer && answer.body.kind === "start_result" ? answer.created_at : null}
          readOnly={readOnly}
        />
      );
    }
    case "start_result": {
      // Inside its card. A result whose card this app does not hold has no card to go in.
      const card = item.answers_seq === null ? undefined : bySeq.get(item.answers_seq);
      if (card && card.body.kind === "start_card") return null;
      return (
        <Entry label="Answer" kind="notice" seq={item.seq}>
          <p className="entry-text">{`An answer to a start card this app has not seen (${body.result})`}</p>
        </Entry>
      );
    }
    case "background_task":
      return (
        <Entry label="Background task" kind="task" seq={item.seq}>
          <p className="entry-text">
            {`Background task ${body.event === "finished" ? "finished" : "started"}: ${body.description ?? body.task_id}${body.summary ? ` (${body.summary})` : ""}`}
          </p>
        </Entry>
      );
    case "notice":
      return <NoticeLine item={item} body={body} />;
    case "redacted":
      return (
        <Entry label="Redacted item" kind="notice" seq={item.seq}>
          <p className="entry-text muted">{`Redacted ${body.item_kind}`}</p>
        </Entry>
      );
    case "unknown":
      return (
        <Entry label="Unsupported item" kind="notice" seq={item.seq}>
          <p className="entry-text muted">{`An item this app cannot show (${body.item_kind})`}</p>
        </Entry>
      );
    case "tool_call":
    case "turn_result":
      return null;
  }
}

/** What a turn that ended says besides its duration: the failure, or the stop (with 11b's stop icon, KL-3). */
function endLine(result: Body<"turn_result">, at: string): string | null {
  if (result.stop === "interrupted") return `You stopped the turn · ${timeText(at)}. The session stays open.`;
  if (result.is_error) return `The turn failed: ${result.stop}`;
  return null;
}

/**
 * The "Worked for" disclosure: the turn's tool calls, folded. It is open while the turn runs and closed
 * once the turn has ended; the operator's own toggle wins after that. It is the `Turn result` article
 * of its turn once the result is held, and it has no label before.
 */
function Disclosure(props: {
  calls: ChatItemRecord[];
  lone: Array<[string, string]>;
  running: Record<string, string>;
  result: ChatItemRecord | null;
}) {
  const { calls, lone, running, result } = props;
  const [chosen, setChosen] = useState<boolean | null>(null);
  const ended = result !== null;
  const open = chosen ?? !ended;
  const resultBody = result?.body.kind === "turn_result" ? result.body : null;
  const label = resultBody ? `Worked for ${durationText(resultBody.duration_ms)}` : "Working…";
  return (
    <article
      className="work"
      aria-label={ended ? "Turn result" : undefined}
      data-seq={result?.seq}
    >
      <button type="button" className="work-toggle" aria-expanded={open} onClick={() => setChosen(!open)}>
        {label}
        <Icon name={open ? "chevron-down" : "chevron-right"} size={14} />
      </button>
      {open ? (
        <div className="work-rows">
          {calls.map((it) => {
            const body = it.body as Body<"tool_call">;
            return (
              <ToolRow
                key={it.seq}
                seq={it.seq}
                tool={body.tool}
                input={body.input}
                output={body.output}
                isError={body.is_error}
                running={!body.finished && body.call_id in running}
              />
            );
          })}
          {lone.map(([callId, tool]) => (
            <ToolRow key={`running-${callId}`} seq={null} tool={tool} input={null} output={null} isError={false} running />
          ))}
        </div>
      ) : null}
    </article>
  );
}

function Turn(props: {
  block: Extract<Block, { type: "turn" }>;
  lone: Array<[string, string]>;
  delta: string;
  agentId: string;
  agentName: string;
} & Shared) {
  const { block, lone, delta, agentId, agentName, ...shared } = props;
  const { result } = block;
  const resultBody = result?.body.kind === "turn_result" ? result.body : null;
  const calls = block.items.filter((i) => i.body.kind === "tool_call");
  const hasWork = calls.length > 0 || lone.length > 0;
  const first = block.items[0] ?? result;
  // The disclosure takes the place of the turn's first tool call; every other item keeps its `seq` order.
  const firstCall = calls[0];
  const end = resultBody && result ? endLine(resultBody, result.created_at) : null;

  return (
    <div className="turn">
      <div className="turn-head">
        <Avatar agent={agentLook(agentId)} size={20} />
        <span className="turn-agent">{agentName}</span>
        {first ? <time className="entry-time">{timeText(first.created_at)}</time> : null}
      </div>
      {block.items.map((it) => {
        if (it.body.kind === "tool_call") {
          return it === firstCall ? (
            <Disclosure key={`work-${it.seq}`} calls={calls} lone={lone} running={shared.running} result={result} />
          ) : null;
        }
        return <ItemView key={it.seq} item={it} {...shared} />;
      })}
      {calls.length === 0 && lone.length > 0 ? (
        <Disclosure key="work-lone" calls={[]} lone={lone} running={shared.running} result={result} />
      ) : null}
      {delta === "" ? null : (
        <article className="entry entry-reply" aria-label="Reply in progress">
          <p className="entry-text">{delta}</p>
        </article>
      )}
      {result && resultBody ? (
        hasWork ? (
          end ? (
            <p className="turn-end">
              {resultBody.stop === "interrupted" ? <Icon name="square" size={11} /> : null}
              {end}
            </p>
          ) : null
        ) : (
          <Entry label="Turn result" kind="result" seq={result.seq}>
            <p className="turn-end">
              {end !== null && resultBody.stop === "interrupted" ? <Icon name="square" size={11} /> : null}
              {end ?? `Worked for ${durationText(resultBody.duration_ms)}`}
            </p>
          </Entry>
        )
      ) : null}
    </div>
  );
}

export function Messages(props: {
  items: ChatItemRecord[];
  delta: string;
  /** `tool_running` calls with no finished item: call ID to tool. */
  running: Record<string, string>;
  /** Cards and the composer cannot change anything: the session ended. */
  readOnly: boolean;
  onAnswer: AnswerFn;
  agentId: string;
  agentName: string;
  /** The session's state is `working`: a line under the last item says so. */
  working: boolean;
}) {
  const { items, delta, running, readOnly, onAnswer, agentId, agentName, working } = props;
  const bySeq = new Map(items.map((i) => [i.seq, i]));
  const answers = new Map<number, ChatItemRecord>();
  for (const it of items) {
    if (
      it.answers_seq !== null &&
      (it.body.kind === "approval_answer" || it.body.kind === "question_answer" || it.body.kind === "start_result")
    ) {
      answers.set(it.answers_seq, it);
    }
  }
  const heldCalls = new Set<string>();
  for (const it of items) if (it.body.kind === "tool_call") heldCalls.add(it.body.call_id);
  const lone = Object.entries(running).filter(([callId]) => !heldCalls.has(callId));

  // The running calls and the delta text belong to the turn that runs: the last turn when it has no result,
  // else a turn that has not made an item yet.
  const blocks = groupTurns(items);
  const last = blocks.at(-1);
  const hasOpenTurn = last !== undefined && last.type === "turn" && last.result === null;
  if (!hasOpenTurn && (lone.length > 0 || delta !== "")) blocks.push({ type: "turn", items: [], result: null });
  const openIndex = blocks.length - 1;

  const nowMs = Date.now();
  let previousDay: string | null = null;
  const shared: Shared = { items, bySeq, answers, running, readOnly, onAnswer };

  return (
    <div className="messages" role="log" aria-label="Messages">
      <Column max={736}>
        {blocks.map((block, index) => {
          const head = firstOf(block);
          const day = head ? dayKey(head.created_at) : null;
          const divider = day !== null && day !== previousDay ? <DayDivider key={`day-${day}`} label={dayLabel(day, nowMs)} /> : null;
          if (day !== null) previousDay = day;
          const key = head ? head.seq : `open-${index}`;
          const view =
            block.type === "item" ? (
              <ItemView key={key} item={block.item} {...shared} />
            ) : (
              <Turn
                key={key}
                block={block}
                lone={index === openIndex && block.result === null ? lone : []}
                delta={index === openIndex && block.result === null ? delta : ""}
                agentId={agentId}
                agentName={agentName}
                {...shared}
              />
            );
          return divider ? [divider, view] : view;
        })}
        {working ? (
          <div className="working-line" role="status">
            <span className="working-dot" />
            {`${agentName} is working`}
          </div>
        ) : null}
      </Column>
    </div>
  );
}
