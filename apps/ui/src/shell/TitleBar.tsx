// The title bar (11a, 11d, 11i, 11k): one row, 42 px, the same on every page. From the left: the server
// chip, the sidebar toggle, Back and Forward (R1: right after the toggle and before the divider), the
// Projects tab, the chat tabs, the tab of the page on screen when it is Agents, Needs you or Settings, New
// tab, a spacer and the need-you pill with its dock toggle. A focused page (Needs you, Settings) hides only
// the sidebar toggle (W2-2, R1-a). The active page tab carries a × as a chat tab does (NG-3); P3: it closes
// the page and shows the last chat tab the operator used, else Projects, and Projects with no chat tab open
// draws none. The Agents tab reads "<selected agent> agent" with that agent's avatar (11g, r1.s6.w3). The chip, the pill and the dock toggle follow the derived not-connected state (11i, P2).
// `data-tauri-drag-region` is on the empty parts, and the left padding is `--titlebar-inset`, so a later
// change in `apps/desktop/**` can make room for the traffic lights without a UI change.
import type { AgentRecord } from "@pulse-factory/api-client";
import { useRef, useState } from "react";
import { useSelectedAgent } from "../screens/agentSelection";
import { ServerMenu } from "../components/ServerMenu";
import { clock } from "../components/format";
import { agentName, useChat, useNotConnected } from "../state/chat";
import { useConnection, useServerName } from "../state/connection";
import { useRequests } from "../state/requests";
import { Avatar, Icon, agentLook, type IconName } from "../ui";
import type { Page } from "./history";

const PAGE_ICON: Record<Page["kind"], IconName> = {
  projects: "layers",
  agents: "bot",
  needs_you: "circle-dot",
  settings: "settings",
  // A chat belongs to the Projects section.
  chat: "layers",
};

const PAGE_NAME: Record<Page["kind"], string> = {
  projects: "Projects",
  agents: "Agents",
  needs_you: "Needs you",
  settings: "Settings",
  chat: "Projects",
};

export interface TitleBarProps {
  page: Page;
  tabs: string[];
  /** Show the toggle: the page has a sidebar. */
  hasSidebar: boolean;
  sidebarOpen: boolean;
  dockOpen: boolean;
  canBack: boolean;
  canForward: boolean;
  onBack(): void;
  onForward(): void;
  onToggleSidebar(): void;
  onToggleDock(): void;
  onOpenTab(session: string): void;
  onCloseTab(session: string): void;
  /** The Projects tab, when another page is on screen. */
  onOpenProjects(): void;
  /** The × of the active page tab (P3). */
  onClosePage(): void;
  onNewTab(): void;
}

/** The sidebar and dock glyph of 11a: a 16×13 frame with a 4 px bar on one side. */
function Glyph(props: { side: "left" | "right" }) {
  return (
    <span className={`tb-glyph tb-glyph-${props.side}`} aria-hidden="true">
      <span className="tb-glyph-bar" />
    </span>
  );
}

/**
 * A page tab (Projects, Agents, Needs you, Settings): the active one is the page's heading and may carry a ×.
 * The Agents tab with an `agent` reads "<name> agent" with that agent's avatar in place of the bot icon (11g, W1-29);
 * with no agent listed yet it reads "Agents" with the bot icon.
 */
function PageTab(props: { kind: Page["kind"]; active: boolean; closable: boolean; agent?: AgentRecord; onOpen?: () => void; onClose: () => void }) {
  const { kind, active, closable, agent } = props;
  const label = kind === "agents" && agent !== undefined ? `${agent.name} agent` : PAGE_NAME[kind];
  const glyph =
    kind === "agents" && agent !== undefined ? <Avatar agent={agentLook(agent.id)} size={16} /> : <Icon name={PAGE_ICON[kind]} size={14} />;
  return (
    <div className={`tb-tab tb-pagetab${active ? " tb-tab-on" : ""}${active && closable ? " tb-pagetab-x" : ""}`}>
      {active ? (
        <h1 className="tb-page tb-page-on">
          {glyph}
          {label}
        </h1>
      ) : (
        <button className="tb-tab-main" type="button" onClick={props.onOpen}>
          {glyph}
          {label}
        </button>
      )}
      {active && closable ? (
        <button className="tb-tab-close" type="button" aria-label={`Close ${label}`} title={`Close ${label}`} onClick={props.onClose}>
          <Icon name="x" size={12} />
        </button>
      ) : null}
    </div>
  );
}

