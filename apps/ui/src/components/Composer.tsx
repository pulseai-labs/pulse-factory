// The composer (11a): one compact box with the text field (one line, growing with its text) and, under it,
// Attach (not built), the runtime's option controls, the context ring, and Send (a green arrow) or Stop while
// the agent works. The cache line sits under the box and always shows while the session is live. A send
// happens only from the operator's submit (Q6, `seat-launch-spend`). An ended session shows the ended panel
// in its place.
//
// The controls render what the runtime declares (Q5, P2). A change applies to that chat only: it is a
// `sessionSet` of one field, and no code path here calls anything that changes an agent. Each control is as
// wide as its label (NG-4): the native pop-up lies over the icon, the display name and the chevron, invisible.
//
// A typed answer to an open question goes here (ruling P4): while the chat holds exactly one open
// single-question card (`openQuestion`), Send and Enter call the chat's `AnswerFn`, the one the card used,
// and not `sessionSend`.
import type { ChatOptionRecord, ChatSessionRecord, SessionSetArgs } from "@pulse-factory/api-client";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { asTransportError, useConnection } from "../state/connection";
import { useChat } from "../state/chat";
import type { AnswerFn, OpenQuestion } from "./Cards";
import { parseTime } from "./chat-format";
import { newCommandId, useCommandId } from "./command-id";
import { clock } from "./format";
// The option texts are the display names of `ui/display-names.ts` (G3), through the `ui` index.
import { Button, Icon, NotBuilt, effortName, modelName, permissionName, type IconName } from "../ui";

/** 11a's icon before each declared option's text. An option the canvas has no icon for shows none. */
const OPTION_ICONS: Partial<Record<string, IconName>> = { model: "sparkles", effort: "zap", permission_mode: "lock" };

/** The session's value of an option, never the agent's. A pending option shows what was chosen. */
function optionValue(record: ChatSessionRecord, option: ChatOptionRecord): string {
  const pending = record.pending_options[option.id];
  if (pending !== undefined) return pending;
  switch (option.id) {
    case "model":
      return record.model;
    case "effort":
      return record.effort;
    case "permission_mode":
      return record.permission_mode;
    default:
      return record.runtime_options[option.id] ?? option.default;
  }
}

/** The one field of a `sessionSet` for an option. A runtime option goes inside `options`. */
function setArgs(record: ChatSessionRecord, option: ChatOptionRecord, value: string): SessionSetArgs {
  const base = { session_id: record.id, command_id: newCommandId() };
  switch (option.id) {
    case "model":
      return { ...base, model: value };
    case "effort":
      return { ...base, effort: value };
    case "permission_mode":
      return { ...base, permission_mode: value };
    default:
      return { ...base, options: { [option.id]: value } };
  }
}

/** The share of the context window in use, in percent, or `null` when the window is not known. */
function gaugeShare(record: ChatSessionRecord): number | null {
  const context = record.context;
  if (context === null || context.max_tokens <= 0) return null;
  return Math.round((100 * context.used_tokens) / context.max_tokens);
}

/** The ring (r = 6 in a 16 px box) at the share of the window in use, with the percentage beside it. */
const RING_RADIUS = 6;
const RING_LENGTH = 2 * Math.PI * RING_RADIUS;

function ContextGauge(props: { record: ChatSessionRecord }) {
  const share = gaugeShare(props.record);
  const context = props.record.context;
  return (
    <span
      className="context-gauge"
      role="group"
      aria-label="Context gauge"
      title={context && share !== null ? `Context: ${context.used_tokens} of ${context.max_tokens} tokens` : "Context: not known"}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
        <circle className="ring-track" cx="8" cy="8" r={RING_RADIUS} fill="none" strokeWidth="2.5" />
        {share === null ? null : (
          <circle
            className="ring-arc"
            cx="8"
            cy="8"
            r={RING_RADIUS}
            fill="none"
            strokeWidth="2.5"
            strokeDasharray={`${(RING_LENGTH * Math.min(share, 100)) / 100} ${RING_LENGTH}`}
            transform="rotate(-90 8 8)"
          />
        )}
      </svg>
      {share === null ? "—" : `${share}%`}
    </span>
  );
}

/** The text an option shows for a value: the display name of a model, an effort or a permission mode (G3). */
function displayValue(optionId: string, value: string): string {
  switch (optionId) {
    case "model":
      return modelName(value);
    case "effort":
      return effortName(value);
    case "permission_mode":
      return permissionName(value);
    default:
      return value;
  }
}

/** An option whose values are exactly `on` and `off` is a switch. */
function isSwitch(option: ChatOptionRecord): boolean {
  return option.values.length === 2 && option.values.includes("on") && option.values.includes("off");
}

/** A TTL in words, as 11b's title says it: "1 hour", "5 minutes". */
function ttlText(seconds: number): string {
  const plural = (n: number, unit: string) => `${n} ${unit}${n === 1 ? "" : "s"}`;
  if (seconds % 3600 === 0) return plural(seconds / 3600, "hour");
  if (seconds % 60 === 0) return plural(seconds / 60, "minute");
  return plural(seconds, "second");
}

