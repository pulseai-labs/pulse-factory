// The Settings page: the server section, the same one the server menu (3b) shows, and the Chat policies
// (r1.s4.w3, W11): each setting as a chosen project and agent read it, the scope its value comes from,
// the values it masks, and a set or unset at that scope.
//
// The page names no key. It renders the records `settings_get` returns, and picks the editor by the type
// of `built_in`: a number field for a number, one pattern per line for a list. A key the server adds later
// shows with no change here. The page reads when it opens, when a select changes (an operator action) and
// once when the link comes back; it sets no timer.
import type {
  ApiStatus,
  ProjectRecord,
  SessionList,
  SettingRecord,
  SettingScope,
  SettingValue,
} from "@pulse-factory/api-client";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useCommandId } from "../components/command-id";
import { ServerSection } from "../components/ServerSection";
import { agentName, useChat } from "../state/chat";
import { asTransportError, isDownKind, useConnection } from "../state/connection";

/** The project and the agent a read or a write names. `null` is "all". */
interface Context {
  project: string | null;
  agent: string | null;
}

function scopeOfContext(ctx: Context): SettingScope {
  const kind = ctx.project && ctx.agent ? "project_agent" : ctx.project ? "project" : ctx.agent ? "agent" : "server";
  return { kind, project_id: ctx.project, agent_id: ctx.agent };
}

function contextArgs(ctx: Context): { project_id?: string; agent_id?: string } {
  return {
    ...(ctx.project ? { project_id: ctx.project } : {}),
    ...(ctx.agent ? { agent_id: ctx.agent } : {}),
  };
}

function valueText(value: SettingValue): string {
  return typeof value === "number" ? String(value) : value.join(", ");
}

/** A scope in words: `the server default`, `project <name>`, `agent <name>` or `agent <name> in project <name>`. */
function scopeWords(scope: SettingScope, projects: ProjectRecord[] | null, list: SessionList | null): string {
  const project = () => projects?.find((p) => p.id === scope.project_id)?.name ?? scope.project_id ?? "";
  const agent = () => agentName(list, scope.agent_id ?? "");
  switch (scope.kind) {
    case "server":
      return "the server default";
    case "project":
      return `project ${project()}`;
    case "agent":
      return `agent ${agent()}`;
    case "project_agent":
      return `agent ${agent()} in project ${project()}`;
    default:
      return "built-in";
  }
}

const APPLIES: Record<SettingRecord["applies"], string> = {
  next_turn: "at the next turn",
  next_launch: "at the next launch",
};

/**
 * The value the operator typed, as the key's type wants it; a string is the line to show when it does not
 * parse. A list with no pattern does not parse: a blank field would set an empty list at this scope, and
 * the page does not set one on a click. Clearing this scope's value is Unset.
 */
function parseDraft(record: SettingRecord, draft: string): { value: SettingValue } | { error: string } {
  if (typeof record.built_in === "number") {
    const n = Number(draft);
    if (draft.trim() === "" || !Number.isFinite(n)) return { error: "Enter a number." };
    return { value: n };
  }
  const patterns = draft
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
  if (patterns.length === 0) return { error: "Enter at least one pattern, or use Unset." };
  return { value: patterns };
}

/** What a lost answer leaves to retry: the same call, with the command ID it already has. */
type Retry = { op: "set"; value: SettingValue } | { op: "unset" };

