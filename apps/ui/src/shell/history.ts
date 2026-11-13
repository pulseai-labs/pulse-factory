// The page history of the shell (Approach 2): pure functions over `{ entries, index }`. `Shell.tsx` keeps
// the value in its state and calls these; nothing here reads a component or a call.

export type Page =
  | { kind: "projects" }
  | { kind: "agents" }
  | { kind: "needs_you" }
  | { kind: "settings" }
  | { kind: "chat"; session: string };

export interface History {
  entries: Page[];
  index: number;
}

export const PROJECTS: Page = { kind: "projects" };

export function initialHistory(): History {
  return { entries: [PROJECTS], index: 0 };
}

export function currentPage(h: History): Page {
  return h.entries[h.index] ?? PROJECTS;
}

function sameChat(page: Page | undefined, session: string): boolean {
  return page?.kind === "chat" && page.session === session;
}

export function samePage(a: Page | undefined, b: Page): boolean {
  if (!a || a.kind !== b.kind) return false;
  return a.kind === "chat" && b.kind === "chat" ? a.session === b.session : true;
}

/** A page after the one on screen. The pages ahead go. A page that is on screen already adds no entry. */
export function push(h: History, page: Page): History {
  if (samePage(h.entries[h.index], page)) return h;
  return { entries: [...h.entries.slice(0, h.index + 1), page], index: h.index + 1 };
}

export function canBack(h: History): boolean {
  return h.index > 0;
}

export function canForward(h: History): boolean {
  return h.index < h.entries.length - 1;
}

export function back(h: History): History {
  return canBack(h) ? { ...h, index: h.index - 1 } : h;
}

export function forward(h: History): History {
  return canForward(h) ? { ...h, index: h.index + 1 } : h;
}

/** Closing a tab takes the pages of that chat out. The tab on screen goes back to the page before it. */
export function closeTab(h: History, session: string): History {
  const keep = (p: Page | undefined) => !sameChat(p, session);
  const entries = h.entries.filter(keep);
  const keptBefore = h.entries.slice(0, h.index).filter(keep).length;
  const onScreen = sameChat(h.entries[h.index], session);
  const index = onScreen ? Math.max(0, keptBefore - 1) : keptBefore;
  return { entries, index: Math.min(index, entries.length - 1) };
}