/** A token count as 11b writes it: `124k` from 1000 up. */
function tokensText(tokens: number): string {
  return tokens >= 1000 ? `${Math.round(tokens / 1000)}k` : String(tokens);
}

/**
 * The cache line of a live session: "Cache warm · Nm left" while the last turn's cache is warm, else "Cache
 * cold", and so before the first turn too, when nothing is cached. A display timer ticks the minutes once a
 * minute, only while the cache is warm. It calls nothing. The title is 11b's sentence where the record gives
 * the numbers (the TTL, and for a warm cache the reply time and the context's tokens), else none.
 */
function useCacheLine(record: ChatSessionRecord): { warm: boolean; text: string; title: string | null } {
  const last = parseTime(record.last_turn_at);
  const ttl = record.cache_ttl_seconds;
  const end = last !== null && ttl !== null ? last + ttl * 1000 : null;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
  }, [end]);
  const warm = end !== null && now < end;
  useEffect(() => {
    if (!warm) return;
    const id = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(id);
  }, [warm]);
  if (end === null || ttl === null) return { warm: false, text: "Cache cold", title: null };
  const stays = `Prompt cache stays warm for ${ttlText(ttl)} after the last reply.`;
  const reuse =
    warm && record.context !== null
      ? ` A reply before ${clock(end)} reuses ${tokensText(record.context.used_tokens)} cached tokens.`
      : "";
  return {
    warm,
    text: warm ? `Cache warm · ${Math.ceil((end - now) / 60_000)}m left` : "Cache cold",
    title: stays + reuse,
  };
}

function Controls(props: { record: ChatSessionRecord; options: ChatOptionRecord[] }) {
  const { record, options } = props;
  const { call } = useConnection();
  const chat = useChat();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const ended = record.ended_at !== null;

  async function change(option: ChatOptionRecord, value: string) {
    setBusy(option.id);
    setError(null);
    setNote(null);
    try {
      const out = await call((t) => t.sessionSet(setArgs(record, option, value)));
      chat.applyRecord(out.session);
      if (out.note) setNote(out.note);
    } catch (e) {
      setError(asTransportError(e).message);
    } finally {
      setBusy(null);
    }
  }

  if (options.length === 0) return null;
  return (
    <div className="controls" role="group" aria-label="Chat controls">
      {options.map((option) => {
        const value = optionValue(record, option);
        const id = `pf-option-${record.id}-${option.id}`;
        const pending = option.id in record.pending_options ? <span className="hint">applies when the chat restarts</span> : null;
        if (isSwitch(option)) {
          return (
            <span className="control control-switch" key={option.id}>
              <span className="control-icon">
                <Icon name="layers" size={13} />
              </span>
              <label htmlFor={id}>{option.label}</label>
              <input
                id={id}
                type="checkbox"
                role="switch"
                className="switch"
                checked={value === "on"}
                disabled={ended || busy === option.id}
                onChange={(e) => void change(option, e.target.checked ? "on" : "off")}
              />
              {pending}
            </span>
          );
        }
        // A free option shows its current value even when it is not one of the declared values.
        const values = option.values.includes(value) ? option.values : [value, ...option.values];
        const icon = OPTION_ICONS[option.id];
        return (
          <span className="control" key={option.id}>
            <label className="ui-sr-only" htmlFor={id}>
              {option.label}
            </label>
            <span className="control-face">
              {icon ? (
                <span className={`control-icon${option.id === "model" ? " control-icon-model" : ""}`}>
                  <Icon name={icon} size={13} />
                </span>
              ) : null}
              <span className="control-text">{displayValue(option.id, value)}</span>
              <span className="control-chevron">
                <Icon name="chevron-down" size={12} />
              </span>
              <select
                id={id}
                className="control-field"
                value={value}
                disabled={ended || busy === option.id}
                onChange={(e) => void change(option, e.target.value)}
              >
                {values.map((v) => (
                  <option key={v} value={v}>
                    {displayValue(option.id, v)}
                  </option>
                ))}
              </select>
            </span>
            {pending}
          </span>
        );
      })}
      {error ? (
        <p className="problem-title" role="alert">
          {error}
        </p>
      ) : null}
      {note ? (
        <p className="composer-note" role="status">
          {note}
        </p>
      ) : null}
    </div>
  );
}

