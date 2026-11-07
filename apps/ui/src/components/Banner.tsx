// The disconnected banner (3c). It sits over the shell. Which kind of failure it shows decides
// whether a timer runs: only `unreachable` retries by itself.
import { KEY_CHANGED_MESSAGE } from "../transport/types";
import { useConnection } from "../state/connection";
import { clock } from "./format";

export function Banner() {
  const conn = useConnection();
  const link = conn.link;
  if (link.state === "up") return null;
  const address = conn.server.address;
  const reached = conn.lastReachedAt === null ? null : clock(conn.lastReachedAt);

  switch (link.kind) {
    case "unreachable": {
      const seconds = Math.max(0, Math.ceil(((link.nextAt ?? conn.now) - conn.now) / 1000));
      return (
        <div className="banner" role="status">
          <div className="banner-text">
            <p className="banner-title">{`Not connected to ${address}`}</p>
            <p className="banner-body">
              {`Your sessions and runs keep going on the server. Retrying in ${seconds}s.`}
            </p>
          </div>
          {reached ? <span className="banner-meta">{`last reached ${reached}`}</span> : null}
          <button className="btn" type="button" onClick={conn.retryNow}>
            Retry now
          </button>
        </div>
      );
    }
    case "unauthorized":
      return (
        <div className="banner" role="status">
          <div className="banner-text">
            <p className="banner-title">{`This device is no longer paired with ${address}.`}</p>
          </div>
          <button className="btn" type="button" onClick={conn.pairAgain}>
            Pair again
          </button>
        </div>
      );
    case "key_changed":
      return (
        <div className="banner" role="status">
          <div className="banner-text">
            <p className="banner-title">{KEY_CHANGED_MESSAGE}</p>
          </div>
          <button className="btn" type="button" onClick={conn.pairAgain}>
            Pair again
          </button>
        </div>
      );
    case "rate_limited":
      return (
        <div className="banner" role="status">
          <div className="banner-text">
            <p className="banner-title">{link.message}</p>
          </div>
          <button className="btn" type="button" onClick={conn.retryNow}>
            Retry now
          </button>
        </div>
      );
  }
}
