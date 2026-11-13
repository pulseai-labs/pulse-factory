// The Projects page (11e): the live projects from `projectList()`, each as a block with its live sessions as
// cards. "Add a project" in the header opens the picker (`components/AddProject.tsx`), and the remove
// button of each block opens its confirm (`components/RemoveProject.tsx`). The page sits in the centred
// `Column` of the shell. "New session" opens the project's Orchestrator chat, or starts one
// (`shell/useStartSession.ts`).
import type { ChatSessionRecord, ProjectRecord } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { AddProject } from "../components/AddProject";
import { RemoveButton, RemoveConfirm, RemovedLine } from "../components/RemoveProject";
import { branchText } from "../components/chat-format";
import { costLabel } from "../components/cost";
import { clock } from "../components/format";
import { Column } from "../shell/Column";
import { useNavigation } from "../shell/navigation";
import { GroupSwitch, STATE_GROUPS, groupOf, type Grouping } from "../shell/Sidebar";
import { useStartSession } from "../shell/useStartSession";
import { agentName, stateLabel, useChat } from "../state/chat";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { Avatar, Button, CountBadge, Icon, StatePill, agentLook } from "../ui";

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

function ProjectBlock(props: {
  project: ProjectRecord;
  projects: ProjectRecord[];
  start: ReturnType<typeof useStartSession>;
  onRemoved: (project: ProjectRecord) => Promise<void>;
}) {
  const { project: p, projects, start, onRemoved } = props;
  const chat = useChat();
  const [confirming, setConfirming] = useState(false);
  const sessions = chat.live.filter((s) => s.project_id === p.id);
  const needs = sessions.filter((s) => s.state === "needs_you").length;
  const error = start.errorOf(p.id);
  return (
    <div className="pj-block" id={`pj-${p.id}`} role="group" aria-label={p.name} tabIndex={-1}>
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
        <RemoveButton project={p} open={confirming} onOpen={() => setConfirming(true)} />
      </div>
      {confirming ? (
        <RemoveConfirm project={p} projects={projects} onCancel={() => setConfirming(false)} onRemoved={onRemoved} />
      ) : null}
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
  const [adding, setAdding] = useState(false);
  // The projects this page removed, so each block's place keeps its notice line after the reload.
  const [removed, setRemoved] = useState<ProjectRecord[]>([]);
  // The block that "Open project" names; it takes focus once the list holds it.
  const [focusId, setFocusId] = useState<string | null>(null);
  const [grouping, setGrouping] = useState<Grouping>("project");
  const chat = useChat();
  const start = useStartSession();
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

  useEffect(() => {
    if (focusId === null) return;
    const block = document.getElementById(`pj-${focusId}`);
    if (block) {
      block.focus();
      setFocusId(null);
    }
  }, [focusId, projects]);

  const closeDialog = useCallback((openId?: string) => {
    setAdding(false);
    if (openId) setFocusId(openId);
    else document.getElementById("pj-add-project")?.focus();
  }, []);

  const afterRemove = useCallback(
    async (project: ProjectRecord) => {
      setRemoved((list) => (list.some((p) => p.id === project.id) ? list : [...list, project]));
      await load();
    },
    [load],
  );

  // The live projects and, in their places, the lines of the projects removed here.
  const shown: Array<{ project: ProjectRecord; gone: boolean }> = [
    ...(projects ?? []).map((project) => ({ project, gone: false })),
    ...removed.filter((r) => !(projects ?? []).some((p) => p.id === r.id)).map((project) => ({ project, gone: true })),
  ].sort((a, b) => a.project.id.localeCompare(b.project.id, undefined, { numeric: true }));

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
          <Button id="pj-add-project" size="lg" icon="plus" disabled={down} onClick={() => setAdding(true)}>
            Add a project
          </Button>
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
        ) : shown.length === 0 ? (
          <p className="empty">No projects yet. Use Add a project to add one.</p>
        ) : grouping === "project" ? (
          shown.map(({ project: p, gone }) =>
            gone ? (
              <div className="pj-block" key={p.id}>
                <RemovedLine name={p.name} />
              </div>
            ) : (
              <ProjectBlock key={p.id} project={p} projects={projects} start={start} onRemoved={afterRemove} />
            ),
          )
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

        {adding ? <AddProject onAdded={load} onClose={closeDialog} /> : null}
      </Column>
    </section>
  );
}