export function Composer(props: {
  record: ChatSessionRecord;
  agentName: string;
  /** Start session, in the ended panel: a new session for the same project and agent. */
  onStart: () => void;
  starting: boolean;
  startError: string | null;
  /** The chat's one open single-question card, or `null`: while it is set, Send answers it (P4). */
  openQuestion: OpenQuestion | null;
  /** The chat's answer call: the one the cards use, with its command-ID rules. */
  onAnswer: AnswerFn;
}) {
  const { record, agentName, onStart, starting, startError, openQuestion, onAnswer } = props;
  const { call } = useConnection();
  const chat = useChat();
  const sendId = useCommandId();
  const interruptId = useCommandId();
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ text: string; retry: boolean } | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const field = useRef<HTMLTextAreaElement>(null);

  const ended = record.ended_at !== null;
  const readOnly = ended || record.context_lost !== null;
  const runtime = chat.list?.runtimes.find((r) => r.id === record.runtime);
  const cache = useCacheLine(record);
  const working = record.state === "working";

  // The field starts at one line and grows with its text, up to the CSS `max-height`.
  useLayoutEffect(() => {
    const el = field.current;
    if (!el) return;
    el.style.height = "auto";
    if (el.scrollHeight > 0) el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  /** What Send does, in a sentence, for its title: no line of this text is on the screen (NG-7, G8). */
  const sendTitle = openQuestion
    ? "Answer the open question"
    : working && runtime
      ? runtime.mid_turn === "steer"
        ? "Steers the running turn"
        : "Runs as the next turn"
      : "Send";

  async function send() {
    const text = draft.trim();
    if (text === "" || busy || readOnly) return;
    setBusy(true);
    setError(null);
    setNote(null);
    if (openQuestion !== null) {
      // A typed answer (P4): the card's own call, with its seq and the question's text as the key. The command
      // ID follows the answer, so the same text after `unreachable` is the same answer.
      try {
        const failure = await onAnswer(openQuestion.seq, { type: "answers", value: { [openQuestion.text]: text } });
        if (failure === null) setDraft("");
        else setError({ text: failure, retry: false });
      } finally {
        setBusy(false);
      }
      return;
    }
    // The same text after `unreachable` is the same send: it reuses the command ID (A7).
    const id = sendId.idFor(text);
    try {
      const out = await call((t) => t.sessionSend({ session_id: record.id, text, command_id: id }));
      sendId.clear();
      chat.applyRecord(out.session);
      setDraft("");
      // An admitted send that could not act: its command ID is spent, so the next send gets a new one.
      if (out.note) setNote(out.note);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") sendId.clear();
      setError({ text: err.message, retry: err.kind === "unreachable" });
    } finally {
      setBusy(false);
    }
  }

  async function interrupt() {
    setError(null);
    const id = interruptId.idFor("interrupt");
    try {
      const out = await call((t) => t.sessionInterrupt({ session_id: record.id, command_id: id }));
      interruptId.clear();
      chat.applyRecord(out.session);
      if (out.note) setNote(out.note);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") interruptId.clear();
      setError({ text: err.message, retry: false });
    }
  }

  function onKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  }

  if (ended) {
    return (
      <div className="composer-ended">
        <div className="ended-text">
          <span className="ended-title">This session has ended.</span>
          <span className="ended-note">The chat stays. Start session opens a new chat in the same project.</span>
        </div>
        <Button variant="primary" size="lg" icon="play" disabled={starting} onClick={onStart}>
          Start session
        </Button>
        {startError ? (
          <p className="problem-title" role="alert">
            {startError}
          </p>
        ) : null}
      </div>
    );
  }

  const sendButton = (
    <Button
      variant="primary"
      aria-label="Send"
      title={sendTitle}
      disabled={readOnly || busy || draft.trim() === ""}
      onClick={() => void send()}
    >
      <Icon name="arrow-right" size={16} />
    </Button>
  );
  return (
    <div className="composer">
      <div className="composer-box">
        <label className="ui-sr-only" htmlFor={`pf-message-${record.id}`}>
          {`Message ${agentName}`}
        </label>
        <textarea
          id={`pf-message-${record.id}`}
          ref={field}
          className="composer-input"
          rows={1}
          value={draft}
          disabled={readOnly}
          placeholder={readOnly ? "" : `Message ${agentName}`}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="composer-row">
          <NotBuilt as="button" label="Attach" icon="paperclip" compact />
          <Controls record={record} options={runtime?.options ?? []} />
          <span className="composer-spacer" />
          <ContextGauge record={record} />
          {error?.retry ? (
            <Button disabled={busy} onClick={() => void send()}>
              Retry
            </Button>
          ) : null}
          {working ? (
            <>
              <Button onClick={() => void interrupt()}>
                <span className="stop-glyph" aria-hidden="true" />
                Stop
              </Button>
              {draft.trim() === "" ? null : sendButton}
            </>
          ) : (
            sendButton
          )}
        </div>
      </div>
      <div className="composer-meta">
        <span className="composer-spacer" />
        <span className={`cache-text${cache.warm ? " cache-warm" : ""}`} {...(cache.title === null ? {} : { title: cache.title })}>
          <Icon name="clock" size={12} />
          {cache.text}
        </span>
      </div>
      {error ? (
        <p className="problem-title" role="alert">
          {error.text}
        </p>
      ) : null}
      {note ? (
        <p className="composer-note" role="status">
          {note}
        </p>
      ) : null}
    </div>
  );
}
