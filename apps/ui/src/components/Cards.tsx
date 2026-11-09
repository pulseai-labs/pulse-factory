// The approval and question cards (C2, M3). A request is pending while no held item answers it, so a
// reload, a reopened stream and a reconnect show the same pending cards. A pending card has no time
// limit and no timer, and it answers nothing by itself: only the operator's click sends an answer.
import type { ChatAnswerArg, ChatItemBody, ChatItemRecord, QuestionRecord } from "@pulse-factory/api-client";
import { useState } from "react";
import { timeText } from "./chat-format";
import { ToolText } from "./ToolText";

/**
 * Sends one answer for the request at `seq`. It resolves to the text to show on the card (a
 * refusal, or the note of an answer that could not act), or `null` when the answer went through.
 */
export type AnswerFn = (seq: number, answer: ChatAnswerArg) => Promise<string | null>;

type Approval = Extract<ChatItemBody, { kind: "approval_request" }>;

/** How a closed request ended, as text: answered, withdrawn by the agent, or lost. */
function Outcome(props: { outcome: "answered" | "lost" | "withdrawn"; reason: string | null; answered: React.ReactNode }) {
  const { outcome, reason, answered } = props;
  if (outcome === "answered") return <>{answered}</>;
  if (outcome === "withdrawn") {
    return (
      <div className="card-outcome">
        <p>
          <strong>Withdrawn by the agent</strong>
        </p>
        {reason ? <p className="entry-text muted">{reason}</p> : null}
      </div>
    );
  }
  return (
    <div className="card-outcome">
      <p>
        <strong>{`Lost: ${reason ?? "the request was closed"}`}</strong>
      </p>
    </div>
  );
}

/** The state of a pending card's answer: busy while it goes out, the text to show after it. */
function useAnswering(onAnswer: AnswerFn, seq: number) {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  // After an answer went through the card waits for its answer item: it follows the items.
  const [sent, setSent] = useState(false);
  async function answer(arg: ChatAnswerArg) {
    if (busy) return;
    setBusy(true);
    setMessage(null);
    const failure = await onAnswer(seq, arg);
    setMessage(failure);
    setSent(failure === null);
    setBusy(false);
  }
  return { busy, message, sent, answer };
}

export function ApprovalCard(props: {
  request: ChatItemRecord;
  body: Approval;
  answer: ChatItemRecord | null;
  readOnly: boolean;
  onAnswer: AnswerFn;
}) {
  const { request, body, answer, readOnly, onAnswer } = props;
  const outcome = answer && answer.body.kind === "approval_answer" ? answer.body : null;
  const answering = useAnswering(onAnswer, request.seq);
  const [denial, setDenial] = useState("");

  return (
    <article className="entry entry-card" aria-label="Approval request">
      <div className="tool-head">
        <strong>{body.tool}</strong>
        <span className="muted">needs your approval</span>
      </div>
      <ToolText label="Input" value={body.input} />
      {body.reason ? <p className="entry-text muted">{body.reason}</p> : null}
      {outcome ? (
        <Outcome
          outcome={outcome.outcome}
          reason={outcome.reason}
          answered={
            <div className="card-outcome">
              <p>
                <strong>{outcome.decision === "allow" ? "Allowed" : "Denied"}</strong>
              </p>
              {outcome.message ? <p className="entry-text">{outcome.message}</p> : null}
            </div>
          }
        />
      ) : readOnly ? null : (
        <>
          <p className="muted">{`Needs you since ${timeText(request.created_at)}`}</p>
          <div className="card-actions">
            <button
              className="btn btn-primary"
              type="button"
              disabled={answering.busy || answering.sent}
              onClick={() => void answering.answer({ type: "allow" })}
            >
              Allow
            </button>
            <button
              className="btn"
              type="button"
              disabled={answering.busy || answering.sent}
              onClick={() => void answering.answer({ type: "deny", value: { message: denial.trim() } })}
            >
              Deny
            </button>
            <input
              className="input"
              type="text"
              aria-label="Message for a denial"
              placeholder="Optional message for a denial"
              maxLength={1000}
              value={denial}
              disabled={answering.busy || answering.sent}
              onChange={(e) => setDenial(e.target.value)}
            />
          </div>
          {answering.sent ? <p className="muted">Answer sent. Waiting for the agent.</p> : null}
          {answering.message ? (
            <p className="problem-title" role="alert">
              {answering.message}
            </p>
          ) : null}
        </>
      )}
    </article>
  );
}

