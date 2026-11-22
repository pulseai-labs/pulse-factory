// The banner of 12f: the last acknowledgement this app sent carried `blocked`, so macOS shows no notification of
// a new request and the app says so, at the top of the Needs you page and of the dock. It goes when a later
// acknowledgement carries `shown`. The state lives in memory only (`state/notices.tsx`). The buttons "Open System
// Settings" and "Settings" are `NotBuilt`: opening System Settings needs one more plugin and permission.
import { useServerName } from "../state/connection";
import { useTold } from "../state/notices";
import { Icon, NotBuilt } from "../ui";

export function BlockedBanner(props: { where: "page" | "dock" }) {
  const { blocked } = useTold();
  const server = useServerName();
  if (!blocked) return null;
  const page = props.where === "page";
  return (
    <div className="ny-blocked" role="status" data-where={props.where}>
      <span className="ny-state-icon">
        <Icon name="alert" size={14} />
      </span>
      <span className="ny-state-text">
        {page
          ? `macOS is blocking notifications from Pulse Factory, so new requests only show here. ${server} lists this Mac as showing none.`
          : "macOS is blocking Pulse Factory’s notifications. New requests only show here."}
      </span>
      <NotBuilt as="button" label={page ? "Open System Settings" : "Settings"} />
    </div>
  );
}
