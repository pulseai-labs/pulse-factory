// The title bar (11a, 11i): one row, 42 px. From the left: the server chip, the sidebar toggle, Back and
// Forward (R1: right after the toggle and before the divider), the page title drawn as the first tab, the
// chat tabs, New tab, a spacer and the need-you pill. The Needs you and Settings pages are focused pages:
// they keep the chip, Back, Forward and the title, and hide the rest. `data-tauri-drag-region` is on the
// empty parts, and the left padding is `--titlebar-inset`, so a later change in `apps/desktop/**` can make
// room for the traffic lights without a UI change.
import { useRef, useState } from "react";
import { ServerMenu } from "../components/ServerMenu";
import { clock } from "../components/format";
import { agentName, useChat } from "../state/chat";
import { useConnection } from "../state/connection";
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
  /** Show the chat tabs, New tab and the pill: the page is not a focused page. */
  focused: boolean;
  dockOpen: boolean;
  canBack: boolean;
  canForward: boolean;
  onBack(): void;
  onForward(): void;
  onToggleSidebar(): void;
  onToggleDock(): void;
  onOpenTab(session: string): void;
  onCloseTab(session: string): void;
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

export function TitleBar(props: TitleBarProps) {
  const conn = useConnection();
  const chat = useChat();
  const requests = useRequests();
  const [menuOpen, setMenuOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  const { page } = props;

  const name = conn.server.server?.name ?? conn.server.address;
  // `down`: the link is down. It drives the chip title, the static pill and the tab dots (Approaches 1.1, 1.5, 1.7).
  // `dotRed` adds the reconnecting stream, and only the chip's dot reads it (Approach 1.1).
  const down = conn.link.state === "down";
  const dotRed = down || chat.status.state === "reconnecting";
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
          <span className={`tb-dot tb-dot-${dotRed ? "down" : "up"}`} aria-hidden="true" />
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
        <h1 className={`tb-page${page.kind === "chat" ? "" : " tb-page-on"}`}>
          <Icon name={PAGE_ICON[page.kind]} size={14} />
          {PAGE_NAME[page.kind]}
        </h1>
        {props.focused ? null : (
          <>
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
            <button className="tb-btn tb-new" type="button" title="New tab" aria-label="New tab" onClick={props.onNewTab}>
              <Icon name="plus" size={14} />
            </button>
          </>
        )}
      </div>

      <div className="tb-spacer" data-tauri-drag-region />
      {props.focused ? null : down ? (
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
