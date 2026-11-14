// The chat with an agent (11a, 11b): a header with the avatar, the agent, the state, the worktree and branch
// chips the server read live, the `unqualified` mark, the cost, End session and the agent's settings; the
// message stream; and the composer. The project is in the header's tree and not drawn (11a has none; the
// chat's tab already reads "<agent> · <project>"). End session asks in a strip under the header (11b).
// Every call goes through the transport, and the stream is the one the shell holds open.
import type { ChatAnswerArg } from "@pulse-factory/api-client";
import { useEffect, useMemo, useState } from "react";
import { openSingleQuestion, type AnswerFn } from "../components/Cards";
import { Messages } from "../components/ChatItems";
import { Composer } from "../components/Composer";
import { Column } from "../shell/Column";
import { useNavigation } from "../shell/navigation";
import { StreamLine } from "../components/StreamLine";
import { branchText, lastSegment } from "../components/chat-format";
import { useCommandId } from "../components/command-id";
import { costLabel, dollarsTitle } from "../components/cost";
import { agentName, stateLabel, useChat } from "../state/chat";
import { asTransportError, useConnection, useServerName } from "../state/connection";
import { Avatar, Button, Chip, Icon, IconButton, StatePill, agentLook } from "../ui";

export function Chat(props: { sessionId: string }) {
  const { sessionId } = props;
  const { call, projects } = useConnection();
  const serverName = useServerName();
  const chat = useChat();
  const { ensureList } = chat;
  const endId = useCommandId();
  const answerId = useCommandId();
  const startId = useCommandId();
  const { openPage, openSession } = useNavigation();
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [endError, setEndError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  // The runtimes (the composer's controls) come with the list, read when the first chat opens.
  useEffect(() => {
    void ensureList();
  }, [ensureList]);

  const record = chat.records[sessionId];
  const held = chat.items[sessionId];
  const items = useMemo(() => Object.values(held ?? {}).sort((a, b) => a.seq - b.seq), [held]);

  if (!record) {
    return (
      <section className="page" aria-label="Chat">
        <p className="muted">Loading…</p>
        <StreamLine />
      </section>
    );
  }

  const name = agentName(chat.list, record.agent_id);
  const ended = record.ended_at !== null;
  const lost = record.context_lost;
  const unqualified = record.process !== null && !record.process.qualified;

  async function end() {
    if (!record) return;
    setConfirmEnd(false);
    setEndError(null);
    const id = endId.idFor("end");
    try {
      await call((t) => t.sessionEnd({ session_id: record.id, command_id: id }));
      endId.clear();
      // The session's own `session` frame says the same; this makes the screen follow at once.
      // The clocks of the app and the server can differ, so the stamp never goes back before the record's.
      const now = new Date().toISOString();
      const stamp = now > record.updated_at ? now : record.updated_at;
      chat.applyRecord({ ...record, state: "idle", ended_at: stamp, updated_at: stamp });
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") endId.clear();
      setEndError(err.message);
    }
  }

  // One answer, in place. The same answer after `unreachable` reuses the command ID; any other end of
  // an answer spends it. The card follows the items, so a refusal only shows the server's text.
  const onAnswer: AnswerFn = async (seq: number, answer: ChatAnswerArg) => {
    if (!record) return null;
    const id = answerId.idFor(`${seq}:${JSON.stringify(answer)}`);
    try {
      const out = await call((t) => t.sessionAnswer({ session_id: record.id, seq, answer, command_id: id }));
      answerId.clear();
      chat.applyRecord(out.session);
      return out.note;
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") answerId.clear();
      return err.message;
    }
  };

  // Start session after the session ended: a new session of the same project and agent. The server has
  // no verb that resumes an ended one (`session_start` always starts a new session).
  async function startAgain() {
    if (!record || starting) return;
    setStarting(true);
    setStartError(null);
    const id = startId.idFor(`start:${record.project_id}:${record.agent_id}`);
    try {
      const started = await call((t) => t.sessionStart({ project_id: record.project_id, agent_id: record.agent_id, command_id: id }));
      startId.clear();
      await chat.refreshList();
      openSession(started.session_id);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") startId.clear();
      setStartError(err.message);
    } finally {
      setStarting(false);
    }
  }

  // While the chat holds exactly one open single-question card, the composer's Send answers it (P4).
  const openQuestion = ended ? null : openSingleQuestion(items);
  const cost = costLabel(record.usage, record.cache_ttl_seconds);
  const unit = cost.lastIndexOf(" ");
  const pillState = ended ? "ended" : record.state;

  return (
    <section className="chat" aria-label="Chat">
      <div className="chat-head" role="group" aria-label="Chat header">
        <Avatar agent={agentLook(record.agent_id)} size={28} state={record.state === "needs_you" && !ended ? "needs_you" : "idle"} />
        <h2 className="chat-agent">{name}</h2>
        <span className="ui-sr-only" role="group" aria-label="Project">
          {record.project_name}
        </span>
        <StatePill state={pillState} role="group" aria-label="State">
          {ended ? "Ended" : stateLabel(record.state)}
        </StatePill>
        {unqualified ? (
          <Chip tone="amber" title="This chat runs an override binary, not the qualified Claude Code version.">
            unqualified
          </Chip>
        ) : null}
        <span className="head-spacer" />
        <Chip mono role="group" aria-label="Worktree" title={record.worktree_dir}>
          <Icon name="archive" size={12} />
          {lastSegment(record.worktree_dir)}
        </Chip>
        <Chip mono role="group" aria-label="Branch">
          <Icon name="git-branch" size={12} />
          {branchText(record, projects)}
        </Chip>
        <span className="head-rule" aria-hidden="true" />
        <span className="chat-cost" role="group" aria-label="Cost" title={dollarsTitle(record.usage)}>
          {cost.slice(0, unit)}{" "}
          <Chip tone="violet" mono>
            {cost.slice(unit + 1)}
          </Chip>
        </span>
        {ended ? (
          <Button variant="primary" icon="play" disabled={starting} onClick={() => void startAgain()}>
            Start session
          </Button>
        ) : (
          <Button onClick={() => setConfirmEnd(true)}>End session</Button>
        )}
        <IconButton icon="settings" title="Agent settings" size={28} onClick={() => openPage("agents")} />
      </div>

      {confirmEnd && !ended ? (
        <div className="end-strip" role="group" aria-label="Confirm end">
          <span className="end-icon" aria-hidden="true">
            <Icon name="triangle-alert" size={15} />
          </span>
          <div className="end-text">
            <span className="end-title">End this session?</span>
            <span className="end-note">
              {`The ${name}'s process on ${serverName} stops, mid-turn if it is working. The chat stays, and you can start the session again from here.`}
            </span>
          </div>
          <Button onClick={() => setConfirmEnd(false)}>Keep it</Button>
          <Button variant="danger-solid" onClick={() => void end()}>
            End session
          </Button>
        </div>
      ) : null}

      <StreamLine />
      {endError ? (
        <p className="problem-title chat-problem" role="alert">
          {endError}
        </p>
      ) : null}
      {lost !== null && !ended ? (
        <p className="chat-note chat-lost" role="status">
          {`Context lost: ${lost}. End this session and start a new one.`}
          {confirmEnd ? null : <Button onClick={() => setConfirmEnd(true)}>End session</Button>}
        </p>
      ) : null}

      <Messages
        items={items}
        delta={chat.deltas[sessionId] ?? ""}
        running={chat.running[sessionId] ?? {}}
        readOnly={ended}
        onAnswer={onAnswer}
        agentId={record.agent_id}
        agentName={name}
        working={record.state === "working" && !ended}
      />
      <div className="chat-foot">
        <Column max={736}>
          <Composer
            record={record}
            agentName={name}
            onStart={() => void startAgain()}
            starting={starting}
            startError={startError}
            openQuestion={openQuestion}
            onAnswer={onAnswer}
          />
        </Column>
      </div>
    </section>
  );
}
