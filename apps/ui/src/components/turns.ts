// The turns of a chat (11a). The items after an operator message, up to and including the next
// `turn_result`, are one turn. A turn opens with the first item the agent made. An item that is no part of
// a turn (the operator's message, a notice before the agent said anything) stands alone.
import type { ChatItemRecord } from "@pulse-factory/api-client";

export type Block =
  | { type: "item"; item: ChatItemRecord }
  | {
      type: "turn";
      /** The items of the turn in `seq` order, without its `turn_result`. */
      items: ChatItemRecord[];
      /** The turn's `turn_result` item, or `null` while the turn runs. */
      result: ChatItemRecord | null;
    };

/** The kinds the agent makes: the first of them after an operator message opens a turn. */
const OPENS_A_TURN = new Set(["assistant_text", "tool_call", "approval_request", "question_request", "background_task", "start_card"]);

/** Groups items (already in `seq` order) into standalone items and turns. */
export function groupTurns(items: ChatItemRecord[]): Block[] {
  const blocks: Block[] = [];
  let open: { items: ChatItemRecord[]; result: ChatItemRecord | null } | null = null;
  const close = () => {
    if (open) blocks.push({ type: "turn", ...open });
    open = null;
  };
  for (const item of items) {
    const kind = item.body.kind;
    if (kind === "operator_message") {
      // An operator message ends a turn that never got its result (a reopened stream, a lost result).
      close();
      blocks.push({ type: "item", item });
    } else if (kind === "turn_result") {
      open ??= { items: [], result: null };
      open.result = item;
      close();
    } else if (open || OPENS_A_TURN.has(kind)) {
      open ??= { items: [], result: null };
      open.items.push(item);
    } else {
      blocks.push({ type: "item", item });
    }
  }
  close();
  return blocks;
}

/** The first item of a block: its time puts the block under a day divider. */
export function firstOf(block: Block): ChatItemRecord | null {
  if (block.type === "item") return block.item;
  return block.items[0] ?? block.result;
}

/** The UTC day of a time as `YYYY-MM-DD`, or `null` when it does not parse. */
export function dayKey(text: string): string | null {
  const ms = Date.parse(text);
  return Number.isNaN(ms) ? null : new Date(ms).toISOString().slice(0, 10);
}

/** "Today", "Yesterday" or the date (`Nov 7`), against the app's clock. */
export function dayLabel(day: string, nowMs: number): string {
  const today = new Date(nowMs).toISOString().slice(0, 10);
  if (day === today) return "Today";
  const yesterday = new Date(nowMs - 86_400_000).toISOString().slice(0, 10);
  if (day === yesterday) return "Yesterday";
  return new Date(`${day}T00:00:00Z`).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });
}
