// The navigation seam of round 3 (Approach 2). `Shell.tsx` provides it; the round-3 screens open a page
// or a chat only through `useNavigation()`, so none of them edits `Shell.tsx` or this directory for it.
// Each call goes through `push` as a rail click or a sidebar row does.
import { createContext, useContext } from "react";

export type PageName = "projects" | "agents" | "needs_you" | "settings";

export interface NavigationApi {
  openPage(page: PageName): void;
  openSession(sessionId: string): void;
}

export const Navigation = createContext<NavigationApi | null>(null);

export function useNavigation(): NavigationApi {
  const api = useContext(Navigation);
  if (!api) throw new Error("useNavigation outside the shell");
  return api;
}
