// The Agents page (6a), read only: the agents the server lists, the selected agent's defaults, and
// its sessions by project, each with Open or Start session. P1 puts create and edit in Release 2, so
// the page has no edit control. The labels of the defaults are the ids the server sends.
import type { AgentRecord, ChatSessionRecord, ProjectRecord } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { branchText } from "../components/chat-format";
import { useCommandId } from "../components/command-id";
import { costLabel } from "../components/cost";
import { asTransportError, useConnection } from "../state/connection";
import { stateLabel, useChat } from "../state/chat";
import { Button, StatePill } from "../ui";

function Defaults(props: { agent: AgentRecord }) {
  const { agent } = props;
  const options = Object.entries(agent.runtime_options).map(([id, value]) => `${id}: ${value}`);
  const rows: Array<[string, string]> = [
    ["Runtime", agent.runtime],
    ["Model", agent.model],
    ["Effort", agent.effort],
    ["Permission mode", agent.permission_mode],
    ["Skills", agent.skills.length === 0 ? "none" : agent.skills.join(", ")],
    ["Runtime options", options.length === 0 ? "none" : options.join(", ")],
  ];
  return (
    <ul className="defaults" aria-label="Defaults">
      {rows.map(([key, value]) => (
        <li key={key}>
          <span className="defaults-key">{key}</span>
          <span className="defaults-value mono">{value}</span>
        </li>
      ))}
    </ul>
  );
}

function ProjectRow(props: {
  project: ProjectRecord;
  agent: AgentRecord;
  session: ChatSessionRecord | undefined;
  projects: ProjectRecord[];
  onOpen: (session: string) => void;
}) {
  const { project, agent, session, projects, onOpen } = props;
  const { call } = useConnection();
  const chat = useChat();
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
      onOpen(started.session_id);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") command.clear();
      if (live.current) setError(err.message);
    } finally {
      if (live.current) setBusy(false);
    }
  }

  return (
    <li className="agent-session">
      <span className="project-name">{project.name}</span>
      {session ? (
        <>
          <StatePill state={session.state}>{stateLabel(session.state)}</StatePill>
          <span className="mono muted">{branchText(session, projects)}</span>
          <span className="muted">{costLabel(session.usage, session.cache_ttl_seconds)}</span>
          <Button onClick={() => onOpen(session.id)}>
            Open
          </Button>
        </>
      ) : (
        <>
          <span className="muted">No live session</span>
          <Button disabled={busy} onClick={() => void start()}>
            Start session
          </Button>
        </>
      )}
      {error ? (
        <p className="problem-title agent-session-error" role="alert">
          {error}
        </p>
      ) : null}
    </li>
  );
}

export function Agents(props: { onOpen: (session: string) => void }) {
  const { onOpen } = props;
  const { call, setProjects, projects, epoch } = useConnection();
  const chat = useChat();
  const [selected, setSelected] = useState<string | null>(null);
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
  const agent = agents.find((a) => a.id === selected) ?? agents[0];

  return (
    <section className="page" aria-labelledby="agents-title">
      <div className="page-head">
        <h2 id="agents-title">Agents</h2>
      </div>

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
        <>
          {agents.length > 1 ? (
            <ul className="agent-list" aria-label="Agent list">
              {agents.map((a) => (
                <li key={a.id}>
                  <Button
                    aria-pressed={a.id === agent.id}
                    onClick={() => setSelected(a.id)}
                  >
                    {a.name}
                  </Button>
                </li>
              ))}
            </ul>
          ) : null}
          <section className="settings-block" aria-labelledby="agent-name">
            <h3 id="agent-name">{agent.name}</h3>
            <p className="muted">{agent.title}</p>
            <Defaults agent={agent} />
          </section>
          <section className="settings-block" aria-labelledby="agent-sessions">
            <h3 id="agent-sessions">Sessions by project</h3>
            {projects === null ? (
              <p className="muted">Loading…</p>
            ) : projects.length === 0 ? (
              <p className="empty">No projects yet.</p>
            ) : (
              <ul className="agent-sessions" aria-label="Sessions by project">
                {projects.map((p) => (
                  <ProjectRow
                    key={`${p.id}:${agent.id}`}
                    project={p}
                    agent={agent}
                    projects={projects}
                    session={chat.live.find((s) => s.project_id === p.id && s.agent_id === agent.id)}
                    onOpen={onOpen}
                  />
                ))}
              </ul>
            )}
          </section>
        </>
      )}
    </section>
  );
}
