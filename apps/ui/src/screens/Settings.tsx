// The Settings page (11k, r1.s5.w4): the Server block (the server, this Mac, Disconnect with its in-place
// confirm), then "Chat policies": a scope chooser (the whole server, a project, an agent in a project) and one
// block for each setting the server returns, with the value in force, where it comes from (the chain from the
// built-in value to the chosen scope), a change at that scope and "Remove this override".
//
// The page names no key. It renders the records `settings_get` returns, and picks the editor by the type of
// `built_in`: chips with an add field for a list, − / a number field / + for a number. A key the server adds
// later shows with no change here. The page reads when it opens, when the chosen scope changes (an operator
// action) and once when the link comes back; it sets no timer.
import type {
  ApiStatus,
  ProjectRecord,
  SessionList,
  SettingRecord,
  SettingScope,
  SettingValue,
} from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { useCommandId } from "../components/command-id";
import { Column } from "../shell/Column";
import { agentName, useChat } from "../state/chat";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { Avatar, Button, Icon, agentLook } from "../ui";
import "../styles/settings.css";

/** The project and the agent a read or a write names. `null` is "all". */
interface Context {
  project: string | null;
  agent: string | null;
}

type Level = "server" | "project" | "agent";

function contextArgs(ctx: Context): { project_id?: string; agent_id?: string } {
  return {
    ...(ctx.project ? { project_id: ctx.project } : {}),
    ...(ctx.agent ? { agent_id: ctx.agent } : {}),
  };
}

/** A value in a chain link: a number as it is, a list as its length. */
function shortValue(value: SettingValue): string {
  return typeof value === "number" ? String(value) : `${value.length} ${value.length === 1 ? "entry" : "entries"}`;
}

