// The server's row and "Disconnect this Mac". The server menu (3b) and the Settings page show it.
import { useState } from "react";
import { useConnection } from "../state/connection";
import { Button } from "../ui";

export function ServerSection() {
  const conn = useConnection();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const up = conn.link.state === "up";
  const state = up ? "Connected" : "Not connected";
  const ms = conn.lastCallMs === null ? "no call yet" : `${conn.lastCallMs} ms`;

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
    <div className="server-section">
      <div className="server-row">
        <span className={`dot ${up ? "dot-on" : "dot-off"}`} aria-hidden="true" />
        <div className="server-text">
          <div className="server-name">{conn.server.address}</div>
          <div className="server-meta">{`${state} · ${ms} · ${conn.server.address}`}</div>
        </div>
      </div>
      <Button variant="danger" disabled={busy} onClick={() => void disconnect()}>
        Disconnect this Mac
      </Button>
      {error ? (
        <p className="problem-title" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