/** One question's answer as text: the chosen labels and a typed answer, joined with `, `. */
function joined(chosen: string[], other: string): string {
  const typed = other.trim();
  return [...chosen, ...(typed === "" ? [] : [typed])].join(", ");
}

export function QuestionCard(props: {
  request: ChatItemRecord;
  questions: QuestionRecord[];
  answer: ChatItemRecord | null;
  readOnly: boolean;
  onAnswer: AnswerFn;
}) {
  const { request, questions, answer, readOnly, onAnswer } = props;
  const outcome = answer && answer.body.kind === "question_answer" ? answer.body : null;
  const answering = useAnswering(onAnswer, request.seq);
  const [chosen, setChosen] = useState<Record<number, string[]>>({});
  const [other, setOther] = useState<Record<number, string>>({});

  const answers = questions.map((_, i) => joined(chosen[i] ?? [], other[i] ?? ""));
  const complete = answers.every((a) => a !== "");

  function choose(index: number, q: QuestionRecord, label: string, on: boolean) {
    if (q.multi_select) {
      setChosen((c) => {
        const now = c[index] ?? [];
        return { ...c, [index]: on ? [...now, label] : now.filter((l) => l !== label) };
      });
    } else {
      // One choice: it replaces a typed answer.
      setChosen((c) => ({ ...c, [index]: [label] }));
      setOther((o) => ({ ...o, [index]: "" }));
    }
  }

  function type(index: number, q: QuestionRecord, value: string) {
    setOther((o) => ({ ...o, [index]: value }));
    // A typed answer replaces the choice of a single-select question.
    if (!q.multi_select && value.trim() !== "") setChosen((c) => ({ ...c, [index]: [] }));
  }

  function submit() {
    const value: Record<string, string> = {};
    questions.forEach((q, i) => {
      value[q.text] = answers[i] ?? "";
    });
    void answering.answer({ type: "answers", value });
  }

  return (
    <article className="entry entry-card" aria-label="Question">
      {questions.map((q, i) => (
        <fieldset key={q.text} className="question" disabled={outcome !== null || readOnly || answering.busy || answering.sent}>
          <legend>
            {q.header ? <span className="chip">{q.header}</span> : null}
            <span className="entry-text">{q.text}</span>
          </legend>
          {outcome ? (
            outcome.outcome === "answered" ? (
              <p>
                <strong>{outcome.answers?.[q.text] ?? ""}</strong>
              </p>
            ) : null
          ) : (
            <>
              <ol className="question-options">
                {q.options.map((o, n) => (
                  <li key={o.label}>
                    <label>
                      <span className="muted">{`${n + 1}.`}</span>
                      <input
                        type={q.multi_select ? "checkbox" : "radio"}
                        name={`q-${request.seq}-${i}`}
                        aria-label={o.label}
                        checked={(chosen[i] ?? []).includes(o.label)}
                        onChange={(e) => choose(i, q, o.label, e.target.checked)}
                      />
                      <strong>{o.label}</strong>
                      {o.description ? <span className="muted">{` ${o.description}`}</span> : null}
                    </label>
                  </li>
                ))}
              </ol>
              <input
                className="input"
                type="text"
                aria-label="Other answer"
                placeholder="Or type a different answer"
                value={other[i] ?? ""}
                onChange={(e) => type(i, q, e.target.value)}
              />
            </>
          )}
        </fieldset>
      ))}
      {outcome ? (
        <Outcome outcome={outcome.outcome} reason={outcome.reason} answered={null} />
      ) : readOnly ? null : (
        <>
          <p className="muted">{`Needs you since ${timeText(request.created_at)}`}</p>
          <div className="card-actions">
            <button
              className="btn btn-primary"
              type="button"
              disabled={!complete || answering.busy || answering.sent}
              onClick={submit}
            >
              Answer
            </button>
          </div>
          {answering.sent ? <p className="muted">Answer sent. Waiting for the agent.</p> : null}
          {answering.message ? (
            <p className="problem-title" role="alert">
              {answering.message}
            </p>
          ) : null}
        </>
      )}
    </article>
  );
}
