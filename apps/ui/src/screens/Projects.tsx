// The Projects page (11e): the live projects from `projectList()`, each as a block with its live sessions as
// cards, and today's "Add project" form below them (`w5` replaces the form). The page sits in the centred
// `Column` of the shell. "New session" opens the project's Orchestrator chat, or starts one
// (`shell/useStartSession.ts`).
import type { ChatSessionRecord, ProjectRecord } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { branchText } from "../components/chat-format";
import { newCommandId } from "../components/command-id";
import { costLabel } from "../components/cost";
import { clock } from "../components/format";
import { Column } from "../shell/Column";
import { useNavigation } from "../shell/navigation";
import { GroupSwitch, STATE_GROUPS, groupOf, type Grouping } from "../shell/Sidebar";
import { useStartSession } from "../shell/useStartSession";
import { agentName, stateLabel, useChat } from "../state/chat";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { Avatar, Button, CountBadge, Icon, Input, StatePill, agentLook } from "../ui";

function SessionCard(props: { session: ChatSessionRecord; projects: ProjectRecord[]; byState: boolean }) {
  const { session: s, projects, byState } = props;
  const chat = useChat();
  const { openSession } = useNavigation();
  const agent = agentName(chat.list, s.agent_id);
  const branch = branchText(s, projects);
  return (
    <li>
      <button className="pj-card" type="button" aria-label={`Open ${agent} session in ${s.project_name}`} onClick={() => openSession(s.id)}>
        <span className="pj-card-top">
          <Avatar agent={agentLook(s.agent_id)} size={28} />
          <span className="pj-card-agent">{agent}</span>
          <StatePill state={s.state}>{stateLabel(s.state)}</StatePill>
        </span>
        <span className="pj-card-line mono">
          <Icon name="git-branch" size={11} />
          <span className="pj-card-branch">{byState ? `${s.project_name} · ${branch}` : branch}</span>
          <span className="pj-card-cost">{costLabel(s.usage, s.cache_ttl_seconds)}</span>
        </span>
      </button>
    </li>
  );
}

function ProjectBlock(props: { project: ProjectRecord; projects: ProjectRecord[]; start: ReturnType<typeof useStartSession> }) {
  const { project: p, projects, start } = props;
  const chat = useChat();
  const sessions = chat.live.filter((s) => s.project_id === p.id);
  const needs = sessions.filter((s) => s.state === "needs_you").length;
  const error = start.errorOf(p.id);
  return (
    <div className="pj-block">
      <div className="pj-block-head">
        <span className="pj-chevron">
          <Icon name="chevron-down" size={14} />
        </span>
        <h3 className="pj-name">{p.name}</h3>
        <CountBadge count={needs} title={`${needs} need you`} />
        <span className="pj-root mono">{p.root}</span>
        <span className="pj-spacer" />
        <Button
          variant="ghost"
          size="sm"
          icon="plus"
          aria-label={`New session in ${p.name}`}
          disabled={start.busy === p.id}
          onClick={() => void start.start(p.id)}
        >
          New session
        </Button>
      </div>
      {error ? (
        <p className="pj-error" role="alert">
          {error}
        </p>
      ) : null}
      {sessions.length === 0 ? (
        <p className="pj-quiet">No live session</p>
      ) : (
        <ul className="pj-cards" aria-label={`Live sessions of ${p.name}`}>
          {sessions.map((s) => (
            <SessionCard key={s.id} session={s} projects={projects} byState={false} />
          ))}
        </ul>
      )}
    </div>
  );
}

