// Remove project (11e, `r1.s5.w5`): the remove button of a project block, the confirm that opens inside
// the block, and the line that takes the block's place after a remove. The confirm is no modal and
// takes no confirm code (it is no request act). It reads `run_list` once when it opens. It has three
// states: a workflow of the project is running (Remove stays locked), the project has live sessions
// (they end), or nothing is live.
import type { ProjectRecord, RunSummary } from "@pulse-factory/api-client";
import { useEffect, useRef, useState } from "react";
import { agentName, stateLabel, useChat } from "../state/chat";
import { asTransportError, useConnection } from "../state/connection";
import { Button, Icon, StateDot } from "../ui";
import { branchText } from "./chat-format";
import { useCommandId } from "./command-id";

/** The button in the block's head. It is the 28 px icon button of the canvas, with a 14 px icon. */
export function RemoveButton(props: { project: ProjectRecord; open: boolean; onOpen: () => void }) {
  const { project, open, onOpen } = props;
  const title = `Remove ${project.name}`;
  return (
    <button
      type="button"
      className="ui-icon-btn ui-icon-btn-28 pj-remove"
      title={title}
      aria-label={title}
      aria-expanded={open}
      onClick={onOpen}
    >
      <Icon name="trash" size={14} />
    </button>
  );
}

/** The line that shows in the block's place after a remove. */
export function RemovedLine(props: { name: string }) {
  const { server } = useConnection();
  const serverName = server.server?.name ?? server.address;
  return (
    <p className="rm-removed" role="status">
      <Icon name="check" size={14} />
      <span>{`${props.name} was removed. Its sessions ended; the folder is untouched on ${serverName}.`}</span>
    </p>
  );
}

export interface RemoveConfirmProps {
  project: ProjectRecord;
  projects: ProjectRecord[];
  onCancel: () => void;
  /** Called once after the server removed the project. */
  onRemoved: (project: ProjectRecord) => Promise<void> | void;
}

export function RemoveConfirm(props: RemoveConfirmProps) {
  const { project, projects, onCancel, onRemoved } = props;
  const { call, link, server } = useConnection();
  const chat = useChat();
  const serverName = server.server?.name ?? server.address;
  const down = link.state === "down";
  const sessions = chat.live.filter((s) => s.project_id === project.id);
  // `null` until `run_list` answers. A failed read counts as no run: the server's refusal holds anyway.
  const [runs, setRuns] = useState<RunSummary[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const commandId = useCommandId();
  const live = useRef(true);
  const callRef = useRef(call);
  callRef.current = call;

  // Once, when the confirm opens.
  useEffect(() => {
    live.current = true;
    callRef
      .current((t) => t.runList())
      .then((list) => {
        if (live.current) setRuns(list.runs);
      })
      .catch(() => {
        if (live.current) setRuns([]);
      });
    return () => {
      live.current = false;
    };
  }, []);

  const active = (runs ?? []).filter((r) => r.state === "active" && r.project_id === project.id);
  const locked = active.length > 0;

  async function remove() {
    if (busy || locked || down || runs === null) return;
    const id = commandId.idFor(project.id);
    setBusy(true);
    setError(null);
    try {
      await callRef.current((t) => t.projectRemove({ id: project.id, end_sessions: true, command_id: id }));
      commandId.clear();
      await onRemoved(project);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") commandId.clear();
      if (live.current) setError(err.message);
    } finally {
      if (live.current) setBusy(false);
    }
  }

  const folder = <span className="mono rm-root">{project.root}</span>;
  const failure = error ? (
    <p className="rm-error" role="alert">
      {error}
    </p>
  ) : null;

  if (locked) {
    return (
      <div className="rm-confirm rm-row" role="alertdialog" aria-label={`Remove ${project.name}`}>
        <div className="rm-text">
          <h4 className="rm-title">{`${project.name} has a workflow running`}</h4>
          <p className="rm-line">
            {active.length === 1 ? "Workflow " : "Workflows "}
            {active.map((r, i) => (
              <span key={r.id}>
                {i > 0 ? ", " : ""}
                <b>{r.spine}</b>
              </span>
            ))}
            {active.length === 1 ? " is running. " : " are running. "}
            You can remove the project after the run ends.
          </p>
          {failure}
        </div>
        <div className="rm-actions">
          <Button onClick={onCancel}>Cancel</Button>
          <Button variant="locked">Remove project</Button>
        </div>
      </div>
    );
  }

  const heading = <h4 className="rm-title">{`Remove ${project.name} from Pulse Factory?`}</h4>;
  const action = (label: string) => (
    <Button variant="danger" disabled={busy || down || runs === null} onClick={() => void remove()}>
      {label}
    </Button>
  );

  if (sessions.length > 0) {
    return (
      <div className="rm-confirm rm-end" role="alertdialog" aria-label={`Remove ${project.name}`}>
        <div className="rm-text">
          {heading}
          <ul className="rm-sessions" aria-label="Live sessions">
            {sessions.map((s) => (
              <li className="rm-session" key={s.id}>
                <StateDot state={s.state} />
                <span>{`${agentName(chat.list, s.agent_id)} · ${branchText(s, projects)} · ${stateLabel(s.state)}`}</span>
              </li>
            ))}
          </ul>
          <p className="rm-line">
            Removing the project ends these sessions. The folder {folder} stays on {serverName}.
          </p>
          {failure}
        </div>
        <div className="rm-actions">
          <Button onClick={onCancel}>Cancel</Button>
          {action("End sessions and remove")}
        </div>
      </div>
    );
  }

  return (
    <div className="rm-confirm rm-row" role="alertdialog" aria-label={`Remove ${project.name}`}>
      <div className="rm-text">
        {heading}
        <p className="rm-line">
          The folder {folder} stays on {serverName}.
        </p>
        {failure}
      </div>
      <div className="rm-actions">
        <Button onClick={onCancel}>Cancel</Button>
        {action("Remove project")}
      </div>
    </div>
  );
}
