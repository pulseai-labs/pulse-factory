// The sidebar (11a): the `region` named "Sidebar", 288 px, on the Projects, Agents and chat pages only. On
// the Agents page its body is `AgentsSidebar`. On the other two it holds the head "Projects" with the
// By project / By state switch, Search sessions, the projects with their live sessions (or the sessions
// grouped by state), and under them one "Workflow runs" group for the server (Planner ruling 6).
//
// The shell's run list: this component calls `runList` once each time it mounts (it unmounts on the
// focused pages, so a visit to Needs you and back reads the list again) and once each time the link comes
// back (`epoch`). No timer and no focus handler. A failure leaves the group as it was; a disconnect shows
// the banner.
import type { ChatSessionRecord, ProjectRecord, RunSummary } from "@pulse-factory/api-client";
import { useCallback, useEffect, useState } from "react";
import { StreamLine } from "../components/StreamLine";
import { branchText } from "../components/chat-format";
import { agentName, stateLabel, useChat } from "../state/chat";
import { useConnection } from "../state/connection";
import { useRequests } from "../state/requests";
import { Avatar, Chip, CountBadge, Eyebrow, Icon, Input, StateDot, agentLook, type RunState } from "../ui";
import { AgentsSidebar } from "./AgentsSidebar";
import type { Page } from "./history";
import { useStartSession } from "./useStartSession";

export type Grouping = "project" | "state";

/** The four groups of By state, in this order. `done` holds `done` and `merged`. */
export const STATE_GROUPS = [
  { id: "needs_you", name: "Needs you" },
  { id: "working", name: "Working" },
  { id: "idle", name: "Idle" },
  { id: "done", name: "Done" },
] as const;

export type StateGroup = (typeof STATE_GROUPS)[number]["id"];

export function groupOf(state: RunState): StateGroup {
  return state === "needs_you" || state === "working" || state === "idle" ? state : "done";
}

/** The By project / By state switch (the sidebar's head and the Projects page's head). */
export function GroupSwitch(props: { value: Grouping; onChange(value: Grouping): void }) {
  const { value, onChange } = props;
  return (
    <div className="seg" role="group" aria-label="Group sessions">
      <button className={`seg-btn${value === "project" ? " seg-on" : ""}`} type="button" aria-pressed={value === "project"} onClick={() => onChange("project")}>
        By project
      </button>
      <button className={`seg-btn${value === "state" ? " seg-on" : ""}`} type="button" aria-pressed={value === "state"} onClick={() => onChange("state")}>
        By state
      </button>
    </div>
  );
}

/** The state words of a run, as the Needs you page says them. */
function runWords(run: RunSummary): string {
  if (run.state === "ended") return run.end_reason ? `Ended: ${run.end_reason}` : "Ended";
  if (run.halted_reason !== null) return `Halted: ${run.halted_reason}`;
  if (run.waiting !== null) return `Waiting: ${run.waiting}`;
  return "Running";
}

function Row(props: {
  session: ChatSessionRecord;
  projects: ProjectRecord[];
  grouping: Grouping;
  current: string | null;
  onOpen(session: string): void;
}) {
  const { session: s, projects, grouping } = props;
  const chat = useChat();
  const agent = agentName(chat.list, s.agent_id);
  const unqualified = s.process !== null && !s.process.qualified;
  const branch = branchText(s, projects);
  return (
    <li>
      <button
        className={`sb-row${props.current === s.id ? " sb-row-on" : ""}`}
        type="button"
        aria-label={`${agent} session in ${s.project_name}: ${stateLabel(s.state)}, ${branch}${unqualified ? ", unqualified" : ""}`}
        onClick={() => props.onOpen(s.id)}
      >
        <Avatar agent={agentLook(s.agent_id)} size={24} />
        <span className="sb-row-text">
          <span className="sb-row-agent">{agent}</span>
          <span className="sb-row-sub mono">
            <Icon name="git-branch" size={11} />
            <span className="sb-row-branch">{grouping === "state" ? `${s.project_name} · ${branch}` : branch}</span>
          </span>
        </span>
        {unqualified ? <Chip tone="amber">unqualified</Chip> : null}
        <span className="sb-dot" title={stateLabel(s.state)}>
          <StateDot state={s.state} />
        </span>
      </button>
    </li>
  );
}

function Runs() {
  const { state } = useRequests();
  const runs = state.runs?.runs ?? [];
  if (runs.length === 0) return null;
  return (
    <div className="sb-runs">
      <Eyebrow>Workflow runs</Eyebrow>
      <ul className="sb-run-list">
        {runs.map((run) => (
          <li className="sb-run" key={run.id} aria-label={`Run ${run.id}`}>
            <span className={`sb-run-tile ${run.state === "ended" ? "sb-run-tile-ended" : "sb-run-tile-active"}`}>
              <Icon name="activity" size={13} />
            </span>
            <span className="sb-row-text">
              <span className="sb-row-agent">{run.spine}</span>
              <span className="sb-run-sub">{runWords(run)}</span>
            </span>
            <CountBadge count={run.open_requests} title={`${run.open_requests} open`} />
          </li>
        ))}
      </ul>
    </div>
  );
}

