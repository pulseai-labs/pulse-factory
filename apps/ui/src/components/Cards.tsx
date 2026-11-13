// The approval and question cards (C2, M3, 11a). A request is pending while no held item answers it, so a
// reload, a reopened stream and a reconnect show the same pending cards. A pending card has no time
// limit and no timer, and it answers nothing by itself: only the operator's press sends an answer.
//
// A question with one single-select question shows one button per option, and one press sends that
// answer (O2). A request with more than one question, or a multi-select question, shows each option as a
// toggle and one Send. The "different answer" field stays on the card (Planner ruling 6).
import type { ChatAnswerArg, ChatItemBody, ChatItemRecord, QuestionRecord } from "@pulse-factory/api-client";
import { useState } from "react";
import { timeText } from "./chat-format";
import { ToolText } from "./ToolText";
import { Button, Chip, Icon, Input } from "../ui";

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

function Feedback(props: { sent: boolean; message: string | null }) {
  return (
    <>
      {props.sent ? <p className="muted">Answer sent. Waiting for the agent.</p> : null}
      {props.message ? (
        <p className="problem-title" role="alert">
          {props.message}
        </p>
      ) : null}
    </>
  );
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
    <article className="entry chat-card" aria-label="Approval request" data-seq={request.seq}>
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
            <Button
              disabled={answering.busy || answering.sent}
              onClick={() => void answering.answer({ type: "allow" })}
            >
              Allow
            </Button>
            <Button
              disabled={answering.busy || answering.sent}
              onClick={() => void answering.answer({ type: "deny", value: { message: denial.trim() } })}
            >
              Deny
            </Button>
            <Input
              type="text"
              aria-label="Message for a denial"
              placeholder="Optional message for a denial"
              maxLength={1000}
              value={denial}
              disabled={answering.busy || answering.sent}
              onChange={(e) => setDenial(e.target.value)}
            />
          </div>
          <Feedback sent={answering.sent} message={answering.message} />
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
  /** The request's ID (`Q-31`), shown on the open card. */
  requestId: string;
  questions: QuestionRecord[];
  answer: ChatItemRecord | null;
  readOnly: boolean;
  onAnswer: AnswerFn;
}) {
  const { request, requestId, questions, answer, readOnly, onAnswer } = props;
  const outcome = answer && answer.body.kind === "question_answer" ? answer.body : null;
  const answering = useAnswering(onAnswer, request.seq);
  const [chosen, setChosen] = useState<Record<number, string[]>>({});
  const [other, setOther] = useState<Record<number, string>>({});

  // One single-select question: a press on an option is the answer. Anything else: toggles and one Send.
  const first = questions[0];
  const simple = questions.length === 1 && first !== undefined && !first.multi_select;
  const answers = questions.map((_, i) => joined(chosen[i] ?? [], other[i] ?? ""));
  const complete = answers.every((a) => a !== "");
  const locked = answering.busy || answering.sent;

  function send(values: string[]) {
    const value: Record<string, string> = {};
    questions.forEach((q, i) => {
      value[q.text] = values[i] ?? "";
    });
    void answering.answer({ type: "answers", value });
  }

  function choose(index: number, q: QuestionRecord, label: string) {
    if (simple) {
      send([label]);
      return;
    }
    if (q.multi_select) {
      setChosen((c) => {
        const now = c[index] ?? [];
        return { ...c, [index]: now.includes(label) ? now.filter((l) => l !== label) : [...now, label] };
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

  const eyebrow = outcome
    ? outcome.outcome === "answered"
      ? `Answered · ${timeText(answer?.created_at ?? request.created_at)}`
      : null
    : readOnly
      ? null
      : `Needs you · since ${timeText(request.created_at)} · ${requestId}`;

  return (
    <article
      className={`entry chat-card question-card${outcome === null && !readOnly ? " question-open" : ""}`}
      aria-label="Question"
      data-seq={request.seq}
    >
      {eyebrow ? (
        <div className="card-head">
          <Icon name={outcome ? "check" : "chat"} size={14} />
          <span className="card-eyebrow">{eyebrow}</span>
        </div>
      ) : null}
      {questions.map((q, i) => (
        <fieldset key={q.text} className="q-block">
          <legend>
            {q.header ? <Chip>{q.header}</Chip> : null}
            <span className="question-text">{q.text}</span>
          </legend>
          {outcome ? (
            outcome.outcome === "answered" ? (
              <p className="question-answer">
                <strong>{outcome.answers?.[q.text] ?? ""}</strong>
              </p>
            ) : null
          ) : readOnly ? null : (
            <>
              <div className="option-list">
                {q.options.map((o, n) => (
                  <Button
                    key={o.label}
                    disabled={locked}
                    aria-pressed={simple ? undefined : (chosen[i] ?? []).includes(o.label)}
                    onClick={() => choose(i, q, o.label)}
                  >
                    <span className="opt-num">{n + 1}</span>
                    <span className="opt-text">
                      <span className="opt-label">{o.label}</span>
                      {o.description ? <span className="opt-desc">{o.description}</span> : null}
                    </span>
                  </Button>
                ))}
              </div>
              <div className="question-other">
                <span className="muted">Or type a different answer:</span>
                <Input
                  type="text"
                  aria-label="Other answer"
                  disabled={locked}
                  value={other[i] ?? ""}
                  onChange={(e) => type(i, q, e.target.value)}
                />
                {simple ? (
                  <Button
                    aria-label="Send answer"
                    disabled={locked || answers[0] === ""}
                    onClick={() => send(answers)}
                  >
                    Send
                  </Button>
                ) : null}
              </div>
            </>
          )}
        </fieldset>
      ))}
      {outcome ? (
        <Outcome outcome={outcome.outcome} reason={outcome.reason} answered={null} />
      ) : readOnly ? null : (
        <>
          {simple ? null : (
            <div className="card-actions">
              <Button aria-label="Send answer" variant="primary" disabled={!complete || locked} onClick={() => send(answers)}>
                Send
              </Button>
            </div>
          )}
          <Feedback sent={answering.sent} message={answering.message} />
        </>
      )}
    </article>
  );
}
