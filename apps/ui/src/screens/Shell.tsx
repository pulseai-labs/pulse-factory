// The shell (11a): it composes the parts in `src/shell/` (the title bar, the rail, the sidebar, the dock)
// around the page on screen, and it keeps the page state: the history, the open chats as tabs, and the
// open state of the sidebar and the dock. The Needs you and Settings pages are focused pages: no sidebar,
// no dock, no tab strip. The Needs you badge counts the open requests that the one stream's frames
// brought, with no call (r1.s4.w3).
//
// An open chat is a tab. No tab exists until a chat opens. The sidebar and the Projects page open (or
// focus) a chat's tab. Two things stay in localStorage, per device: the sidebar's open state
// (`pf.sidebar`) and the dock's (`pf.dock`). Round-3 screens open a page or a chat only through
// `useNavigation()` (`shell/navigation.ts`), which this component provides.
import { useCallback, useEffect, useMemo, useState } from "react";
import { Banner } from "../components/Banner";
import { useChat } from "../state/chat";
import { useConnection } from "../state/connection";
import { Dock, readDock, writeDock } from "../shell/Dock";
import { back, canBack, canForward, closeTab, currentPage, forward, initialHistory, push, type Page } from "../shell/history";
import { Navigation, type NavigationApi } from "../shell/navigation";
import { Rail } from "../shell/Rail";
import { Sidebar, type Grouping } from "../shell/Sidebar";
import { TitleBar } from "../shell/TitleBar";
import { Agents } from "./Agents";
import { Chat } from "./Chat";
import { NeedsYou } from "./NeedsYou";
import { Projects } from "./Projects";
import { Settings } from "./Settings";

const SIDEBAR_KEY = "pf.sidebar";

function readSidebar(): boolean {
  try {
    return window.localStorage.getItem(SIDEBAR_KEY) !== "closed";
  } catch {
    return true;
  }
}

export function Shell() {
  const chat = useChat();
  const { link } = useConnection();
  const { follow } = chat;
  const [history, setHistory] = useState(initialHistory);
  const [tabs, setTabs] = useState<string[]>([]);
  const [sidebarOpen, setSidebarOpen] = useState(readSidebar);
  const [dockOpen, setDockOpen] = useState(readDock);
  const [grouping, setGrouping] = useState<Grouping>("project");
  const page: Page = currentPage(history);
  const chatOnScreen = page.kind === "chat" ? page.session : null;

  useEffect(() => {
    try {
      window.localStorage.setItem(SIDEBAR_KEY, sidebarOpen ? "open" : "closed");
    } catch {
      // No storage: the sidebar starts open next time.
    }
  }, [sidebarOpen]);

  // The stream follows the chat on screen, and goes back to the overview when there is none.
  useEffect(() => {
    follow(chatOnScreen);
  }, [chatOnScreen, follow]);

  const go = useCallback((next: Page) => setHistory((h) => push(h, next)), []);

  const openChat = useCallback(
    (session: string) => {
      setTabs((t) => (t.includes(session) ? t : [...t, session]));
      go({ kind: "chat", session });
    },
    [go],
  );

  // Closing a tab removes its pages from the history. The tab on screen goes back to the page before it.
  const closeChat = useCallback((session: string) => {
    setTabs((t) => t.filter((id) => id !== session));
    setHistory((h) => closeTab(h, session));
  }, []);

  const toggleSidebar = useCallback(() => setSidebarOpen((o) => !o), []);
  // The dock's state is written when the operator changes it, not on mount: a device that never opened the dock
  // keeps no `pf.dock` (the d19 journey asserts that `pf.sidebar` is the only key after a chat).
  const toggleDock = useCallback(
    () =>
      setDockOpen((o) => {
        writeDock(!o);
        return !o;
      }),
    [],
  );

  const navigation = useMemo<NavigationApi>(
    () => ({ openPage: (name) => go({ kind: name }), openSession: openChat }),
    [go, openChat],
  );

  const focused = page.kind === "settings" || page.kind === "needs_you";
  const hasSidebar = !focused;

  // ⌘B toggles the sidebar on a page that has one.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "b") {
        e.preventDefault();
        if (hasSidebar) toggleSidebar();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hasSidebar, toggleSidebar]);

  return (
    <Navigation.Provider value={navigation}>
      <div className={`app${link.state === "down" ? " app-down" : ""}`}>
        <TitleBar
          page={page}
          tabs={tabs}
          hasSidebar={hasSidebar}
          sidebarOpen={sidebarOpen}
          focused={focused}
          dockOpen={dockOpen}
          canBack={canBack(history)}
          canForward={canForward(history)}
          onBack={() => setHistory(back)}
          onForward={() => setHistory(forward)}
          onToggleSidebar={toggleSidebar}
          onToggleDock={toggleDock}
          onOpenTab={openChat}
          onCloseTab={closeChat}
          onNewTab={() => go({ kind: "projects" })}
        />
        <Banner />
        <div className="body">
          <Rail page={page} sidebarOpen={sidebarOpen} onOpenPage={(name) => go({ kind: name })} onToggleSidebar={toggleSidebar} />
          {hasSidebar && sidebarOpen ? (
            <Sidebar page={page} current={chatOnScreen} grouping={grouping} onGrouping={setGrouping} onOpen={openChat} />
          ) : null}
          <main className="main">
            {page.kind === "projects" ? (
              <Projects />
            ) : page.kind === "agents" ? (
              <Agents onOpen={openChat} />
            ) : page.kind === "needs_you" ? (
              <NeedsYou />
            ) : page.kind === "settings" ? (
              <Settings />
            ) : (
              <Chat key={page.session} sessionId={page.session} />
            )}
          </main>
          {!focused && dockOpen ? <Dock /> : null}
        </div>
      </div>
    </Navigation.Provider>
  );
}
