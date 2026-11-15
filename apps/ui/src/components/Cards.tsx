// The approval and question cards (C2, M3, 11a). A request is pending while no held item answers it, so a
// reload, a reopened stream and a reconnect show the same pending cards. A pending card has no time
// limit and no timer, and it answers nothing by itself: only the operator's press sends an answer.
//
// A question with one single-select question shows one button per option, and one press sends that
// answer (O2). A typed answer to it goes in the composer, as 11a says ("Or type a different answer below"):
// the card has no field and no Send for it (ruling P4, `composer-answer.test.tsx`). A request with more than
// one question, or a multi-select question, shows each option as a toggle, its own "Other answer" field
// and one Send on the card (no frame draws it; G8). The request's ID is a chip at the right of the head: it
// is `NotBuilt`, "Copy as CLI command" (KL-11).
import type { ChatAnswerArg, ChatItemBody, ChatItemRecord, QuestionRecord } from "@pulse-factory/api-client";
import { useState } from "react";
import { timeText } from "./chat-format";
import { ToolText } from "./ToolText";
import { Button, Chip, Icon, Input, NotBuilt } from "../ui";

/**
 * Sends one answer for the request at `seq`. It resolves to the text to show on the card (a
 * refusal, or the note of an answer that could not act), or `null` when the answer went through.
 */
export type AnswerFn = (seq: number, answer: ChatAnswerArg) => Promise<string | null>;

type Approval = Extract<ChatItemBody, { kind: "approval_request" }>;

/** The one open single-question card of a chat, as the composer needs it to send a typed answer (P4). */
export interface OpenQuestion {
  /** The request item's `seq`: the answer goes to it. */
  seq: number;
  /** The question's text: the key of the answer. */
  text: string;
}

/**
 * The chat's open single-question card, or `null`. A card is open while no `question_answer` item (of any
 * outcome) answers its request. It counts when it has one question and that question is single-select. With
 * no such card, or with more than one, there is no target: Send is a plain send.
 */
export function openSingleQuestion(items: ChatItemRecord[]): OpenQuestion | null {
  const closed = new Set<number>();
  for (const it of items) {
    if (it.body.kind === "question_answer" && it.answers_seq !== null) closed.add(it.answers_seq);
  }
  const open: OpenQuestion[] = [];
  for (const it of items) {
    if (it.body.kind !== "question_request" || closed.has(it.seq)) continue;
    const [only, ...rest] = it.body.items;
    if (only !== undefined && rest.length === 0 && !only.multi_select) open.push({ seq: it.seq, text: only.text });
  }
  return open.length === 1 ? (open[0] ?? null) : null;
}

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

  const open = outcome === null && !readOnly;
  const answeredHead =
    outcome && outcome.outcome === "answered" ? `Answered · ${timeText(answer?.created_at ?? request.created_at)}` : null;

  return (
    <article
      className={`entry chat-card question-card${outcome === null && !readOnly ? " question-open" : ""}`}
      aria-label="Question"
      data-seq={request.seq}
    >
      {answeredHead ? (
        <div className="card-head">
          <Icon name="check" size={14} />
          <span className="card-eyebrow">{answeredHead}</span>
        </div>
      ) : null}
      {open ? (
        <div className="card-head">
          <Icon name="chat" size={14} />
          <span className="card-eyebrow">Needs you</span>
          <span className="card-since">{`since ${timeText(request.created_at)}`}</span>
          <span className="head-spacer" />
          <span className="request-chip" title="Copy as CLI command">
            <NotBuilt as="button" label={requestId} icon="copy" />
          </span>
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
              {simple ? (
                <p className="question-hint">Or type a different answer below. You can also answer from your phone or the CLI.</p>
              ) : (
                <div className="question-other">
                  <span className="muted">Or type another answer:</span>
                  <Input
                    type="text"
                    aria-label="Other answer"
                    disabled={locked}
                    value={other[i] ?? ""}
                    onChange={(e) => type(i, q, e.target.value)}
                  />
                </div>
              )}
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