function SettingRow(props: {
  record: SettingRecord;
  ctx: Context;
  words: (scope: SettingScope) => string;
  onChanged(ctx: Context, record: SettingRecord): void;
}) {
  const { record, ctx, words, onChanged } = props;
  const { call } = useConnection();
  const ids = useCommandId();
  const fieldId = useId();
  const [draft, setDraft] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [retry, setRetry] = useState<Retry | null>(null);
  const [busy, setBusy] = useState(false);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  // A layer at exactly the chosen scope: the one an unset removes.
  const atThisScope = record.layers.some(
    (l) => l.scope.kind !== "built_in" && (l.scope.project_id ?? null) === ctx.project && (l.scope.agent_id ?? null) === ctx.agent,
  );
  const isNumber = typeof record.built_in === "number";

  const act = useCallback(
    async (what: Retry) => {
      // One operator action, one command ID; a retry after `unreachable` has the same key and so the same ID.
      const key = `${what.op}|${record.key}|${ctx.project ?? ""}|${ctx.agent ?? ""}|${what.op === "set" ? JSON.stringify(what.value) : ""}`;
      const commandId = ids.idFor(key);
      setBusy(true);
      setMessage(null);
      try {
        const changed = await call((t) =>
          what.op === "set"
            ? t.settingsSet({ key: record.key, value: what.value, ...contextArgs(ctx), command_id: commandId })
            : t.settingsUnset({ key: record.key, ...contextArgs(ctx), command_id: commandId }),
        );
        ids.clear();
        if (!live.current) return;
        setRetry(null);
        setDraft("");
        onChanged(ctx, changed.resolved);
      } catch (e) {
        if (!live.current) return;
        const err = asTransportError(e);
        if (err.kind === "unreachable") {
          // The ID stays: the retry is the same action.
          setMessage(err.message);
          setRetry(what);
        } else {
          ids.clear();
          setRetry(null);
          // A down kind is shown by the 3c banner; a refusal is shown on the row.
          if (!isDownKind(err.kind)) setMessage(err.message);
        }
      } finally {
        if (live.current) setBusy(false);
      }
    },
    [call, ctx, ids, onChanged, record.key],
  );

  const set = () => {
    const parsed = parseDraft(record, draft);
    if ("error" in parsed) {
      setMessage(parsed.error);
      return;
    }
    void act({ op: "set", value: parsed.value });
  };

  return (
    <div className="entry-card" role="group" aria-label={`Setting ${record.key}`}>
      <p className="mono">
        <strong>{record.key}</strong>
      </p>
      <dl className="facts">
        <dt>Value</dt>
        <dd>
          {typeof record.value === "number" ? (
            <span className="mono">{record.value}</span>
          ) : (
            <pre className="mono">{record.value.join("\n")}</pre>
          )}
        </dd>
        <dt>From</dt>
        <dd>{record.from.kind === "built_in" ? "built-in" : words(record.from)}</dd>
        <dt>Applies</dt>
        <dd>{APPLIES[record.applies]}</dd>
      </dl>
      {record.masked.length > 0 ? (
        <ul className="muted" aria-label={`Values masked for ${record.key}`}>
          {record.masked.map((m, i) => (
            <li key={i}>{`masks ${words(m.scope)} ${valueText(m.value)}`}</li>
          ))}
        </ul>
      ) : null}
      <label htmlFor={fieldId}>New value</label>
      {isNumber ? (
        <input
          id={fieldId}
          className="input mono"
          type="number"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
      ) : (
        <textarea
          id={fieldId}
          className="input mono"
          rows={3}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
      )}
      <div className="card-actions">
        <button className="btn btn-primary" type="button" disabled={busy} onClick={set}>
          Set at this scope
        </button>
        {atThisScope ? (
          <button className="btn" type="button" disabled={busy} onClick={() => void act({ op: "unset" })}>
            Unset at this scope
          </button>
        ) : null}
      </div>
      {message !== null ? (
        <p className="row-message" role="alert">
          <span>{message}</span>
          {retry !== null ? (
            <button className="btn" type="button" disabled={busy} onClick={() => void act(retry)}>
              Retry
            </button>
          ) : null}
        </p>
      ) : null}
    </div>
  );
}

