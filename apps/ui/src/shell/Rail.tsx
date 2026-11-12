// The rail (11a): 56 px, the one `navigation` landmark. In this order: Projects (`layers`), Needs you
// (`circle-dot`, with its count), Agents (`bot`; the operator decided it stays pinned until More is built),
// a spacer, More and the usage ring (both not built: 11l), and Settings. Projects is the selected item on
// the Projects page and on a chat page, because a chat belongs to the Projects section.
import { useRequests } from "../state/requests";
import { CountBadge, Icon, NotBuilt, type IconName } from "../ui";
import type { Page } from "./history";
import type { PageName } from "./navigation";

export interface RailProps {
  page: Page;
  sidebarOpen: boolean;
  onOpenPage(page: PageName): void;
  onToggleSidebar(): void;
}

export function Rail(props: RailProps) {
  const { page, sidebarOpen } = props;
  const requests = useRequests();

  // The section the page belongs to. A chat belongs to Projects.
  const section: PageName = page.kind === "chat" ? "projects" : page.kind;
  const tip = (name: string, here: boolean) =>
    here ? `${name}: click or press ⌘B to ${sidebarOpen ? "hide" : "show"} the sidebar` : name;

  const item = (name: PageName, label: string, icon: IconName, opts: { sidebar?: boolean; count?: number } = {}) => {
    const selected = section === name;
    // A click on the selected item on its own page toggles the sidebar. A chat page opens the page.
    const toggles = opts.sidebar === true && page.kind === name;
    return (
      <button
        className={`rail-btn${selected ? " rail-btn-on" : ""}`}
        type="button"
        aria-label={label}
        aria-current={selected ? "page" : undefined}
        title={opts.sidebar === true ? tip(label, toggles) : label}
        onClick={() => (toggles ? props.onToggleSidebar() : props.onOpenPage(name))}
      >
        <Icon name={icon} size={18} />
        {opts.count === undefined ? null : <CountBadge count={opts.count} title={`${opts.count} open`} />}
      </button>
    );
  };

  return (
    <nav className="rail" aria-label="Sections">
      {item("projects", "Projects", "layers", { sidebar: true })}
      {item("needs_you", "Needs you", "circle-dot", { count: requests.open.length })}
      {item("agents", "Agents", "bot", { sidebar: true })}
      <span className="rail-spacer" />
      <NotBuilt as="rail" label="More" icon="more" />
      <NotBuilt as="rail" label="Usage" ring />
      {item("settings", "Settings", "settings")}
    </nav>
  );
}
