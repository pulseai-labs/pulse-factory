// The not-connected banner (11i): full width under the title bar, a red dot, the title, the body, "last
// reached HH:MM" when known, and its button. It shows while `link.state` is `down`, and while the chat
// stream is `reconnecting` (read only), so the operator sees at once that the server cannot be reached.
// Which kind of failure it shows decides whether a timer runs: only `unreachable` retries by itself.
import type { ReactNode } from "react";
import { KEY_CHANGED_MESSAGE } from "../transport/types";
import { useChat } from "../state/chat";
import { useConnection } from "../state/connection";
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
  const link = conn.link;
  const address = conn.server.address;
  const name = conn.server.server?.name ?? address;
  const reached = conn.lastReachedAt === null ? null : clock(conn.lastReachedAt);

  if (link.state === "up") {
    if (chat.status.state !== "reconnecting") return null;
    return (
      <Shape
        title={`Not connected to ${name}`}
        body="Your sessions and runs keep going on the server. Reconnecting…"
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
        <Shape title={`This device is no longer paired with ${address}.`} reached={reached}>
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
