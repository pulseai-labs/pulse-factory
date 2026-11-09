// The shell (2a to 2c): a title bar with the page title, the history arrows and the tab strip, a slim
// rail with Projects and Agents (top) and Settings (bottom), and on the Projects, Agents and chat
// pages a left sidebar. The rail shows no other section: later spines add them (Q5, A9).
//
// An open chat is a tab. No tab exists until a chat opens. The sidebar lists each project's live
// sessions; a click opens (or focuses) that chat's tab. The sidebar's open state is the only thing
// the UI keeps in localStorage.
import { useCallback, useEffect, useState } from "react";
import { Banner } from "../components/Banner";
import { StreamLine } from "../components/StreamLine";
import { ArrowLeftIcon, ArrowRightIcon, BotIcon, FolderIcon, GearIcon } from "../components/Icons";
import { ServerMenu } from "../components/ServerMenu";
import { branchText } from "../components/chat-format";
import { agentName, stateLabel, useChat } from "../state/chat";
import { useConnection } from "../state/connection";
import { Agents } from "./Agents";
import { Chat } from "./Chat";
import { Projects } from "./Projects";
import { Settings } from "./Settings";

type Page = { kind: "projects" } | { kind: "agents" } | { kind: "settings" } | { kind: "chat"; session: string };

const PROJECTS: Page = { kind: "projects" };
const AGENTS: Page = { kind: "agents" };
const SETTINGS: Page = { kind: "settings" };

function sameChat(page: Page | undefined, session: string): boolean {
  return page?.kind === "chat" && page.session === session;
}

function samePage(a: Page | undefined, b: Page): boolean {
  if (!a || a.kind !== b.kind) return false;
  return a.kind === "chat" && b.kind === "chat" ? a.session === b.session : true;
}

// The sidebar's open state is the only thing the UI keeps in localStorage.
const SIDEBAR_KEY = "pf.sidebar";

function readSidebar(): boolean {
  try {
    return window.localStorage.getItem(SIDEBAR_KEY) !== "closed";
  } catch {
    return true;
  }
}

