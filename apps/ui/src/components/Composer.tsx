// The composer (11a): one compact box with the text field and, under it, a row of the runtime's option
// chips, the context ring, the cache-warm time, and Send (a green arrow) or Stop while the agent works.
// A send happens only from the operator's submit (Q6, `seat-launch-spend`). An ended session shows the
// ended panel in its place.
//
// The controls render what the runtime declares (Q5, P2). A change applies to that chat only: it is a
// `sessionSet` of one field, and no code path here calls anything that changes an agent.
import type { ChatOptionRecord, ChatSessionRecord, SessionSetArgs } from "@pulse-factory/api-client";
import { useEffect, useState, type KeyboardEvent } from "react";
import { asTransportError, useConnection } from "../state/connection";
import { useChat } from "../state/chat";
import { parseTime } from "./chat-format";
import { newCommandId, useCommandId } from "./command-id";
// The option texts are the display names of `ui/display-names.ts` (G3), through the `ui` index.
import { Button, Icon, Select, effortName, modelName, permissionName } from "../ui";

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

/**
 * The cache-warm text from the last turn's time and TTL: nothing before the first turn. A display
 * timer ticks the minutes once a minute, only while the cache is warm. It calls nothing.
 */
function useCacheText(record: ChatSessionRecord): string | null {
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
  if (end === null) return null;
  return warm ? `Cache warm · ${Math.ceil((end - now) / 60_000)}m left` : "Cache cold";
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
        return (
          <span className="control" key={option.id}>
            <label className="ui-sr-only" htmlFor={id}>
              {option.label}
            </label>
            <Select
              id={id}
              value={value}
              disabled={ended || busy === option.id}
              onChange={(e) => void change(option, e.target.value)}
            >
              {values.map((v) => (
                <option key={v} value={v}>
                  {displayValue(option.id, v)}
                </option>
              ))}
            </Select>
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
}) {
  const { record, agentName, onStart, starting, startError } = props;
  const { call } = useConnection();
  const chat = useChat();
  const sendId = useCommandId();
  const interruptId = useCommandId();
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ text: string; retry: boolean } | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const ended = record.ended_at !== null;
  const readOnly = ended || record.context_lost !== null;
  const runtime = chat.list?.runtimes.find((r) => r.id === record.runtime);
  const cache = useCacheText(record);
  const hint =
    record.state !== "working" || !runtime
      ? null
      : runtime.mid_turn === "steer"
        ? "steers the running turn"
        : "runs as the next turn";

  async function send() {
    const text = draft.trim();
    if (text === "" || busy || readOnly) return;
    setBusy(true);
    setError(null);
    setNote(null);
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

  const working = record.state === "working";
  return (
    <div className="composer">
      <div className="composer-box">
        <label className="ui-sr-only" htmlFor={`pf-message-${record.id}`}>
          {`Message ${agentName}`}
        </label>
        <textarea
          id={`pf-message-${record.id}`}
          className="composer-input"
          rows={2}
          value={draft}
          disabled={readOnly}
          placeholder={readOnly ? "" : `Message ${agentName}`}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="composer-row">
          <Controls record={record} options={runtime?.options ?? []} />
          <span className="composer-spacer" />
          <ContextGauge record={record} />
          {error?.retry ? (
            <Button disabled={busy} onClick={() => void send()}>
              Retry
            </Button>
          ) : null}
          {working ? (
            <Button onClick={() => void interrupt()}>
              <span className="stop-glyph" aria-hidden="true" />
              Stop
            </Button>
          ) : (
            <Button
              variant="primary"
              aria-label="Send"
              title="Send"
              disabled={readOnly || busy || draft.trim() === ""}
              onClick={() => void send()}
            >
              <Icon name="arrow-right" size={16} />
            </Button>
          )}
        </div>
      </div>
      <div className="composer-meta">
        {hint ? <span className="hint">{hint}</span> : null}
        <span className="composer-spacer" />
        {cache ? (
          <span className="cache-text">
            <Icon name="clock" size={12} />
            {cache}
          </span>
        ) : null}
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