function Tree(props: { grouping: Grouping; query: string; current: string | null; onOpen(session: string): void }) {
  const { grouping, query, current, onOpen } = props;
  const { projects } = useConnection();
  const chat = useChat();
  const start = useStartSession();
  const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());

  if (projects === null) return null;
  if (projects.length === 0) return <p className="sb-empty">No projects yet</p>;

  const q = query.trim().toLowerCase();
  const matches = (s: ChatSessionRecord): boolean =>
    q === "" ||
    [agentName(chat.list, s.agent_id), s.project_name, branchText(s, projects)].some((text) => text.toLowerCase().includes(q));
  const live = chat.live.filter(matches);

  if (grouping === "state") {
    const groups = STATE_GROUPS.map((g) => ({ ...g, rows: live.filter((s) => groupOf(s.state) === g.id) })).filter((g) => g.rows.length > 0);
    return (
      <div className="sb-groups">
        {groups.map((g) => (
          <div className="sb-group" key={g.id}>
            <div className="sb-group-head">
              <span className="sb-group-name">{g.name}</span>
              <span className="sb-group-count">{g.rows.length}</span>
            </div>
            <ul className="sb-rows">
              {g.rows.map((s) => (
                <Row key={s.id} session={s} projects={projects} grouping={grouping} current={current} onOpen={onOpen} />
              ))}
            </ul>
          </div>
        ))}
      </div>
    );
  }

  return (
    <ul className="sb-projects">
      {projects.map((p) => {
        const sessions = live.filter((s) => s.project_id === p.id);
        if (q !== "" && sessions.length === 0) return null;
        const needs = chat.live.filter((s) => s.project_id === p.id && s.state === "needs_you").length;
        const isFolded = folded.has(p.id);
        const error = start.errorOf(p.id);
        return (
          <li key={p.id} className="sb-project">
            <div className="sb-project-head" title={p.root}>
              <button
                className="sb-fold"
                type="button"
                aria-label={`${isFolded ? "Unfold" : "Fold"} ${p.name}`}
                aria-expanded={!isFolded}
                onClick={() =>
                  setFolded((set) => {
                    const next = new Set(set);
                    if (next.has(p.id)) next.delete(p.id);
                    else next.add(p.id);
                    return next;
                  })
                }
              >
                <Icon name={isFolded ? "chevron-right" : "chevron-down"} size={14} />
              </button>
              <span className="sb-project-name">{p.name}</span>
              <span className="sb-spacer" />
              <CountBadge count={needs} title={`${needs} need you`} />
              <button
                className="sb-new"
                type="button"
                title={`New session in ${p.name}`}
                aria-label={`New session in ${p.name}`}
                disabled={start.busy === p.id}
                onClick={() => void start.start(p.id)}
              >
                <Icon name="plus" size={14} />
              </button>
            </div>
            {error ? (
              <p className="sb-error" role="alert">
                {error}
              </p>
            ) : null}
            {sessions.length === 0 || isFolded ? null : (
              <ul className="sb-rows" aria-label={`Sessions of ${p.name}`}>
                {sessions.map((s) => (
                  <Row key={s.id} session={s} projects={projects} grouping={grouping} current={current} onOpen={onOpen} />
                ))}
              </ul>
            )}
          </li>
        );
      })}
    </ul>
  );
}

export function Sidebar(props: {
  page: Page;
  current: string | null;
  grouping: Grouping;
  onGrouping(value: Grouping): void;
  onOpen(session: string): void;
}) {
  const { page, current, grouping, onGrouping, onOpen } = props;
  const { call, epoch } = useConnection();
  const chat = useChat();
  const requests = useRequests();
  const [query, setQuery] = useState("");
  const { apply } = requests;

  const loadRuns = useCallback(async () => {
    try {
      apply({ type: "runs", runs: await call((t) => t.runList()) });
    } catch {
      // A failure leaves the group as it was. A disconnect shows the banner.
    }
  }, [call, apply]);

  useEffect(() => {
    void loadRuns();
  }, [loadRuns, epoch]);

  return (
    <section className="sidebar" aria-label="Sidebar">
      {page.kind === "agents" ? (
        <AgentsSidebar />
      ) : (
        <>
          <div className="sb-head">
            <span className="sb-title">Projects</span>
            <GroupSwitch value={grouping} onChange={onGrouping} />
          </div>
          <label className="sb-search">
            <Icon name="search" size={14} />
            <Input
              type="search"
              placeholder="Search sessions"
              aria-label="Search sessions"
              autoComplete="off"
              spellCheck={false}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <div className="sb-body">
            <Tree grouping={grouping} query={query} current={current} onOpen={onOpen} />
            <Runs />
          </div>
        </>
      )}
      {chat.following === null ? <StreamLine /> : null}
    </section>
  );
}