/** The title of a block: the last segment of its key, humanised. The key itself shows beside it. */
function titleOf(key: string): string {
  const last = key.split(".").pop() ?? key;
  const words = last.replaceAll("_", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const APPLIES: Record<SettingRecord["applies"], string> = {
  next_turn: "Applies at the next turn",
  next_launch: "Applies at the next launch",
};

/** What a lost answer leaves to retry: the same call, with the command ID it already has. */
type Retry = { op: "set"; value: SettingValue } | { op: "unset" };

/** One link of the chain "Comes from": the scope it names, and how it reads. */
interface Link {
  label: string;
  scope: SettingScope;
}

function sameScope(a: SettingScope, b: SettingScope): boolean {
  return a.kind === b.kind && (a.project_id ?? null) === (b.project_id ?? null) && (a.agent_id ?? null) === (b.agent_id ?? null);
}

/** The links a setting can come from at this scope, from the built-in value to the most specific. */
function chainOf(ctx: Context, projects: ProjectRecord[] | null, list: SessionList | null): Link[] {
  const project = ctx.project === null ? "" : (projects?.find((p) => p.id === ctx.project)?.name ?? ctx.project);
  const agent = ctx.agent === null ? "" : agentName(list, ctx.agent);
  const links: Link[] = [
    { label: "Built-in", scope: { kind: "built_in", project_id: null, agent_id: null } },
    { label: "Server default", scope: { kind: "server", project_id: null, agent_id: null } },
  ];
  if (ctx.project) links.push({ label: project, scope: { kind: "project", project_id: ctx.project, agent_id: null } });
  if (ctx.agent) links.push({ label: agent, scope: { kind: "agent", project_id: null, agent_id: ctx.agent } });
  if (ctx.project && ctx.agent) links.push({ label: `${agent} in ${project}`, scope: { kind: "project_agent", project_id: ctx.project, agent_id: ctx.agent } });
  return links;
}

function ChainRow(props: { record: SettingRecord; links: Link[]; scopeLabel: string; here: boolean; canRemove: boolean; busy: boolean; onRemove(): void }) {
  const { record, links, scopeLabel, here, canRemove, busy, onRemove } = props;
  const valueAt = (link: Link): SettingValue | null => {
    if (link.scope.kind === "built_in") return record.built_in;
    return record.layers.find((l) => sameScope(l.scope, link.scope))?.value ?? null;
  };
  const liveLabel = links.find((l) => sameScope(l.scope, record.from))?.label ?? "Built-in";
  // The link before the chosen scope that holds a value: where the value goes back to on a removal.
  const heldBefore = [...links.slice(0, -1)].reverse().find((l) => valueAt(l) !== null)?.label ?? "Built-in";
  const note = !here
    ? `In force from ${liveLabel}. A change here applies only to ${scopeLabel}.`
    : canRemove
      ? `Set for ${scopeLabel}. Remove the override and ${heldBefore}'s value applies again.`
      : "The server default. Projects and agents use it unless they set their own.";
  return (
    <>
      <div className="st-chain-row">
        <span className="st-chain-label">Comes from</span>
        <ul className="st-chain" aria-label="Comes from">
          {links.map((link, i) => {
            const value = valueAt(link);
            const current = sameScope(link.scope, record.from);
            return (
              <li key={`${link.scope.kind}:${i}`} className="st-chain-item" aria-current={current ? "true" : undefined}>
                {i > 0 ? (
                  <span className="st-chain-sep" aria-hidden="true">
                    <Icon name="chevron-right" size={12} />
                  </span>
                ) : null}
                <span className={`st-node${current ? " st-node-live" : ""}${value === null ? " st-node-inherits" : ""}`} title={value === null ? "Uses the value before it" : "Set at this level"}>
                  <span className="st-node-dot" aria-hidden="true" />
                  <span className="st-node-label">{link.label}</span>
                  <span className="st-node-value">{value === null ? "inherits" : shortValue(value)}</span>
                </span>
              </li>
            );
          })}
        </ul>
        <span className="st-spacer" />
        {canRemove ? (
          <Button variant="ghost" size="sm" icon="x" disabled={busy} onClick={onRemove}>
            Remove this override
          </Button>
        ) : null}
      </div>
      <p className="st-note">{note}</p>
      <p className="st-applies">{APPLIES[record.applies]}</p>
    </>
  );
}

function PolicyBlock(props: {
  record: SettingRecord;
  ctx: Context;
  links: Link[];
  scopeLabel: string;
  onChanged(ctx: Context, record: SettingRecord): void;
}) {
  const { record, ctx, links, scopeLabel, onChanged } = props;
  const { call } = useConnection();
  const ids = useCommandId();
  const [message, setMessage] = useState<string | null>(null);
  const [retry, setRetry] = useState<Retry | null>(null);
  const [busy, setBusy] = useState(false);
  const [numberDraft, setNumberDraft] = useState(typeof record.value === "number" ? String(record.value) : "");
  const [addDraft, setAddDraft] = useState("");
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  // The field follows the value in force: after an answer, a scope change or a removal.
  useEffect(() => {
    if (typeof record.value === "number") setNumberDraft(String(record.value));
  }, [record.value]);

  // A layer at exactly the chosen scope: the one a removal deletes.
  const here = ctx.project === null && ctx.agent === null;
  const atThisScope = record.layers.some(
    (l) => l.scope.kind !== "built_in" && (l.scope.project_id ?? null) === ctx.project && (l.scope.agent_id ?? null) === ctx.agent,
  );

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
        setAddDraft("");
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
          // A down kind is shown by the 3c banner; a refusal is shown on the block.
          if (!isDownKind(err.kind)) setMessage(err.message);
        }
      } finally {
        if (live.current) setBusy(false);
      }
    },
    [call, ctx, ids, onChanged, record.key],
  );

  // A typed number: one set on Enter or on leaving the field, none when it did not change.
  const commit = () => {
    if (typeof record.value !== "number") return;
    const n = Number(numberDraft);
    if (numberDraft.trim() === "" || !Number.isFinite(n)) {
      setMessage("Enter a number.");
      return;
    }
    if (n === record.value) return;
    void act({ op: "set", value: n });
  };

  const step = (by: 1 | -1) => {
    if (typeof record.value !== "number") return;
    const next = Math.max(0, record.value + by);
    if (next === record.value) return;
    void act({ op: "set", value: next });
  };

  const removeChip = (pattern: string) => {
    if (typeof record.value === "number") return;
    const next = record.value.filter((p) => p !== pattern);
    if (next.length === 0) {
      // PR #49: no empty list on a click.
      setMessage("Use Remove this override to go back to the wider list.");
      return;
    }
    void act({ op: "set", value: next });
  };

  const addChip = () => {
    if (typeof record.value === "number") return;
    const pattern = addDraft.trim();
    if (pattern === "" || record.value.includes(pattern)) return;
    void act({ op: "set", value: [...record.value, pattern] });
  };

  const isNumber = typeof record.built_in === "number";
  return (
    <div className="st-policy" role="group" aria-label={`Setting ${record.key}`}>
      <div className="st-policy-head">
        <span className="st-policy-title">{titleOf(record.key)}</span>
        <span className="st-policy-key">{record.key}</span>
      </div>
      {isNumber ? (
        <div className="st-stepper-row">
          <div className="st-stepper">
            <button type="button" className="st-step" aria-label="Fewer" disabled={busy} onClick={() => step(-1)}>
              <Icon name="minus" size={15} />
            </button>
            <input
              className="st-number"
              type="number"
              aria-label="Value"
              value={numberDraft}
              disabled={busy}
              onChange={(e) => setNumberDraft(e.target.value)}
              onBlur={commit}
              onKeyDown={(e) => {
                if (e.key === "Enter") commit();
              }}
            />
            <button type="button" className="st-step" aria-label="More" disabled={busy} onClick={() => step(1)}>
              <Icon name="plus" size={15} />
            </button>
          </div>
        </div>
      ) : typeof record.value === "number" ? null : (
        <>
          <p className="st-sentence">{`${record.value.length} ${record.value.length === 1 ? "entry applies" : "entries apply"} here.`}</p>
          <div className="st-chips">
            {record.value.map((pattern) => (
              <span key={pattern} className="st-chip">
                {pattern}
                <button type="button" className="st-chip-x" aria-label={`Remove ${pattern}`} title={`Allow ${pattern} again`} disabled={busy} onClick={() => removeChip(pattern)}>
                  <Icon name="x" size={12} />
                </button>
              </span>
            ))}
            <span className="st-add">
              <input
                className="st-add-field"
                aria-label="Add a pattern"
                placeholder="Add a pattern"
                value={addDraft}
                disabled={busy}
                onChange={(e) => setAddDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") addChip();
                }}
              />
              <button type="button" className="st-chip-x" aria-label="Add" disabled={busy} onClick={addChip}>
                <Icon name="plus" size={13} />
              </button>
            </span>
          </div>
          <p className="st-note">{"A project's or agent's own list replaces the wider list. It doesn't add to it."}</p>
        </>
      )}
      <ChainRow record={record} links={links} scopeLabel={scopeLabel} here={here} canRemove={atThisScope} busy={busy} onRemove={() => void act({ op: "unset" })} />
      {message !== null ? (
        <p className="st-message" role="alert">
          <span>{message}</span>
          {retry !== null ? (
            <Button size="sm" disabled={busy} onClick={() => void act(retry)}>
              Retry
            </Button>
          ) : null}
        </p>
      ) : null}
    </div>
  );
}

