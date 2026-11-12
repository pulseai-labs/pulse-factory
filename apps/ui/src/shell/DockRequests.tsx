// The cards of the dock. In this item it is a read-only list of the open requests of `useRequests()`: no
// call and no new state. One compact card for each request, with its run and unit, its age, the first line
// of its reason and an "Open" button that opens the Needs you page. A `merge_ack` comes first, as on the
// page (`openRequests`). In round 3 `w4` replaces the cards with the Needs you cards and their actions;
// `Dock.tsx` renders whatever this file renders.
import type { RequestSummary } from "@pulse-factory/api-client";
import { parseTime } from "../components/chat-format";
import { useRequests } from "../state/requests";
import { Button, Card } from "../ui";
import { useNavigation } from "./navigation";

/** `just now`, `12m`, `3h 05m`, `2d`. */
function age(openedAt: string): string {
  const at = parseTime(openedAt);
  if (at === null) return "";
  const minutes = Math.max(0, Math.floor((Date.now() - at) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${Math.floor(hours / 24)}d`;
}

function firstLine(reason: string): string {
  return reason.split("\n")[0]?.trim() ?? "";
}

function Request(props: { request: RequestSummary }) {
  const { request } = props;
  const { openPage } = useNavigation();
  return (
    <li>
      <Card tone="needs_you">
        <div className="dock-card">
          <div className="dock-card-head">
            <span className="dock-card-where mono">{request.unit ? `${request.run} · ${request.unit}` : request.run}</span>
            <span className="dock-age mono">{age(request.opened_at)}</span>
          </div>
          <p className="dock-card-reason">{firstLine(request.reason)}</p>
          <Button size="sm" onClick={() => openPage("needs_you")}>
            Open
          </Button>
        </div>
      </Card>
    </li>
  );
}

export function DockRequests() {
  const { open } = useRequests();
  if (open.length === 0) return <p className="dock-empty">Nothing needs you.</p>;
  return (
    <ul className="dock-list" aria-label="Open requests">
      {open.map((request) => (
        <Request key={request.id} request={request} />
      ))}
    </ul>
  );
}
