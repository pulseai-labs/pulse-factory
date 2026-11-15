// The not-connected banner (11i): full width under the title bar, a red dot, the title, the body, "last
// reached HH:MM" when known, and its button. It shows for the derived state "not connected" (r1.s6.w1, P2):
// while `link.state` is `down`, and while the chat stream is `reconnecting`, so the operator sees at once
// that the server cannot be reached. The countdown is the link's next retry when the link is down, else the
// stream's `retry_in_ms` (`pf-client` retries the stream by itself). The countdown is a display tick: it
// makes no call. Which kind of link failure it shows decides whether a timer runs: only `unreachable`
// retries by itself.
import { useEffect, useState, type ReactNode } from "react";
import { KEY_CHANGED_MESSAGE } from "../transport/types";
import { useChat } from "../state/chat";
import { useConnection, useServerName } from "../state/connection";
import { clock } from "./format";
import { Button } from "../ui";

function Shape(props: { title: string; body?: string; reached: string | null; children: ReactNode }) {
  return (
    <div className="banner" role="status">
      <span className="banner-dot" aria-hidden="true" />
      <p className="banner-title">{props.title}</p>
      {props.body ? <p className="banner-body">{props.body}</p> : null}
      <span className="banner-spacer" />
      {props.reached ? <span className="banner-meta">{`last reached ${props.reached}`}</span> : null}
      {props.children}
    </div>
  );
}

export function Banner() {
  const conn = useConnection();
  const chat = useChat();
  const name = useServerName();
  const link = conn.link;
  const stream = chat.status;
  const reached = conn.lastReachedAt === null ? null : clock(conn.lastReachedAt);

  // The display tick of the stream's countdown. It runs only while the stream is reconnecting and the link is up
  // (a down link counts on `conn.now`), and it calls nothing.
  const streamRetrying = link.state === "up" && stream.state === "reconnecting";
  const [tick, setTick] = useState(() => Date.now());
  useEffect(() => {
    if (!streamRetrying) return;
    setTick(Date.now());
    const id = setInterval(() => setTick(Date.now()), 1000);
    return () => clearInterval(id);
  }, [streamRetrying]);

  if (link.state === "up") {
    if (stream.state !== "reconnecting") return null;
    const seconds = Math.max(0, Math.ceil((stream.at + stream.retryInMs - Math.max(tick, stream.at)) / 1000));
    return (
      <Shape
        title={`Not connected to ${name}`}
        body={`Your sessions and runs keep going on the server. Retrying in ${seconds}s.`}
        reached={reached}
      >
        <Button size="sm" icon="refresh" onClick={chat.reconnect}>
          Retry now
        </Button>
      </Shape>
    );
  }

  switch (link.kind) {
    case "unreachable": {
      const seconds = Math.max(0, Math.ceil(((link.nextAt ?? conn.now) - conn.now) / 1000));
      return (
        <Shape
          title={`Not connected to ${name}`}
          body={`Your sessions and runs keep going on the server. Retrying in ${seconds}s.`}
          reached={reached}
        >
          <Button size="sm" icon="refresh" onClick={conn.retryNow}>
            Retry now
          </Button>
        </Shape>
      );
    }
    case "unauthorized":
      return (
        <Shape title={`This device is no longer paired with ${conn.server.address}.`} reached={reached}>
          <Button size="sm" onClick={conn.pairAgain}>
            Pair again
          </Button>
        </Shape>
      );
    case "key_changed":
      return (
        <Shape title={KEY_CHANGED_MESSAGE} reached={reached}>
          <Button size="sm" onClick={conn.pairAgain}>
            Pair again
          </Button>
        </Shape>
      );
    case "rate_limited":
      return (
        <Shape title={link.message} reached={reached}>
          <Button size="sm" icon="refresh" onClick={conn.retryNow}>
            Retry now
          </Button>
        </Shape>
      );
  }
}
