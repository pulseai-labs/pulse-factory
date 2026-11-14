// The Agents page (11g, r1.s5.w4; r1.s6.w3): the selected agent's identity band (with the Orchestrator's "One
// session per project" chip), its Persona card, Brain (three tiles: the runtime by its name with "or <the other
// runtimes>", the model with its note, the effort), Skills, Permissions (the three-way control at the position
// nearest the agent's mode, P7), Notifications (11g's card), and "Sessions by project" as one card for each
// project, each with Open or Start session, and the Orchestrator's footnote under them. The agent list is in the sidebar (`shell/AgentsSidebar.tsx`); the selection is in
// `screens/agentSelection.ts`. P1 puts agent create and edit in a later release: every function the page does
// not build is drawn with the 11l look (`NotBuilt`) and does nothing. The page makes the calls it made before:
// `projectList` when it opens and when the link comes back, `sessionList` when it opens, `sessionStart` on a
// press of Start session. No timer.
import type { AgentRecord, ChatRuntimeRecord, ChatSessionRecord, ProjectRecord } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { branchText, parseTime } from "../components/chat-format";
import { useCommandId } from "../components/command-id";
import { costLabel } from "../components/cost";
import { Column } from "../shell/Column";
import { useNavigation } from "../shell/navigation";
import { asTransportError, useConnection } from "../state/connection";
import { stateLabel, useChat } from "../state/chat";
import { age } from "../needs-you/format";
import { Avatar, Button, Card, Eyebrow, Icon, LaterTag, NotBuilt, StatePill, agentLook, effortName, modelName, permissionName, runtimeName } from "../ui";
import { isOrchestrator, useSelectedAgent } from "./agentSelection";
import "../styles/agents.css";

/** How many of the four bars an effort fills. An effort the table does not know fills none. */
function effortRank(effort: string): number {
  return ({ low: 1, medium: 2, high: 3, xhigh: 4, max: 4 } as Record<string, number>)[effort] ?? 0;
}

function Tile(props: { label: string; value: string; icon: "bot" | "sparkles" | "zap"; violet?: boolean; note?: string | null; bars?: number; title?: string }) {
  const { label, value, icon, violet = false, note = null, bars, title } = props;
  return (
    <div className={`ag-tile${violet ? " ag-tile-violet" : ""}`} role="group" aria-label={label} title={title}>
      <div className="ag-tile-top">
        <span className="ag-tile-label">{label}</span>
        <span className="ag-tile-chevrons" aria-hidden="true">
          <Icon name="chevrons-up-down" size={12} />
        </span>
      </div>
      <span className="ag-tile-value">
        <span className={`ag-tile-icon${violet ? " ag-tile-icon-violet" : ""}`}>
          <Icon name={icon} size={16} />
        </span>
        {value}
      </span>
      {bars !== undefined ? (
        <div className="ag-bars">
          {[1, 2, 3, 4].map((n) => (
            <span key={n} className="ag-bar" data-bar={n <= bars ? "on" : "off"} />
          ))}
        </div>
      ) : null}
      {note ? <span className="ag-tile-note">{note}</span> : null}
    </div>
  );
}

/** The position of the three-way control that is nearest an agent's mode (P7): six modes, three positions. A mode the table does not know has none. */
const POSITION_OF: Readonly<Record<string, number>> = {
  plan: 0,
  manual: 1,
  acceptEdits: 2,
  auto: 2,
  dontAsk: 2,
  bypassPermissions: 2,
};

/**
 * The three-way switch of 11g: disabled with the Later tag (R5), at the position nearest the agent's mode (P7). The
 * control's `title` and a visually hidden text name the exact mode, so the choice the six modes make is not lost.
 */
function PermissionSwitch(props: { mode: string }) {
  const { mode } = props;
  const positions = [
    { name: "Read only", icon: "search" },
    { name: "Ask before edits", icon: "lock" },
    { name: "Edit freely", icon: "zap" },
  ] as const;
  const selected = Object.hasOwn(POSITION_OF, mode) ? (POSITION_OF[mode] as number) : -1;
  return (
    <div className="ag-switch" role="group" aria-label="Permissions" title={permissionName(mode)}>
      {positions.map((p, i) => (
        <button key={p.name} type="button" className={`ag-switch-pos${i === selected ? " ag-switch-pos-on" : ""}`} disabled aria-pressed={i === selected ? "true" : "false"}>
          <Icon name={p.icon} size={15} />
          <span>{p.name}</span>
        </button>
      ))}
      <span className="ui-sr-only">{permissionName(mode)}</span>
      <span className="ag-switch-tag">
        <LaterTag />
      </span>
    </div>
  );
}

