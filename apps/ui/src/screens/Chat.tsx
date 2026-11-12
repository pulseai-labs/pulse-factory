// The chat with an agent (2a): a header with the project, the worktree and the branch the server read
// live, the state, the `unqualified` mark and the cost; the message stream; and the composer. Every
// call goes through the transport, and the stream is the one the shell holds open.
import type { ChatAnswerArg } from "@pulse-factory/api-client";
import { useEffect, useMemo, useState } from "react";
import type { AnswerFn } from "../components/Cards";
import { Messages } from "../components/ChatItems";
import { Composer } from "../components/Composer";
import { StreamLine } from "../components/StreamLine";
import { branchText, lastSegment } from "../components/chat-format";
import { useCommandId } from "../components/command-id";
import { costLabel, dollarsTitle } from "../components/cost";
import { agentName, stateLabel, useChat } from "../state/chat";
import { asTransportError, useConnection } from "../state/connection";
import { Button, Chip, StatePill } from "../ui";

export function Chat(props: { sessionId: string }) {
  const { sessionId } = props;
  const { call, projects } = useConnection();
  const chat = useChat();
  const { ensureList } = chat;
  const endId = useCommandId();
  const answerId = useCommandId();
  const [confirmEnd, setConfirmEnd] = useState(false);
  const [endError, setEndError] = useState<string | null>(null);

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

  const endControls = confirmEnd ? (
    <span className="confirm" role="group" aria-label="Confirm end">
      <span>End this session?</span>
      <Button variant="danger" onClick={() => void end()}>
        Yes, end it
      </Button>
      <Button onClick={() => setConfirmEnd(false)}>
        Keep it
      </Button>
    </span>
  ) : (
    <Button onClick={() => setConfirmEnd(true)}>
      End session
    </Button>
  );

  return (
    <section className="chat" aria-label="Chat">
      <div className="chat-head" role="group" aria-label="Chat header">
        <h2 className="chat-agent">{name}</h2>
        <span className="head-key" aria-hidden="true">
          Project
        </span>
        <span className="head-value" role="group" aria-label="Project">
          {record.project_name}
        </span>
        <span className="head-key" aria-hidden="true">
          Worktree
        </span>
        <span className="head-value mono" role="group" aria-label="Worktree" title={record.worktree_dir}>
          {lastSegment(record.worktree_dir)}
        </span>
        <span className="head-key" aria-hidden="true">
          Branch
        </span>
        <span className="head-value mono" role="group" aria-label="Branch">
          {branchText(record, projects)}
        </span>
        <StatePill state={record.state} role="group" aria-label="State">
          {stateLabel(record.state)}
        </StatePill>
        {unqualified ? (
          <Chip tone="amber" title="This chat runs an override binary, not the qualified Claude Code version.">
            unqualified
          </Chip>
        ) : null}
        <span className="head-spacer" />
        <span className="head-value" role="group" aria-label="Cost" title={dollarsTitle(record.usage)}>
          {costLabel(record.usage, record.cache_ttl_seconds)}
        </span>
        {ended ? null : endControls}
      </div>

      <StreamLine />
      {endError ? (
        <p className="problem-title" role="alert">
          {endError}
        </p>
      ) : null}
      {ended ? <p className="chat-note">This session ended</p> : null}
      {lost !== null && !ended ? (
        <p className="chat-note chat-lost" role="status">
          {`Context lost: ${lost}. End this session and start a new one.`}
          {confirmEnd ? null : (
            <Button onClick={() => setConfirmEnd(true)}>
              End session
            </Button>
          )}
        </p>
      ) : null}

      <Messages
        items={items}
        delta={chat.deltas[sessionId] ?? ""}
        running={chat.running[sessionId] ?? {}}
        readOnly={ended}
        onAnswer={onAnswer}
      />
      <Composer record={record} agentName={name} />
    </section>
  );
}