function Pill(props: { on: boolean; onClick(): void; children: string; avatar?: string }) {
  return (
    <button type="button" className={`st-pill${props.on ? " st-pill-on" : ""}${props.avatar ? " st-pill-agent" : ""}`} aria-pressed={props.on} onClick={props.onClick}>
      {props.avatar ? <Avatar agent={agentLook(props.avatar)} size={22} /> : null}
      {props.children}
    </button>
  );
}

function ChatPolicies() {
  const { call, epoch, projects } = useConnection();
  const { list, ensureList } = useChat();
  const [level, setLevel] = useState<Level>("server");
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

  // On opening the page, on a scope change (the context changes `load`), and once when the link comes
  // back. Never on a timer.
  useEffect(() => {
    void load();
  }, [load, epoch]);

  // The agents are in the session list: read it once if it is not loaded.
  useEffect(() => {
    void ensureList();
  }, [ensureList]);

  const agents = list?.agents ?? [];
  // "An agent in a project" starts on the first agent once the list is there.
  useEffect(() => {
    if (level === "agent" && ctx.agent === null && agents.length > 0) setCtx((c) => ({ ...c, agent: agents[0]?.id ?? null }));
  }, [level, ctx.agent, agents]);

  const choose = (nextLevel: Level, next: Context) => {
    // The rows of the old scope are not the new scope's: drop them until the new read arrives.
    if (next.project !== ctx.project || next.agent !== ctx.agent) {
      setRows(null);
      setError(null);
    }
    setLevel(nextLevel);
    setCtx(next);
  };

  const firstProject = projects?.[0]?.id ?? null;
  const pickLevel = (nextLevel: Level) => {
    if (nextLevel === "server") choose("server", { project: null, agent: null });
    else if (nextLevel === "project") choose("project", { project: ctx.project ?? firstProject, agent: null });
    else choose("agent", { project: ctx.project ?? firstProject, agent: ctx.agent ?? agents[0]?.id ?? null });
  };

  const onChanged = useCallback((at: Context, record: SettingRecord) => {
    // The answer replaces the block, unless the context moved on while it was out.
    if (at.project !== current.current.project || at.agent !== current.current.agent) return;
    setRows((rs) => (rs === null ? rs : rs.map((r) => (r.key === record.key ? record : r))));
  }, []);

  const projectName = (id: string | null) => (id === null ? "" : (projects?.find((p) => p.id === id)?.name ?? id));
  const scopeLabel =
    ctx.agent !== null
      ? ctx.project !== null
        ? `${agentName(list, ctx.agent)} in ${projectName(ctx.project)}`
        : `${agentName(list, ctx.agent)} in every project`
      : ctx.project !== null
        ? `every agent in ${projectName(ctx.project)}`
        : "the whole server";
  const links = chainOf(ctx, projects, list);

  return (
    <section className="st-section" aria-labelledby="settings-chat-policies">
      <div className="st-section-head">
        <h3 id="settings-chat-policies" className="st-section-title">
          Chat policies
        </h3>
        <p className="st-lead">Three rules every chat follows. Set them once for the server, then make an exception for a project, or for one agent in a project.</p>
      </div>
      <div className="st-scope">
        <div className="st-scope-row" role="group" aria-label="Show the rules for">
          <span className="st-scope-label">Show the rules for</span>
          <Pill on={level === "server"} onClick={() => pickLevel("server")}>
            The whole server
          </Pill>
          <Pill on={level === "project"} onClick={() => pickLevel("project")}>
            A project
          </Pill>
          <Pill on={level === "agent"} onClick={() => pickLevel("agent")}>
            An agent in a project
          </Pill>
        </div>
        {level !== "server" ? (
          <div className="st-scope-row" role="group" aria-label="Project">
            <span className="st-scope-name">Project</span>
            {level === "agent" ? (
              <Pill on={ctx.project === null} onClick={() => choose("agent", { ...ctx, project: null })}>
                All projects
              </Pill>
            ) : null}
            {(projects ?? []).map((p) => (
              <Pill key={p.id} on={ctx.project === p.id} onClick={() => choose(level, { ...ctx, project: p.id })}>
                {p.name}
              </Pill>
            ))}
          </div>
        ) : null}
        {level === "agent" ? (
          <div className="st-scope-row" role="group" aria-label="Agent">
            <span className="st-scope-name">Agent</span>
            {agents.map((a) => (
              <Pill key={a.id} avatar={a.id} on={ctx.agent === a.id} onClick={() => choose("agent", { ...ctx, agent: a.id })}>
                {a.name}
              </Pill>
            ))}
          </div>
        ) : null}
        <p className="st-applies-to">
          <Icon name="sliders" size={14} />
          Changes below apply to <b>{scopeLabel}</b>
        </p>
      </div>
      {error !== null ? (
        <p className="page-error" role="alert">
          <span>{error}</span>
          <Button onClick={() => void load()}>Retry now</Button>
        </p>
      ) : null}
      {rows === null ? (
        error === null ? (
          <p className="st-empty">Reading the settings.</p>
        ) : null
      ) : (
        rows.map((r) => <PolicyBlock key={r.key} record={r} ctx={ctx} links={links} scopeLabel={scopeLabel} onChanged={onChanged} />)
      )}
    </section>
  );
}