export function TitleBar(props: TitleBarProps) {
  const conn = useConnection();
  const chat = useChat();
  const requests = useRequests();
  const [menuOpen, setMenuOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  const { page } = props;
  const agent = useSelectedAgent(chat.list?.agents ?? []);

  const name = useServerName();
  // `down`: the link is down. It drives the tab dots (Approaches 1.1, 1.5, 1.7).
  // `notConnected` is the derived state of 11i: the link is down or the stream reconnects (r1.s6.w1, P2). It
  // drives the chip's dot and the grey pill with no toggle. The chip's title and the tab dots keep to `down`.
  const down = conn.link.state === "down";
  const notConnected = useNotConnected();
  const reached = conn.lastReachedAt === null ? null : clock(conn.lastReachedAt);
  const chip = down
    ? "Not connected"
    : `Connected to ${name}${conn.lastCallMs === null ? "" : ` · ${conn.lastCallMs} ms`}`;
  const count = requests.open.length;
  const chatOnScreen = page.kind === "chat" ? page.session : null;

  const tabName = (session: string) => {
    const record = chat.records[session];
    return `${record ? agentName(chat.list, record.agent_id) : "Orchestrator"} · ${record ? record.project_name : session}`;
  };
  const tabDot = (session: string) => {
    if (down) return "down";
    const record = chat.records[session];
    if (!record || record.ended_at !== null) return "ended";
    return record.state;
  };

  return (
    <header className="titlebar" role="banner" data-tauri-drag-region>
      <div className="tb-server" ref={anchor}>
        <button
          className="tb-chip"
          type="button"
          title={chip}
          aria-haspopup="dialog"
          aria-expanded={menuOpen}
          onClick={() => setMenuOpen((o) => !o)}
        >
          <span className={`tb-dot tb-dot-${notConnected ? "down" : "up"}`} aria-hidden="true" />
          {name}
          <span className="tb-chip-caret" aria-hidden="true">
            <Icon name="chevrons-up-down" size={12} />
          </span>
        </button>
        {menuOpen ? <ServerMenu boundary={anchor} onClose={() => setMenuOpen(false)} /> : null}
      </div>

      {props.hasSidebar ? (
        <button
          className="tb-btn"
          type="button"
          title="Show or hide the sidebar"
          aria-label="Show or hide the sidebar"
          aria-pressed={props.sidebarOpen}
          onClick={props.onToggleSidebar}
        >
          <Glyph side="left" />
        </button>
      ) : null}
      <button className="tb-btn" type="button" title="Back" aria-label="Back" disabled={!props.canBack} onClick={props.onBack}>
        <Icon name="arrow-left" size={14} />
      </button>
      <button className="tb-btn" type="button" title="Forward" aria-label="Forward" disabled={!props.canForward} onClick={props.onForward}>
        <Icon name="arrow-right" size={14} />
      </button>
      <span className="tb-divider" aria-hidden="true" />

      <div className="tb-strip" data-tauri-drag-region>
        <PageTab
          kind="projects"
          active={page.kind === "projects"}
          closable={props.tabs.length > 0}
          onOpen={props.onOpenProjects}
          onClose={props.onClosePage}
        />
        {props.tabs.length > 0 ? (
          <div className="tb-tabs" role="tablist" aria-label="Open chats">
            {props.tabs.map((session) => {
              const record = chat.records[session];
              return (
                <div className={`tb-tab${chatOnScreen === session ? " tb-tab-on" : ""}`} key={session}>
                  <button
                    className="tb-tab-main"
                    type="button"
                    role="tab"
                    aria-selected={chatOnScreen === session}
                    onClick={() => props.onOpenTab(session)}
                  >
                    <Avatar agent={agentLook(record?.agent_id ?? "orchestrator")} size={16} />
                    {tabName(session)}
                    <span className={`tb-tab-dot tb-tab-dot-${tabDot(session)}`} aria-hidden="true" />
                  </button>
                  <button
                    className="tb-tab-close"
                    type="button"
                    aria-label={`Close ${tabName(session)}`}
                    title={`Close ${tabName(session)}`}
                    onClick={() => props.onCloseTab(session)}
                  >
                    <Icon name="x" size={12} />
                  </button>
                </div>
              );
            })}
          </div>
        ) : null}
        {page.kind === "agents" || page.kind === "needs_you" || page.kind === "settings" ? (
          <PageTab kind={page.kind} active closable agent={page.kind === "agents" ? agent : undefined} onClose={props.onClosePage} />
        ) : null}
        <button className="tb-btn tb-new" type="button" title="New tab" aria-label="New tab" onClick={props.onNewTab}>
          <Icon name="plus" size={14} />
        </button>
      </div>

      <div className="tb-spacer" data-tauri-drag-region />
      {notConnected ? (
        <span className="tb-pill tb-pill-stale" title={reached === null ? undefined : `Counts as of ${reached}`}>
          <Icon name="circle-dot" size={14} />
          {`${count} need you${reached === null ? "" : ` · ${reached}`}`}
        </span>
      ) : (
        <button
          className={`tb-pill${props.dockOpen ? " tb-pill-on" : ""}`}
          type="button"
          title="Show or hide Needs you"
          aria-pressed={props.dockOpen}
          onClick={props.onToggleDock}
        >
          <Icon name="circle-dot" size={14} />
          <span className="tb-pill-text">{`${count} need you`}</span>
          <span className="tb-pill-rule" aria-hidden="true" />
          <Glyph side="right" />
        </button>
      )}
    </header>
  );
}