function ChatPolicies() {
  const { call, epoch, projects } = useConnection();
  const { list, ensureList } = useChat();
  const projectId = useId();
  const agentId = useId();
  const [ctx, setCtx] = useState<Context>({ project: null, agent: null });
  const [rows, setRows] = useState<SettingRecord[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const live = useRef(true);
  // A read that is out when the context changes must not fill the new context's rows.
  const seq = useRef(0);
  const current = useRef(ctx);
  current.current = ctx;

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    const n = (seq.current += 1);
    try {
      const view = await call((t) => t.settingsGet(contextArgs(ctx)));
      if (!live.current || n !== seq.current) return;
      setRows(view.settings);
      setError(null);
    } catch (e) {
      if (!live.current || n !== seq.current) return;
      const err = asTransportError(e);
      // A disconnect shows the 3c banner; any other failure is one error line.
      if (!isDownKind(err.kind)) setError(err.message);
    }
  }, [call, ctx]);

  // On opening the page, on a select change (the context changes `load`), and once when the link comes
  // back. Never on a timer.
  useEffect(() => {
    void load();
  }, [load, epoch]);

  // The agents are in the session list: read it once if it is not loaded.
  useEffect(() => {
    void ensureList();
  }, [ensureList]);

  const choose = (next: Context) => {
    // The rows of the old context are not the new context's: drop them until the new read arrives.
    setRows(null);
    setError(null);
    setCtx(next);
  };

  const words = useCallback((scope: SettingScope) => scopeWords(scope, projects, list), [projects, list]);

  const onChanged = useCallback((at: Context, record: SettingRecord) => {
    // The answer replaces the row, unless the context moved on while it was out.
    if (at.project !== current.current.project || at.agent !== current.current.agent) return;
    setRows((rs) => (rs === null ? rs : rs.map((r) => (r.key === record.key ? record : r))));
  }, []);

  return (
    <section className="settings-block" aria-labelledby="settings-chat-policies">
      <h3 id="settings-chat-policies">Chat policies</h3>
      <div className="card-actions">
        <label htmlFor={projectId}>Project</label>
        <select
          id={projectId}
          className="select"
          value={ctx.project ?? ""}
          onChange={(e) => choose({ ...ctx, project: e.target.value === "" ? null : e.target.value })}
        >
          <option value="">All projects</option>
          {(projects ?? []).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <label htmlFor={agentId}>Agent</label>
        <select
          id={agentId}
          className="select"
          value={ctx.agent ?? ""}
          onChange={(e) => choose({ ...ctx, agent: e.target.value === "" ? null : e.target.value })}
        >
          <option value="">All agents</option>
          {(list?.agents ?? []).map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </select>
      </div>
      <p className="muted">{`Scope: ${words(scopeOfContext(ctx))}`}</p>
      {error !== null ? (
        <p className="page-error" role="alert">
          <span>{error}</span>
          <button className="btn" type="button" onClick={() => void load()}>
            Retry now
          </button>
        </p>
      ) : null}
      {rows === null ? (
        error === null ? (
          <p className="empty">Reading the settings.</p>
        ) : null
      ) : (
        rows.map((r) => <SettingRow key={r.key} record={r} ctx={ctx} words={words} onChanged={onChanged} />)
      )}
    </section>
  );
}

export function Settings() {
  const { call, epoch, server } = useConnection();
  const [status, setStatus] = useState<ApiStatus | null>(null);
  const [pageError, setPageError] = useState<string | null>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const s = await call((t) => t.status());
      if (live.current) {
        setStatus(s);
        setPageError(null);
      }
    } catch (e) {
      const err = asTransportError(e);
      if (live.current) setPageError(isDownKind(err.kind) ? null : err.message);
    }
  }, [call]);

  // On opening the page, and once when the link comes back. Never on a timer.
  useEffect(() => {
    void load();
  }, [load, epoch]);

  return (
    <section className="page" aria-labelledby="settings-title">
      <div className="page-head">
        <h2 id="settings-title">Settings</h2>
      </div>
      {pageError ? (
        <p className="page-error" role="alert">
          <span>{pageError}</span>
          <button className="btn" type="button" onClick={() => void load()}>
            Retry now
          </button>
        </p>
      ) : null}
      <section className="settings-block" aria-labelledby="settings-server">
        <h3 id="settings-server">Server</h3>
        <ServerSection />
        <dl className="facts">
          <dt>This device</dt>
          <dd>{`${server.device.name} (${server.device.id})`}</dd>
          {status ? (
            <>
              <dt>Server version</dt>
              <dd className="mono">{status.version}</dd>
            </>
          ) : null}
        </dl>
      </section>
      <ChatPolicies />
    </section>
  );
}
