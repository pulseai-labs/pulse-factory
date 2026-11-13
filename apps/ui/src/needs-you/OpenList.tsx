// The list of open cards (r1.s5.w4), for the page and for the dock: the merge cards first, then the other
// requests by when they opened, then the chat questions by when their last turn was. A held merge card
// leaves the list and one quiet row stays at its end ("Held on this Mac · <question> · Show"). The Open
// count is the open requests only, so the chat-question cards follow in the list without adding to it.
import type { ChatSessionRecord, ProjectRecord, RequestSummary } from "@pulse-factory/api-client";
import { agentName } from "../state/chat";
import { Button } from "../ui";
import { ChatQuestionCard, RequestCard } from "./cards";
import { fieldOf } from "./format";
import { isHeld, show } from "./held";
import type { NeedsYou } from "./useNeedsYou";

/** The sessions that wait for the operator's answer in the chat, by their last turn. */
export function chatQuestions(live: ChatSessionRecord[]): ChatSessionRecord[] {
  return live
    .filter((s) => s.state === "needs_you")
    .sort((a, b) => {
      const at = a.last_turn_at ?? "";
      const bt = b.last_turn_at ?? "";
      if (at !== bt) return at < bt ? -1 : 1;
      return a.id.localeCompare(b.id, undefined, { numeric: true });
    });
}

function HeldRow(props: { ny: NeedsYou; request: RequestSummary }) {
  const { ny, request } = props;
  const detail = ny.bodies.detailOf(request);
  const base = fieldOf("base", request, detail);
  const from = fieldOf("head_branch", request, detail) ?? ny.spineOf(request.run) ?? request.run;
  const question = base !== null ? `Merge ${from} into ${base}?` : `Merge ${from}?`;
  return (
    <li className="ny-held" data-testid="held-row">
      <span>{`Held on this Mac · ${question} · `}</span>
      <Button size="sm" variant="ghost" onClick={() => show(request.id)}>
        Show
      </Button>
    </li>
  );
}

export function OpenList(props: {
  ny: NeedsYou;
  compact: boolean;
  /** The chat-question cards: the page shows them, the dock does not. */
  chats?: { sessions: ChatSessionRecord[]; list: Parameters<typeof agentName>[0]; projects: ProjectRecord[] | null };
}) {
  const { ny, compact, chats } = props;
  const shown = ny.open.filter((r) => !isHeld(ny.held, r.id, r.revision));
  const heldRows = ny.open.filter((r) => isHeld(ny.held, r.id, r.revision) && r.kind === "merge_ack");
  return (
    <ul className={`ny-list${compact ? " ny-list-compact" : ""}`} aria-label="Open requests">
      {shown.map((r) => (
        <li key={r.id}>
          <RequestCard request={r} env={ny.envOf(r, compact)} />
        </li>
      ))}
      {chats?.sessions.map((s) => (
        <li key={s.id}>
          <ChatQuestionCard session={s} agent={agentName(chats.list, s.agent_id)} projects={chats.projects} compact={compact} />
        </li>
      ))}
      {heldRows.map((r) => (
        <HeldRow key={r.id} ny={ny} request={r} />
      ))}
    </ul>
  );
}