function Identity(props: { agent: AgentRecord; waiting: ChatSessionRecord[] }) {
  const { agent, waiting } = props;
  const projects = new Set(waiting.map((s) => s.project_id)).size;
  return (
    <div className="ag-identity">
      <Avatar agent={agentLook(agent.id)} size={64} />
      <div className="ag-identity-text">
        <h2 className="ag-name">{agent.name}</h2>
        <span className="ag-title">{agent.title}</span>
      </div>
      <div className="ag-identity-side">
        {isOrchestrator(agent) ? (
          <span className="ag-chip">
            <Icon name="layers" size={12} />
            One session per project
          </span>
        ) : null}
        {waiting.length > 0 ? (
          <span className="ag-waiting">
            <span className="ui-count">{waiting.length}</span>
            {`waiting across ${projects} ${projects === 1 ? "project" : "projects"}`}
          </span>
        ) : null}
      </div>
    </div>
  );
}

/**
 * 11g's notification card: a solid card, an amber icon tile, the label and its description, the "Later release" tag,
 * then the switch (off). It stays not built (P1, R5): no call, no focus, `aria-disabled`. It is drawn here, not
 * with `NotBuilt`'s `row` form, so the page edits no `ui/` component.
 */
function Notification() {
  return (
    <div className="ag-notify" aria-disabled="true" data-not-built="notify">
      <span className="ag-notify-tile" aria-hidden="true">
        <Icon name="chat" size={15} />
      </span>
      <span className="ag-notify-text">
        <span className="ag-notify-label">Send its questions to my phone</span>
        <span className="ag-notify-description">As push notifications with answer buttons</span>
      </span>
      <LaterTag />
      <span className="ag-notify-switch" aria-hidden="true" />
    </div>
  );
}

/** The Model tile's note: the agent's `billing` when it sets one, else the basis `cost.ts` records for the claude-code runtime (a chat runs the operator's subscription login and is shown in wu), else none. */
function modelNote(agent: AgentRecord): string | null {
  const billing = agent.runtime_options.billing;
  if (billing !== undefined && billing !== "") return billing;
  return agent.runtime === "claude-code" ? "Anthropic subscription · wu" : null;
}

function Detail(props: { agent: AgentRecord; waiting: ChatSessionRecord[]; runtimes: ChatRuntimeRecord[] }) {
  const { agent, waiting, runtimes } = props;
  // The agent's runtime options stay in the Runtime tile's title (G8), where the page's old "Runtime options" line was.
  const options = Object.entries(agent.runtime_options)
    .map(([id, value]) => `${id}: ${value}`)
    .join(", ");
  const others = runtimes.filter((r) => r.id !== agent.runtime).map((r) => runtimeName(r.id));
  return (
    <div className="ag-col">
      <Identity agent={agent} waiting={waiting} />
      <section className="ag-card" aria-label="Persona">
        <div className="ag-card-head">
          <Avatar agent={agentLook(agent.id)} size={20} />
          <span className="ag-card-title">Persona</span>
          <span className="ag-spacer" />
          <NotBuilt as="button" label="Edit" />
        </div>
        <p className="ag-persona">The persona text is not shown in this release.</p>
      </section>
      <section className="ag-block">
        <Eyebrow>Brain</Eyebrow>
        <div className="ag-tiles">
          <Tile
            label="Runtime"
            value={runtimeName(agent.runtime)}
            icon="bot"
            note={others.length > 0 ? `or ${others.join(", ")}` : null}
            title={options === "" ? undefined : options}
          />
          <Tile label="Model" value={modelName(agent.model)} icon="sparkles" violet note={modelNote(agent)} />
          <Tile label="Reasoning effort" value={effortName(agent.effort)} icon="zap" bars={effortRank(agent.effort)} />
        </div>
      </section>
      <section className="ag-block">
        <Eyebrow>Skills</Eyebrow>
        <div className="ag-chips">
          {agent.skills.map((skill) => (
            <span key={skill} className="ag-skill">
              <Icon name="sparkles" size={12} />
              {skill}
            </span>
          ))}
          <NotBuilt as="button" icon="plus" label="Add skill" />
        </div>
      </section>
      <section className="ag-block">
        <Eyebrow>Permissions</Eyebrow>
        <div className="ag-perm">
          <PermissionSwitch mode={agent.permission_mode} />
        </div>
      </section>
      <section className="ag-block">
        <Eyebrow>Notifications</Eyebrow>
        <Notification />
      </section>
    </div>
  );
}