function Sidebar(props: { onOpen: (session: string) => void; current: string | null }) {
  const { projects } = useConnection();
  const chat = useChat();
  return (
    <section className="sidebar" aria-label="Sidebar">
      <ServerMenu />
      <div className="sidebar-head">Projects</div>
      {projects === null ? null : projects.length === 0 ? (
        <p className="sidebar-empty">No projects yet</p>
      ) : (
        <ul className="sidebar-list">
          {projects.map((p) => {
            const sessions = chat.live.filter((s) => s.project_id === p.id);
            return (
              <li key={p.id} className="sidebar-project">
                <div className="sidebar-project-row" title={p.root}>
                  <span className="sidebar-icon" aria-hidden="true">
                    <FolderIcon />
                  </span>
                  <span className="sidebar-name">{p.name}</span>
                </div>
                {sessions.length === 0 ? null : (
                  <ul className="session-rows" aria-label={`Sessions of ${p.name}`}>
                    {sessions.map((s) => {
                      const agent = agentName(chat.list, s.agent_id);
                      const unqualified = s.process !== null && !s.process.qualified;
                      const branch = branchText(s, projects);
                      return (
                        <li key={s.id}>
                          <button
                            className={`session-row${props.current === s.id ? " session-row-on" : ""}`}
                            type="button"
                            aria-label={`${agent} session in ${p.name}: ${stateLabel(s.state)}, ${branch}${unqualified ? ", unqualified" : ""}`}
                            onClick={() => props.onOpen(s.id)}
                          >
                            <span className="session-agent">{agent}</span>
                            <span className={`session-state session-${s.state}`}>{stateLabel(s.state)}</span>
                            <span className="session-branch mono">{branch}</span>
                            {unqualified ? <span className="chip chip-warn">unqualified</span> : null}
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {chat.following === null ? <StreamLine /> : null}
    </section>
  );
}

export function Shell() {
  const chat = useChat();
  const { follow } = chat;
  const [history, setHistory] = useState<{ entries: Page[]; index: number }>({
    entries: [PROJECTS],
    index: 0,
  });
  const [tabs, setTabs] = useState<string[]>([]);
  const [sidebarOpen, setSidebarOpen] = useState(readSidebar);
  const page: Page = history.entries[history.index] ?? PROJECTS;
  const canBack = history.index > 0;
  const canForward = history.index < history.entries.length - 1;
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

  const go = useCallback((next: Page) => {
    setHistory((h) => {
      if (samePage(h.entries[h.index], next)) return h;
      return { entries: [...h.entries.slice(0, h.index + 1), next], index: h.index + 1 };
    });
  }, []);

  const openChat = useCallback(
    (session: string) => {
      setTabs((t) => (t.includes(session) ? t : [...t, session]));
      go({ kind: "chat", session });
    },
    [go],
  );

  // Closing a tab removes its pages from the history. The tab on screen goes back to the page before it.
  const closeTab = useCallback((session: string) => {
    setTabs((t) => t.filter((id) => id !== session));
    setHistory((h) => {
      const keep = (p: Page | undefined) => !sameChat(p, session);
      const entries = h.entries.filter(keep);
      const keptBefore = h.entries.slice(0, h.index).filter(keep).length;
      const onScreen = sameChat(h.entries[h.index], session);
      const index = onScreen ? Math.max(0, keptBefore - 1) : keptBefore;
      return { entries, index: Math.min(index, entries.length - 1) };
    });
  }, []);

  const toggleSidebar = useCallback(() => setSidebarOpen((o) => !o), []);

  const hasSidebar = page.kind !== "settings";

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

  const showSidebar = hasSidebar && sidebarOpen;
  const tip = (name: string, here: boolean) =>
    here ? `${name}: click or press ⌘B to ${sidebarOpen ? "hide" : "show"} the sidebar` : name;

  const tabName = (session: string) => {
    const record = chat.records[session];
    return `${record ? agentName(chat.list, record.agent_id) : "Orchestrator"} · ${record ? record.project_name : session}`;
  };

  const title =
    page.kind === "projects" ? "Projects" : page.kind === "agents" ? "Agents" : page.kind === "settings" ? "Settings" : "Chat";

  return (
    <div className="app">
      <header className="titlebar" role="banner">
        <button
          className="icon-btn"
          type="button"
          aria-label="Back"
          disabled={!canBack}
          onClick={() => setHistory((h) => ({ ...h, index: Math.max(0, h.index - 1) }))}
        >
          <ArrowLeftIcon />
        </button>
        <button
          className="icon-btn"
          type="button"
          aria-label="Forward"
          disabled={!canForward}
          onClick={() => setHistory((h) => ({ ...h, index: Math.min(h.entries.length - 1, h.index + 1) }))}
        >
          <ArrowRightIcon />
        </button>
        <h1 className="titlebar-title">{title}</h1>
        {tabs.length > 0 ? (
          <div className="tabs" role="tablist" aria-label="Open chats">
            {tabs.map((session) => (
              <div className={`tab-wrap${chatOnScreen === session ? " tab-on" : ""}`} key={session}>
                <button
                  className="tab"
                  type="button"
                  role="tab"
                  aria-selected={chatOnScreen === session}
                  onClick={() => go({ kind: "chat", session })}
                >
                  {tabName(session)}
                </button>
                <button
                  className="tab-close"
                  type="button"
                  aria-label={`Close ${tabName(session)}`}
                  onClick={() => closeTab(session)}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
        ) : null}
      </header>
      <Banner />
      <div className="body">
        <nav className="rail" aria-label="Sections">
          <button
            className={`rail-btn${page.kind === "projects" ? " rail-btn-on" : ""}`}
            type="button"
            aria-label="Projects"
            aria-current={page.kind === "projects" ? "page" : undefined}
            title={tip("Projects", page.kind === "projects")}
            onClick={() => (page.kind === "projects" ? toggleSidebar() : go(PROJECTS))}
          >
            <FolderIcon />
          </button>
          <button
            className={`rail-btn${page.kind === "agents" || page.kind === "chat" ? " rail-btn-on" : ""}`}
            type="button"
            aria-label="Agents"
            aria-current={page.kind === "agents" ? "page" : undefined}
            title={tip("Agents", page.kind === "agents")}
            onClick={() => (page.kind === "agents" ? toggleSidebar() : go(AGENTS))}
          >
            <BotIcon />
          </button>
          <span className="rail-spacer" />
          <button
            className={`rail-btn${page.kind === "settings" ? " rail-btn-on" : ""}`}
            type="button"
            aria-label="Settings"
            aria-current={page.kind === "settings" ? "page" : undefined}
            title="Settings"
            onClick={() => go(SETTINGS)}
          >
            <GearIcon />
          </button>
        </nav>
        {showSidebar ? <Sidebar onOpen={openChat} current={chatOnScreen} /> : null}
        <main className="main">
          {page.kind === "projects" ? (
            <Projects />
          ) : page.kind === "agents" ? (
            <Agents onOpen={openChat} />
          ) : page.kind === "settings" ? (
            <Settings />
          ) : (
            <Chat key={page.session} sessionId={page.session} />
          )}
        </main>
      </div>
    </div>
  );
}
