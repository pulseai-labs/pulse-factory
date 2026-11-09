// The message stream of a chat (2a): one entry per held item in `seq` order, then the tool calls that
// run and the delta text of the turn in progress. Every text is set as text: no Markdown, no HTML.
// The answer items render inside their cards, not as entries of their own.
import type { ChatItemRecord, QuestionRecord, ToolTextRecord } from "@pulse-factory/api-client";
import { durationText, timeText } from "./chat-format";
import { ApprovalCard, QuestionCard, type AnswerFn } from "./Cards";
import { ToolText } from "./ToolText";

function Entry(props: { label: string; kind: string; children: React.ReactNode }) {
  return (
    <article className={`entry entry-${props.kind}`} aria-label={props.label}>
      {props.children}
    </article>
  );
}

function ToolEntry(props: { tool: string; input: ToolTextRecord | null; output: ToolTextRecord | null; isError: boolean; running: boolean }) {
  return (
    <Entry label="Tool call" kind="tool">
      <div className="tool-head">
        <strong>{props.tool}</strong>
        {props.running ? <span className="chip">running</span> : null}
        {props.isError ? <span className="chip chip-err">error</span> : null}
      </div>
      <ToolText label="Input" value={props.input} />
      <ToolText label="Output" value={props.output} />
    </Entry>
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
}) {
  const { items, delta, running, readOnly, onAnswer } = props;
  const bySeq = new Map(items.map((i) => [i.seq, i]));
  const answers = new Map<number, ChatItemRecord>();
  for (const it of items) {
    if (it.answers_seq !== null && (it.body.kind === "approval_answer" || it.body.kind === "question_answer")) {
      answers.set(it.answers_seq, it);
    }
  }
  const heldCalls = new Set<string>();
  for (const it of items) if (it.body.kind === "tool_call") heldCalls.add(it.body.call_id);
  const loneCalls = Object.entries(running).filter(([callId]) => !heldCalls.has(callId));

  return (
    <div className="messages" role="log" aria-label="Messages">
      {items.map((it) => {
        const body = it.body;
        switch (body.kind) {
          case "operator_message":
            return (
              <Entry key={it.seq} label="Operator message" kind="operator">
                <time className="entry-time">{timeText(it.created_at)}</time>
                <p className="entry-text">{body.text}</p>
              </Entry>
            );
          case "assistant_text":
            return (
              <Entry key={it.seq} label="Reply" kind="reply">
                <p className="entry-text">{body.text}</p>
              </Entry>
            );
          case "tool_call":
            return (
              <ToolEntry
                key={it.seq}
                tool={body.tool}
                input={body.input}
                output={body.output}
                isError={body.is_error}
                running={!body.finished && body.call_id in running}
              />
            );
          case "approval_request":
            return (
              <ApprovalCard
                key={it.seq}
                request={it}
                body={body}
                answer={answers.get(it.seq) ?? null}
                readOnly={readOnly}
                onAnswer={onAnswer}
              />
            );
          case "question_request":
            return (
              <QuestionCard
                key={it.seq}
                request={it}
                questions={body.items as QuestionRecord[]}
                answer={answers.get(it.seq) ?? null}
                readOnly={readOnly}
                onAnswer={onAnswer}
              />
            );
          case "approval_answer":
          case "question_answer":
            // Inside its card. An answer whose request this app does not hold has no card to go in.
            if (it.answers_seq !== null && bySeq.has(it.answers_seq)) return null;
            return (
              <Entry key={it.seq} label="Answer" kind="notice">
                <p className="entry-text">{`An answer to a request this app has not seen (${body.outcome})`}</p>
              </Entry>
            );
          case "background_task":
            return (
              <Entry key={it.seq} label="Background task" kind="task">
                <p className="entry-text">
                  {`Background task ${body.event === "finished" ? "finished" : "started"}: ${body.description ?? body.task_id}${body.summary ? ` (${body.summary})` : ""}`}
                </p>
              </Entry>
            );
          case "notice":
            return (
              <Entry key={it.seq} label="Notice" kind="notice">
                <p className="entry-text">{body.text}</p>
              </Entry>
            );
          case "turn_result":
            return (
              <Entry key={it.seq} label="Turn result" kind="result">
                <p className="entry-text muted">
                  {body.is_error ? `The turn failed: ${body.stop}` : `Worked for ${durationText(body.duration_ms)}`}
                </p>
              </Entry>
            );
          case "redacted":
            return (
              <Entry key={it.seq} label="Redacted item" kind="notice">
                <p className="entry-text muted">{`Redacted ${body.item_kind}`}</p>
              </Entry>
            );
          case "unknown":
            return (
              <Entry key={it.seq} label="Unsupported item" kind="notice">
                <p className="entry-text muted">{`An item this app cannot show (${body.item_kind})`}</p>
              </Entry>
            );
        }
      })}
      {loneCalls.map(([callId, tool]) => (
        <ToolEntry key={`running-${callId}`} tool={tool} input={null} output={null} isError={false} running />
      ))}
      {delta === "" ? null : (
        <Entry label="Reply in progress" kind="reply">
          <p className="entry-text">{delta}</p>
        </Entry>
      )}
    </div>
  );
}