/** One project's card: its live session of this agent (the state, the branch, the cost, the age, Open) or Start session. */
function ProjectCard(props: { project: ProjectRecord; agent: AgentRecord; session: ChatSessionRecord | undefined; projects: ProjectRecord[] }) {
  const { project, agent, session, projects } = props;
  const { call } = useConnection();
  const chat = useChat();
  const { openSession } = useNavigation();
  const command = useCommandId();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  async function start() {
    if (busy) return;
    setBusy(true);
    setError(null);
    // A retry of the same start after `unreachable` reuses the ID, so the server starts one session.
    const id = command.idFor(`start:${project.id}:${agent.id}`);
    try {
      const started = await call((t) => t.sessionStart({ project_id: project.id, agent_id: agent.id, command_id: id }));
      command.clear();
      // The record of the new session comes with the list.
      await chat.refreshList();
      openSession(started.session_id);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") command.clear();
      if (live.current) setError(err.message);
    } finally {
      if (live.current) setBusy(false);
    }
  }

  const waiting = session?.state === "needs_you";
  const since = session === undefined ? null : parseTime(session.last_turn_at ?? session.updated_at);
  return (
    <li>
      <Card tone={waiting ? "needs_you" : "default"}>
        <div className="ag-project">
          <div className="ag-project-top">
            <span className="ag-project-name">{project.name}</span>
            <span className="ag-spacer" />
            {session ? <StatePill state={session.state}>{stateLabel(session.state)}</StatePill> : null}
          </div>
          {session ? (
            <div className="ag-project-meta">
              <span className="ag-meta-branch">
                <Icon name="git-branch" size={11} />
                <span>{branchText(session, projects)}</span>
              </span>
              <span>{costLabel(session.usage, session.cache_ttl_seconds)}</span>
              {since !== null ? <span>{age(new Date(since).toISOString())}</span> : null}
            </div>
          ) : null}
        </div>
        {session ? (
          <div className={`ag-foot${waiting ? " ag-foot-waiting" : ""}`}>
            {waiting ? <span className="ag-foot-text">Waiting for your answer</span> : <span className="ag-spacer" />}
            <Button variant="ghost" size="sm" onClick={() => openSession(session.id)}>
              Open
              <Icon name="chevron-right" size={14} />
            </Button>
          </div>
        ) : (
          <div className="ag-foot">
            <span className="ag-foot-text ag-foot-quiet">No live session</span>
            <Button size="sm" icon="plus" disabled={busy} onClick={() => void start()}>
              Start session
            </Button>
          </div>
        )}
        {error ? (
          <p className="ag-error" role="alert">
            {error}
          </p>
        ) : null}
      </Card>
    </li>
  );
}

/** `onOpen` stays in the props type for `Shell.tsx`, which passes it; the page opens a chat through `useNavigation()`. */
export function Agents(_props: { onOpen: (session: string) => void }) {
  const { call, setProjects, projects, epoch } = useConnection();
  const chat = useChat();
  const [pageError, setPageError] = useState<string | null>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const loadProjects = useCallback(async () => {
    try {
      const list = await call((t) => t.projectList());
      setProjects(list.projects);
      if (live.current) setPageError(null);
    } catch (e) {
      const err = asTransportError(e);
      if (live.current && err.kind !== "unreachable") setPageError(err.message);
    }
  }, [call, setProjects]);

  // On opening the page, and once when the link comes back (`epoch`). Never on a timer. The session
  // list is read when the page opens; the provider reads it again when the link comes back.
  useEffect(() => {
    void loadProjects();
  }, [loadProjects, epoch]);
  const { refreshList } = chat;
  useEffect(() => {
    void refreshList().then((failure) => {
      if (failure !== null && live.current) setPageError(failure);
    });
  }, [refreshList]);

  const agents = chat.list?.agents ?? [];
  const agent = useSelectedAgent(agents);
  const waiting = agent === undefined ? [] : chat.live.filter((s) => s.agent_id === agent.id && s.state === "needs_you");

  return (
    <Column max={1000}>
      <section className="ag-page" aria-label="Agent">
        {pageError ? (
          <p className="page-error" role="alert">
            <span>{pageError}</span>
          </p>
        ) : null}
        {chat.list === null ? (
          <p className="muted">Loading…</p>
        ) : agent === undefined ? (
          <p className="empty">This server lists no agent.</p>
        ) : (
          <div className="ag-grid">
            <Detail agent={agent} waiting={waiting} runtimes={chat.list.runtimes} />
            <section className="ag-block ag-sessions-block" aria-labelledby="agent-sessions">
              <div className="ag-sessions-head">
                <Eyebrow id="agent-sessions">Sessions by project</Eyebrow>
                <span className="ag-spacer" />
                <span className="ag-count">{projects === null ? "" : projects.length}</span>
              </div>
              {projects === null ? (
                <p className="muted">Loading…</p>
              ) : projects.length === 0 ? (
                <p className="empty">No projects yet.</p>
              ) : (
                <ul className="ag-sessions" aria-label="Sessions by project">
                  {projects.map((p) => (
                    <ProjectCard
                      key={`${p.id}:${agent.id}`}
                      project={p}
                      agent={agent}
                      projects={projects}
                      session={chat.live.find((s) => s.project_id === p.id && s.agent_id === agent.id)}
                    />
                  ))}
                </ul>
              )}
              {isOrchestrator(agent) ? (
                <span className="ag-footnote">
                  At most one live Orchestrator session per project. Opening it from a project, from here, or from Needs you lands in the same session.
                </span>
              ) : null}
            </section>
          </div>
        )}
      </section>
    </Column>
  );
}
