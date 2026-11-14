// The Needs you page (11d, r1.s5.w4): one centred column of cards, the spine merge first and then the
// questions, and "Answered today" under them. Each act is one press (Approve merge, Send, Resume): behind
// the press the app asks for a fresh confirm code and spends it at once (`needs-you/useConfirmedAct.ts`),
// so the `client-api-auth` control stays and the confirm panel is gone (G1). The line that named the claim and
// the list of runs left the page: the claim shows only in Answered today, and the runs are in the sidebar.
//
// When this page calls (docs/contracts/ui-transport.md, W9): when it opens, `requests {all: true}`, `runList`
// (for the cards' spine names only), one `show` for each open request of every kind and one `show` for each
// answered row it holds no detail for; when the link comes back, the same once; on Refresh, the same (the
// Refresh of the read-error line: the header's Refresh icon button left in r1.s6.w3, W1-33, because 11d has none,
// and the page reads again when the link comes back while the stream brings new requests); a press
// makes exactly two calls (`confirmCode` and the act); Hold and Show make none. While it is on screen, a
// request frame for an open request at a revision it holds no body for costs one `show`. Nothing else: no
// timer, no interval, no focus handler, and no stream of its own: the frames come from the one stream the
// app holds (W1).
//
// `NeedsYouDock` is the same cards in the dock's 328 px width. `shell/DockRequests.tsx` renders it.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { clock } from "../components/format";
import { AnsweredRow } from "../needs-you/AnsweredRow";
import { OpenList, chatQuestions } from "../needs-you/OpenList";
import { useNeedsYou } from "../needs-you/useNeedsYou";
import { useChat } from "../state/chat";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { useRequests } from "../state/requests";
import { Column } from "../shell/Column";
import { Button } from "../ui";
import "../styles/needs-you.css";

/** The last read of `requests`: when it worked, and the line of its failure. */
interface ReadState {
  at: number | null;
  error: string | null;
}

const NEVER: ReadState = { at: null, error: null };

export function NeedsYou() {
  const { call, epoch, link, server, projects } = useConnection();
  const { apply } = useRequests();
  const chat = useChat();
  const ny = useNeedsYou();
  const { retry } = ny.bodies;
  const [read, setRead] = useState<ReadState>(NEVER);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const readRequests = useCallback(async () => {
    try {
      const list = await call((t) => t.requests({ all: true }));
      if (!live.current) return;
      apply({ type: "read", list });
      setRead({ at: Date.now(), error: null });
    } catch (e) {
      const err = asTransportError(e);
      // A disconnect shows the 3c banner. Any other failure is one error line for the sections it feeds.
      if (live.current && !isDownKind(err.kind)) setRead((r) => ({ at: r.at, error: err.message }));
    }
  }, [call, apply]);

  // The run list feeds the cards' spine names only. A failed read leaves the names as run IDs.
  const readRuns = useCallback(async () => {
    try {
      const runs = await call((t) => t.runList());
      if (live.current) apply({ type: "runs", runs });
    } catch {
      // The names fall back to the run IDs; the 3c banner shows a disconnect.
    }
  }, [call, apply]);

  // On opening the page: the requests and the runs, and (in `useNeedsYou`) a body for each card.
  useEffect(() => {
    void readRequests();
    void readRuns();
  }, [readRequests, readRuns]);

  // When the link comes back: the same once. Never on a timer.
  const lastEpoch = useRef(epoch);
  const lastLink = useRef(link.state);
  useEffect(() => {
    const cameBack = lastLink.current !== "up" && link.state === "up";
    lastLink.current = link.state;
    if (epoch === lastEpoch.current && !cameBack) return;
    lastEpoch.current = epoch;
    void readRequests();
    void readRuns();
    retry();
  }, [epoch, link.state, readRequests, readRuns, retry]);

  const refresh = useCallback(() => {
    retry();
    void readRequests();
    void readRuns();
  }, [retry, readRequests, readRuns]);

  const chats = useMemo(() => chatQuestions(chat.live), [chat.live]);
  const empty = ny.open.length === 0 && chats.length === 0;

  return (
    <Column max={820}>
      <section className="ny-page" aria-labelledby="needs-you-title">
        <div className="ny-title-row">
          <div className="ny-title-block">
            <h2 id="needs-you-title" className="ny-title">
              Needs you
            </h2>
            <p className="ny-subtitle">
              Every open question across your projects. Answer here, in the session, on your phone or from the CLI.
            </p>
          </div>
        </div>

        <section className="ny-block" aria-labelledby="ny-open">
          <div className="ny-block-head">
            <h3 id="ny-open">Open</h3>
            <span className="ny-count" data-testid="open-count">
              {ny.open.length}
            </span>
          </div>
          {read.error !== null ? (
            <>
              <p className="ny-read-error" role="alert">
                <span>{read.error}</span>
                <Button size="sm" onClick={refresh}>
                  Refresh
                </Button>
              </p>
              {read.at !== null ? <p className="ny-quiet">{`Last read at ${clock(read.at)}`}</p> : null}
            </>
          ) : null}
          {empty ? (
            <p className="ny-empty">{ny.state.loaded ? "Nothing needs you." : "Reading the requests."}</p>
          ) : (
            <OpenList ny={ny} compact={false} chats={{ sessions: chats, list: chat.list, projects }} />
          )}
        </section>

        <section className="ny-block" aria-labelledby="ny-answered">
          <div className="ny-block-head">
            <h3 id="ny-answered">Answered today</h3>
            <span className="ny-count" data-testid="answered-count">
              {ny.answered.length}
            </span>
          </div>
          {ny.answered.length === 0 ? (
            <p className="ny-empty">Nothing answered in the last day.</p>
          ) : (
            <ul className="ny-answered" aria-label="Answered today">
              {ny.answered.map((r) => (
                <AnsweredRow
                  key={r.id}
                  request={r}
                  body={ny.bodyFor(r.id)}
                  detail={ny.bodies.detailOf(r)}
                  done={ny.acts.done[r.id]}
                  notice={ny.acts.notices[r.id]}
                  spine={ny.spineOf(r.run)}
                  claim={ny.state.claim}
                  device={server.device}
                />
              ))}
            </ul>
          )}
        </section>
      </section>
    </Column>
  );
}

/** The open cards in the dock's 328 px width: the same hooks and Hold, no reads of its own. */
export function NeedsYouDock() {
  const ny = useNeedsYou();
  if (ny.open.length === 0) return <p className="dock-empty">Nothing needs you.</p>;
  return <OpenList ny={ny} compact />;
}