function ServerBlock(props: { status: ApiStatus | null }) {
  const conn = useConnection();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const up = conn.link.state === "up";
  const name = conn.server.server?.name ?? props.status?.server?.name ?? conn.server.address;
  const ms = conn.lastCallMs === null ? "no call yet" : `${conn.lastCallMs} ms`;

  async function disconnect() {
    setBusy(true);
    setError(null);
    try {
      await conn.forget();
    } catch (e) {
      setError(e instanceof Error ? e.message : "could not disconnect");
      setBusy(false);
    }
  }

  return (
    <section className="st-section" aria-labelledby="settings-server">
      <h3 id="settings-server" className="st-section-title">
        Server
      </h3>
      <div className="st-server">
        <div className="st-card st-card-server">
          <div className="st-server-head">
            <span className="st-tile st-tile-mint">
              <Icon name="layers" size={18} />
            </span>
            <div className="st-server-id">
              <span className="st-server-name">{name}</span>
              <span className="st-server-state">
                <span className={`st-dot${up ? " st-dot-up" : ""}`} aria-hidden="true" />
                {`${up ? "Connected" : "Not connected"} · ${ms}`}
              </span>
            </div>
          </div>
          <div className="st-fields">
            <div className="st-field">
              <span className="st-field-label">Address</span>
              <span className="st-field-value">{conn.server.address}</span>
            </div>
            <div className="st-field">
              <span className="st-field-label">Server version</span>
              <span className="st-field-value">{props.status ? `pulse-factory ${props.status.version}` : ""}</span>
            </div>
          </div>
        </div>
        <div className="st-card st-card-device">
          <div className="st-server-head">
            <span className="st-tile">
              <Icon name="settings" size={16} />
            </span>
            <div className="st-server-id">
              <span className="st-field-label">This device</span>
              <span className="st-device-name">{conn.server.device.name}</span>
            </div>
          </div>
          <div className="st-field">
            <span className="st-field-label">Device ID</span>
            <span className="st-field-value">{conn.server.device.id}</span>
          </div>
          <span className="st-spacer" />
          {confirming ? (
            <div className="st-confirm" role="alertdialog" aria-label="Disconnect this Mac">
              <span className="st-confirm-title">{`Disconnect from ${name}?`}</span>
              <span className="st-confirm-text">Sessions keep running. Pair again with a new code to come back.</span>
              <div className="st-confirm-actions">
                <Button size="sm" disabled={busy} onClick={() => setConfirming(false)}>
                  Cancel
                </Button>
                <Button variant="danger" size="sm" disabled={busy} onClick={() => void disconnect()}>
                  Disconnect
                </Button>
              </div>
            </div>
          ) : (
            <Button variant="danger" disabled={busy} onClick={() => setConfirming(true)}>
              Disconnect this Mac
            </Button>
          )}
          {error ? (
            <p className="problem-title" role="alert">
              {error}
            </p>
          ) : null}
        </div>
      </div>
    </section>
  );
}

export function Settings() {
  const { call, epoch } = useConnection();
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
    <Column max={860}>
      <div className="st-page">
        <div className="st-title-block">
          <h2 className="st-title">Settings</h2>
          <p className="st-subtitle">Your server, this Mac, and the rules chats follow.</p>
        </div>
        {pageError ? (
          <p className="page-error" role="alert">
            <span>{pageError}</span>
            <Button onClick={() => void load()}>Retry now</Button>
          </p>
        ) : null}
        <ServerBlock status={status} />
        <ChatPolicies />
      </div>
    </Column>
  );
}