export function Projects() {
  const { call, setProjects, epoch, projects, projectsAt, link, server } = useConnection();
  const [pageError, setPageError] = useState<string | null>(null);
  const [root, setRoot] = useState("");
  const [name, setName] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [grouping, setGrouping] = useState<Grouping>("project");
  const chat = useChat();
  const start = useStartSession();
  // The command ID of an add that ended with `unreachable`: a retry of the same submit reuses it,
  // so the server admits the add once (A7).
  const pending = useRef<{ root: string; name: string; id: string } | null>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const list = await call((t) => t.projectList());
      setProjects(list.projects);
      if (live.current) setPageError(null);
    } catch (e) {
      const err = asTransportError(e);
      // A disconnect shows the banner. Any other refusal is one error line on this page.
      if (live.current) setPageError(isDownKind(err.kind) ? null : err.message);
    }
  }, [call, setProjects]);

  // On opening the page, and once when the link comes back (`epoch`). Never on a timer.
  useEffect(() => {
    void load();
  }, [load, epoch]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const r = root.trim();
    const n = name.trim();
    if (r === "") {
      setFormError("Enter the absolute path of the project's folder on the server.");
      return;
    }
    setFormError(null);
    setBusy(true);
    const same = pending.current && pending.current.root === r && pending.current.name === n;
    const id = same && pending.current ? pending.current.id : newCommandId();
    pending.current = { root: r, name: n, id };
    try {
      await call((t) => t.projectAdd({ root: r, ...(n ? { name: n } : {}), command_id: id }));
      pending.current = null;
      if (live.current) {
        setRoot("");
        setName("");
      }
      await load();
    } catch (err) {
      const te = asTransportError(err);
      if (te.kind !== "unreachable") pending.current = null;
      if (live.current) setFormError(te.message);
    } finally {
      if (live.current) setBusy(false);
    }
  }

  const down = link.state === "down";
  const serverName = server.server?.name ?? server.address;
  return (
    <section aria-labelledby="projects-title">
      <Column>
        <div className="pj-head">
          <div className="pj-head-text">
            <h2 id="projects-title" className="pj-title">
              Projects
            </h2>
            {down && projectsAt !== null ? (
              <p className="stale">{`As of ${clock(projectsAt)}, when ${serverName} was last reachable`}</p>
            ) : null}
          </div>
          <GroupSwitch value={grouping} onChange={setGrouping} />
        </div>

        {pageError ? (
          <p className="page-error" role="alert">
            <span>{pageError}</span>
            <Button onClick={() => void load()}>Retry now</Button>
          </p>
        ) : null}

        {projects === null ? (
          <p className="muted">Loading…</p>
        ) : projects.length === 0 ? (
          <p className="empty">No projects yet. Add one below.</p>
        ) : grouping === "project" ? (
          projects.map((p) => <ProjectBlock key={p.id} project={p} projects={projects} start={start} />)
        ) : (
          <div className="pj-groups">
            {STATE_GROUPS.map((g) => ({ ...g, rows: chat.live.filter((s) => groupOf(s.state) === g.id) }))
              .filter((g) => g.rows.length > 0)
              .map((g) => (
                <div className="pj-group" key={g.id}>
                  <div className="pj-group-head">
                    <h3 className="pj-group-name">{g.name}</h3>
                    <span className="pj-group-count">{g.rows.length}</span>
                  </div>
                  <ul className="pj-cards" aria-label={`${g.name} sessions`}>
                    {g.rows.map((s) => (
                      <SessionCard key={s.id} session={s} projects={projects} byState />
                    ))}
                  </ul>
                </div>
              ))}
          </div>
        )}

        <form className="add-form" onSubmit={submit} noValidate aria-label="Add project">
          <h3>Add project</h3>
          <label htmlFor="pf-root">Project root</label>
          <Input
            mono
            id="pf-root"
            type="text"
            autoComplete="off"
            spellCheck={false}
            placeholder="/home/you/code/project"
            value={root}
            disabled={busy || down}
            onChange={(e) => setRoot(e.target.value)}
          />
          <label htmlFor="pf-project-name">Name (optional)</label>
          <Input
            id="pf-project-name"
            type="text"
            autoComplete="off"
            value={name}
            disabled={busy || down}
            onChange={(e) => setName(e.target.value)}
          />
          {formError ? (
            <p className="problem-title" role="alert">
              {formError}
            </p>
          ) : null}
          <Button type="submit" disabled={busy || down}>
            Add project
          </Button>
        </form>
      </Column>
    </section>
  );
}
