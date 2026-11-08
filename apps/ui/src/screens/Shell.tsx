// The shell (2a to 2c): a title bar with the page title and the history arrows, a slim rail with
// Projects (top) and Settings (bottom), and on the Projects page a left sidebar. The rail shows no
// other section: later spines add them (Q5). The tab strip is empty, because this spine opens no
// session, run or PR.
import { useCallback, useEffect, useState } from "react";
import { Banner } from "../components/Banner";
import { ArrowLeftIcon, ArrowRightIcon, FolderIcon, GearIcon } from "../components/Icons";
import { ServerMenu } from "../components/ServerMenu";
import { useConnection } from "../state/connection";
import { Projects } from "./Projects";
import { Settings } from "./Settings";

type Page = "projects" | "settings";
const TITLES: Record<Page, string> = { projects: "Projects", settings: "Settings" };

// The sidebar's open state is the only thing the UI keeps in localStorage.
const SIDEBAR_KEY = "pf.sidebar";

function readSidebar(): boolean {
  try {
    return window.localStorage.getItem(SIDEBAR_KEY) !== "closed";
  } catch {
    return true;
  }
}

function Sidebar() {
  const { projects } = useConnection();
  return (
    <section className="sidebar" aria-label="Sidebar">
      <ServerMenu />
      <div className="sidebar-head">Projects</div>
      {projects === null ? null : projects.length === 0 ? (
        <p className="sidebar-empty">No projects yet</p>
      ) : (
        <ul className="sidebar-list">
          {projects.map((p) => (
            <li key={p.id} title={p.root}>
              <span className="sidebar-icon" aria-hidden="true">
                <FolderIcon />
              </span>
              <span className="sidebar-name">{p.name}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function Shell() {
  const [history, setHistory] = useState<{ entries: Page[]; index: number }>({
    entries: ["projects"],
    index: 0,
  });
  const [sidebarOpen, setSidebarOpen] = useState(readSidebar);
  const page: Page = history.entries[history.index] ?? "projects";
  const canBack = history.index > 0;
  const canForward = history.index < history.entries.length - 1;

  useEffect(() => {
    try {
      window.localStorage.setItem(SIDEBAR_KEY, sidebarOpen ? "open" : "closed");
    } catch {
      // No storage: the sidebar starts open next time.
    }
  }, [sidebarOpen]);

  const go = useCallback((next: Page) => {
    setHistory((h) => {
      if (h.entries[h.index] === next) return h;
      return { entries: [...h.entries.slice(0, h.index + 1), next], index: h.index + 1 };
    });
  }, []);

  const toggleSidebar = useCallback(() => setSidebarOpen((o) => !o), []);

  // ⌘B toggles the sidebar on a page that has one.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "b") {
        e.preventDefault();
        if (page === "projects") toggleSidebar();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [page, toggleSidebar]);

  const showSidebar = page === "projects" && sidebarOpen;
  const projectsTip = page === "projects"
    ? `Projects: click or press ⌘B to ${sidebarOpen ? "hide" : "show"} the sidebar`
    : "Projects";

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
        <h1 className="titlebar-title">{TITLES[page]}</h1>
      </header>
      <Banner />
      <div className="body">
        <nav className="rail" aria-label="Sections">
          <button
            className={`rail-btn${page === "projects" ? " rail-btn-on" : ""}`}
            type="button"
            aria-label="Projects"
            aria-current={page === "projects" ? "page" : undefined}
            title={projectsTip}
            onClick={() => (page === "projects" ? toggleSidebar() : go("projects"))}
          >
            <FolderIcon />
          </button>
          <span className="rail-spacer" />
          <button
            className={`rail-btn${page === "settings" ? " rail-btn-on" : ""}`}
            type="button"
            aria-label="Settings"
            aria-current={page === "settings" ? "page" : undefined}
            title="Settings"
            onClick={() => go("settings")}
          >
            <GearIcon />
          </button>
        </nav>
        {showSidebar ? <Sidebar /> : null}
        <main className="main">{page === "projects" ? <Projects /> : <Settings />}</main>
      </div>
    </div>
  );
}
