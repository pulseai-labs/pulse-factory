// The Settings page: the server section, the same one the server menu (3b) shows.
import type { ApiStatus } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { ServerSection } from "../components/ServerSection";
import { asTransportError, isDownKind, useConnection } from "../state/connection";

export function Settings() {
  const { call, epoch, server } = useConnection();
  const [status, setStatus] = useState<ApiStatus | null>(null);
  const [pageError, setPageError] = useState<string | null>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const s = await call((t) => t.status());
      if (live.current) {
        setStatus(s);
        setPageError(null);
      }
    } catch (e) {
      const err = asTransportError(e);
      if (live.current) setPageError(isDownKind(err.kind) ? null : err.message);
    }
  }, [call]);

  // On opening the page, and once when the link comes back. Never on a timer.
  useEffect(() => {
    void load();
  }, [load, epoch]);

  return (
    <section className="page" aria-labelledby="settings-title">
      <div className="page-head">
        <h2 id="settings-title">Settings</h2>
      </div>
      {pageError ? (
        <p className="page-error" role="alert">
          <span>{pageError}</span>
          <button className="btn" type="button" onClick={() => void load()}>
            Retry now
          </button>
        </p>
      ) : null}
      <section className="settings-block" aria-labelledby="settings-server">
        <h3 id="settings-server">Server</h3>
        <ServerSection />
        <dl className="facts">
          <dt>This device</dt>
          <dd>{`${server.device.name} (${server.device.id})`}</dd>
          {status ? (
            <>
              <dt>Server version</dt>
              <dd className="mono">{status.version}</dd>
            </>
          ) : null}
        </dl>
      </section>
    </section>
  );
}
