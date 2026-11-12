// The server menu (3b): it opens from the server's name at the top of the sidebar. Of the menu this
// spine builds the server row and "Disconnect this Mac"; the rest has no verb on the allowlist yet.
import { useEffect, useRef, useState } from "react";
import { useConnection } from "../state/connection";
import { Icon } from "../ui";
import { ServerSection } from "./ServerSection";

export function ServerMenu() {
  const conn = useConnection();
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const up = conn.link.state === "up";
  return (
    <div className="server-menu" ref={box}>
      <button
        className="server-button"
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        title="Server menu"
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`dot ${up ? "dot-on" : "dot-off"}`} aria-hidden="true" />
        <span className="server-button-name">{conn.server.address}</span>
        <span className="muted" aria-hidden="true">
          <Icon name="chevrons-up-down" size={14} />
        </span>
      </button>
      {open ? (
        <div className="menu" role="dialog" aria-label="Server menu">
          <div className="menu-label">Servers</div>
          <ServerSection />
        </div>
      ) : null}
    </div>
  );
}
