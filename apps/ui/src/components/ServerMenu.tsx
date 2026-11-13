// The server menu (11h): it opens from the server chip in the title bar and sits under it. It shows the
// server block (the tile, the name, the state with the last call's time, the address), the red "Disconnect
// this Mac" row with its inline confirm, and four functions of a later release as `NotBuilt` rows. Escape
// and a click outside `boundary` (the chip and this panel) close it. "Disconnect" calls `forget()`.
import { useEffect, useState, type RefObject } from "react";
import { useConnection } from "../state/connection";
import { Button, Icon, NotBuilt } from "../ui";

export function ServerMenu(props: { boundary: RefObject<HTMLElement | null>; onClose: () => void }) {
  const { boundary, onClose } = props;
  const conn = useConnection();
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (boundary.current && !boundary.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [boundary, onClose]);

  const up = conn.link.state === "up";
  const name = conn.server.server?.name ?? conn.server.address;

  async function disconnect() {
    setBusy(true);
    setError(null);
    try {
      await conn.forget();
    } catch (e) {
      setError(e instanceof Error ? e.message : "could not disconnect");
      setBusy(false);
    }
  }

  return (
    <div className="menu" role="dialog" aria-label="Server menu">
      <div className="menu-server">
        <div className="menu-server-head">
          <span className="menu-tile" aria-hidden="true">
            <Icon name="activity" size={16} />
          </span>
          <div className="menu-server-text">
            <span className="menu-server-name">{name}</span>
            <span className="menu-server-state">
              <span className={`tb-dot tb-dot-${up ? "up" : "down"}`} aria-hidden="true" />
              {up ? "Connected" : "Not connected"}
              {conn.lastCallMs === null ? null : (
                <>
                  <span className="menu-sep">·</span>
                  <span className="mono">{`${conn.lastCallMs} ms`}</span>
                </>
              )}
            </span>
          </div>
        </div>
        <div className="menu-address">
          Address
          <span className="mono">{conn.server.address}</span>
        </div>
      </div>

      {confirm ? (
        <div className="menu-confirm" role="alertdialog" aria-label={`Disconnect this Mac from ${name}?`}>
          <span className="menu-confirm-title">{`Disconnect this Mac from ${name}?`}</span>
          <span className="menu-confirm-body">
            Sessions and runs keep going on the server. To come back, pair again with a new code.
          </span>
          <div className="menu-confirm-actions">
            <Button
              disabled={busy}
              onClick={() => {
                setConfirm(false);
                setError(null);
              }}
            >
              Cancel
            </Button>
            <button className="menu-go" type="button" disabled={busy} onClick={() => void disconnect()}>
              Disconnect
            </button>
          </div>
        </div>
      ) : (
        <button className="menu-row menu-row-danger" type="button" onClick={() => setConfirm(true)}>
          <Icon name="x" size={15} />
          <span className="menu-row-label">Disconnect this Mac</span>
        </button>
      )}
      {error ? (
        <p className="problem-title" role="alert">
          {error}
        </p>
      ) : null}

      <div className="menu-rule" />
      <span className="menu-eyebrow">Coming in a later release</span>
      <NotBuilt as="item" icon="plus" label="Add a server" />
      <NotBuilt as="item" icon="upload" label="Pair a phone" />
      <NotBuilt as="item" icon="lock" label="Paired devices" />
      <NotBuilt as="item" icon="settings" label="Server settings" />
    </div>
  );
}
