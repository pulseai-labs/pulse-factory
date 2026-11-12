// The dock (11a): 328 px on the right of the Projects, Agents and chat pages. The need-you pill opens and
// closes it. Its open state is kept per device in localStorage under `pf.dock` (`open` or `closed`, read
// in a `try`, as `pf.sidebar`), and it starts closed. It holds the switch (Needs you on, as text; Files not
// built yet), the header and `DockRequests`.
import { CountBadge, Icon, NotBuilt } from "../ui";
import { useRequests } from "../state/requests";
import { DockRequests } from "./DockRequests";

export const DOCK_KEY = "pf.dock";

export function readDock(): boolean {
  try {
    return window.localStorage.getItem(DOCK_KEY) === "open";
  } catch {
    return false;
  }
}

export function writeDock(open: boolean): void {
  try {
    window.localStorage.setItem(DOCK_KEY, open ? "open" : "closed");
  } catch {
    // No storage: the dock starts closed next time.
  }
}

export function Dock() {
  const requests = useRequests();
  const count = requests.open.length;
  return (
    <aside className="dock" aria-label="Needs you">
      <div className="dock-switch">
        <span className="dock-seg dock-seg-on">Needs you</span>
        <NotBuilt as="segment" label="Files" />
      </div>
      <div className="dock-head">
        <span className="dock-head-icon">
          <Icon name="circle-dot" size={15} />
        </span>
        <span className="dock-head-title">Needs you</span>
        <CountBadge count={count} title={`${count} open`} />
        <span className="dock-head-scope">all projects</span>
      </div>
      <DockRequests />
    </aside>
  );
}
