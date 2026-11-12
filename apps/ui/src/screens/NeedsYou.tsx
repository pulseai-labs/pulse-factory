// The Needs you page (3f, functional; the look is r1.s5's): every open request across the runs with the
// spine merge approval on top, the current claim, the runs with their state, and the requests answered
// in the last day. The operator answers a stop-rule or gap question with a typed answer, or confirms the
// spine merge, each in two clicks through the confirm dialog.
//
// When this page calls (docs/contracts/ui-transport.md, W9): when it opens, `requests {all: true}`,
// `runList` and one `show` for each open request that takes `answer`; when the link comes back, the same
// once; on Refresh, `requests` and `runList`; after an `ack`, `runList` once. While it is on screen, a
// request frame that brings an open request that takes `answer` at a revision it holds no body for
// costs one `show`. Nothing else: no timer, no interval, no focus handler, and no stream of its own:
// the frames come from the one stream the app holds (W1).
import type { RequestActed, RequestSummary } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { ConfirmDialog, type DialogKind } from "../components/ConfirmDialog";
import { clock } from "../components/format";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { answeredRecently, bodyKey, useRequests } from "../state/requests";
import { Button, Chip } from "../ui";

/** The last read of one thing: when it worked, and the line of its failure. */
interface ReadState {
  at: number | null;
  error: string | null;
}

const NEVER: ReadState = { at: null, error: null };

interface Gap {
  section: string;
  question: string;
  severity: string;
}

/** The question of a `stop` or `gaps` request, from its body: the text is data and renders as text only. */
function questionOf(body: unknown): { stop: string | null; gaps: Gap[] } {
  if (typeof body !== "object" || body === null) return { stop: null, gaps: [] };
  const b = body as Record<string, unknown>;
  const stop = typeof b.stop_text === "string" ? b.stop_text : null;
  const gaps: Gap[] = [];
  if (Array.isArray(b.gaps)) {
    for (const g of b.gaps) {
      if (typeof g !== "object" || g === null) continue;
      const r = g as Record<string, unknown>;
      gaps.push({
        section: typeof r.section === "string" ? r.section : "",
        question: typeof r.question === "string" ? r.question : "",
        severity: typeof r.severity === "string" ? r.severity : "",
      });
    }
  }
  return { stop, gaps };
}

function ReadError(props: { read: ReadState; onRefresh(): void }) {
  const { read } = props;
  if (read.error === null) return null;
  return (
    <>
      <p className="page-error" role="alert">
        <span>{read.error}</span>
        <Button onClick={props.onRefresh}>
          Refresh
        </Button>
      </p>
      {read.at !== null ? <p className="muted">{`Last read at ${clock(read.at)}`}</p> : null}
    </>
  );
}

interface Frozen {
  kind: DialogKind;
  request: RequestSummary;
  text: string;
}

function RequestCard(props: {
  request: RequestSummary;
  body: unknown;
  bodyError: string | null;
  /** The page's one dialog is open for this request: its answer text is locked. */
  locked: boolean;
  /** Any dialog is open: no other act starts. */
  busy: boolean;
  onRefresh(): void;
  onOpenDialog(frozen: Frozen): void;
}) {
  const { request } = props;
  const [text, setText] = useState("");
  const takesAnswer = request.verbs.includes("answer");
  const takesAck = request.verbs.includes("ack");
  const { stop, gaps } = questionOf(props.body);

  return (
    <article className="entry-card" aria-label={`Request ${request.id}`}>
      <dl className="facts">
        <dt>Kind</dt>
        <dd>{request.kind}</dd>
        <dt>Run</dt>
        <dd>{request.run}</dd>
        <dt>Unit</dt>
        <dd>{request.unit ?? "none"}</dd>
        <dt>Stage</dt>
        <dd>{request.stage}</dd>
        <dt>Reason</dt>
        <dd>{request.reason}</dd>
        <dt>Opened</dt>
        <dd className="mono">{request.opened_at}</dd>
        {request.kind === "merge_ack" ? (
          <>
            <dt>PR</dt>
            <dd className="mono">{request.pr ?? ""}</dd>
            <dt>Base</dt>
            <dd className="mono">{request.base ?? ""}</dd>
            <dt>Head SHA</dt>
            <dd className="mono">{request.head_sha ?? ""}</dd>
          </>
        ) : null}
      </dl>
      {takesAnswer ? (
        <>
          {stop !== null ? (
            <p className="question" data-testid="stop-text">
              {stop}
            </p>
          ) : null}
          {gaps.length > 0 ? (
            <ul className="question-options" aria-label={`Gaps of ${request.id}`}>
              {gaps.map((g, i) => (
                <li key={i}>
                  <span>{g.section}</span> <span>{g.question}</span> <Chip>{g.severity}</Chip>
                </li>
              ))}
            </ul>
          ) : null}
          {props.bodyError !== null ? (
            <p className="page-error" role="alert">
              <span>{props.bodyError}</span>
              <Button onClick={props.onRefresh}>
                Refresh
              </Button>
            </p>
          ) : null}
          <label className="composer-label">
            Your answer
            <textarea
              className="field-area"
              value={text}
              readOnly={props.locked}
              rows={3}
              onChange={(e) => setText(e.target.value)}
            />
          </label>
          <div className="card-actions">
            <Button
              disabled={text.trim() === "" || props.busy}
              onClick={() => props.onOpenDialog({ kind: "answer", request, text })}
            >
              Send answer
            </Button>
          </div>
        </>
      ) : null}
      {takesAck ? (
        <div className="card-actions">
          <Button
            variant="primary"
            size="lg"
            disabled={props.busy}
            onClick={() => props.onOpenDialog({ kind: "merge", request, text: "" })}
          >
            Confirm merge
          </Button>
        </div>
      ) : null}
    </article>
  );
}

function runState(run: { state: string; halted_reason: string | null; end_reason: string | null; waiting: string | null }): string {
  if (run.state === "ended") return `Done: ${run.end_reason ?? ""}`;
  if (run.halted_reason !== null) return `Halted: ${run.halted_reason}`;
  // The run's own text, from its workflow: the app never spells it (C12).
  if (run.waiting !== null) return `Waiting on you: ${run.waiting}`;
  return "Running";
}

export function NeedsYou() {
  const { call, epoch, link } = useConnection();
  const { state, open, apply } = useRequests();
  const [requestsRead, setRequestsRead] = useState<ReadState>(NEVER);
  const [runsRead, setRunsRead] = useState<ReadState>(NEVER);
  const [bodyErrors, setBodyErrors] = useState<Record<string, string>>({});
  // What this page's own acts did, by request ID: the outcome, or that it was done before.
  const [notes, setNotes] = useState<Record<string, string>>({});
  // A bump asks the body effect to try again after a Refresh.
  const [bodyTick, setBodyTick] = useState(0);
  // The one dialog of the page. It lives here, not in a card: a request frame can close the request while
  // its spend is in flight (the controller pushes the frame once the act is admitted, before an ack's
  // merge answers), and the card then leaves Open requests. The dialog stays until its answer arrives.
  const [dialog, setDialog] = useState<Frozen | null>(null);
  const closeDialog = useCallback(() => setDialog(null), []);
  const requested = useRef(new Set<string>());
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
      if (!live.current) return null;
      apply({ type: "read", list });
      setRequestsRead({ at: Date.now(), error: null });
      return list;
    } catch (e) {
      const err = asTransportError(e);
      // A disconnect shows the 3c banner. Any other failure is one error line for the sections it feeds.
      if (live.current && !isDownKind(err.kind)) setRequestsRead((r) => ({ at: r.at, error: err.message }));
      return null;
    }
  }, [call, apply]);

  const readRuns = useCallback(async () => {
    try {
      const runs = await call((t) => t.runList());
      if (!live.current) return;
      apply({ type: "runs", runs });
      setRunsRead({ at: Date.now(), error: null });
    } catch (e) {
      const err = asTransportError(e);
      if (live.current && !isDownKind(err.kind)) setRunsRead((r) => ({ at: r.at, error: err.message }));
    }
  }, [call, apply]);

  const loadBody = useCallback(
    async (request: RequestSummary) => {
      const key = bodyKey(request.id, request.revision);
      if (requested.current.has(key)) return;
      requested.current.add(key);
      try {
        const detail = await call((t) => t.show({ id: request.id }));
        if (!live.current) return;
        apply({ type: "body", detail });
        setBodyErrors((m) => {
          if (!(request.id in m)) return m;
          const rest = { ...m };
          delete rest[request.id];
          return rest;
        });
      } catch (e) {
        requested.current.delete(key);
        const err = asTransportError(e);
        if (live.current && !isDownKind(err.kind)) setBodyErrors((m) => ({ ...m, [request.id]: err.message }));
      }
    },
    [call, apply],
  );

  // On opening the page: the requests, the runs, and (below) a body for each open request that takes `answer`.
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
  }, [epoch, link.state, readRequests, readRuns]);

  // A body for each open request that takes `answer`, at the revision it has now: at open, and when a
  // frame brings a new one. A failed `show` is tried again on Refresh.
  useEffect(() => {
    for (const r of open) {
      if (r.verbs.includes("answer") && !state.bodies[bodyKey(r.id, r.revision)]) void loadBody(r);
    }
  }, [open, state.bodies, loadBody, bodyTick]);

  const refresh = useCallback(() => {
    for (const key of [...requested.current]) {
      if (!state.bodies[key]) requested.current.delete(key);
    }
    setBodyErrors({});
    setBodyTick((n) => n + 1);
    void readRequests();
    void readRuns();
  }, [readRequests, readRuns, state.bodies]);

  const note = useCallback((id: string, text: string) => setNotes((n) => ({ ...n, [id]: text })), []);

  const onActed = useCallback(
    (request: RequestSummary, acted: RequestActed) => {
      apply({ type: "acted", acted });
      if (acted.verb === "ack") {
        note(
          request.id,
          acted.merged === true
            ? `Merged at ${acted.head_sha ?? ""}${acted.outcome ? ` (${acted.outcome})` : ""}`
            : `Not merged: ${acted.outcome ?? "the merge did not land"}`,
        );
        void readRuns();
      } else {
        note(request.id, `Answered under claim ${acted.generation ?? ""}`);
      }
    },
    [apply, note, readRuns],
  );

  // A replay acts on nothing and says nothing of its outcome: read the request as the server has it.
  const onReplayed = useCallback(
    (request: RequestSummary, acted: RequestActed) => {
      void (async () => {
        const list = await readRequests();
        const row = list?.requests.find((r) => r.id === request.id);
        if (row && row.status !== "open") note(request.id, `Done before: ${row.status} at ${row.answered_at ?? ""}`);
        if (acted.verb === "ack") await readRuns();
      })();
    },
    [readRequests, readRuns, note],
  );

  const onSubjectMoved = useCallback(() => {
    void readRequests();
  }, [readRequests]);

  const answered = answeredRecently(state, Date.now());
  const claim = state.claim;
  const runs = state.runs?.runs ?? [];

  return (
    <section className="page" aria-labelledby="needs-you-title">
      <div className="page-head">
        <h2 id="needs-you-title">Needs you</h2>
        <span className="muted">{`${open.length} open`}</span>
        <Button onClick={refresh}>
          Refresh
        </Button>
      </div>
      <dl className="facts">
        <dt>The claim</dt>
        <dd>
          {claim === null
            ? "No claim"
            : `${claim.holder}, generation ${claim.generation}${claim.revoked ? ", revoked" : ""}`}
        </dd>
      </dl>

      <section className="settings-block" aria-labelledby="ny-open">
        <h3 id="ny-open">Open requests</h3>
        <ReadError read={requestsRead} onRefresh={refresh} />
        {open.length === 0 ? (
          <p className="empty">{state.loaded ? "Nothing needs you." : "Reading the requests."}</p>
        ) : (
          <ul className="project-list" aria-label="Open requests">
            {open.map((r) => {
              const detail = state.bodies[bodyKey(r.id, r.revision)];
              return (
                <li key={r.id}>
                  <RequestCard
                    request={r}
                    body={detail?.body}
                    bodyError={bodyErrors[r.id] ?? null}
                    locked={dialog?.request.id === r.id}
                    busy={dialog !== null}
                    onRefresh={refresh}
                    onOpenDialog={setDialog}
                  />
                  {notes[r.id] ? <p className="card-outcome">{notes[r.id]}</p> : null}
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <section className="settings-block" aria-labelledby="ny-runs">
        <h3 id="ny-runs">Runs</h3>
        <ReadError read={runsRead} onRefresh={refresh} />
        {runs.length === 0 ? (
          <p className="empty">{state.runs === null ? "Reading the runs." : "No runs."}</p>
        ) : (
          <ul className="project-list" aria-label="Runs">
            {runs.map((run) => (
              <li key={run.id}>
                <article className="entry-card" aria-label={`Run ${run.id}`}>
                  <dl className="facts">
                    <dt>Spine</dt>
                    <dd>{run.spine}</dd>
                    <dt>State</dt>
                    <dd>{runState(run)}</dd>
                  </dl>
                </article>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="settings-block" aria-labelledby="ny-answered">
        <h3 id="ny-answered">Answered</h3>
        {answered.length === 0 ? (
          <p className="empty">Nothing answered in the last day.</p>
        ) : (
          <ul className="project-list" aria-label="Answered">
            {answered.map((r) => (
              <li key={r.id}>
                <span>{r.kind}</span> <strong>{r.id}</strong> <span>{r.run}</span> <span>{r.status}</span>{" "}
                <span className="mono">{r.answered_at ?? ""}</span>
                {notes[r.id] ? (
                  <>
                    {" "}
                    <span className="card-outcome">{notes[r.id]}</span>
                  </>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </section>
      {dialog ? (
        <ConfirmDialog
          kind={dialog.kind}
          request={dialog.request}
          text={dialog.text}
          onActed={(acted) => onActed(dialog.request, acted)}
          onReplayed={(acted) => onReplayed(dialog.request, acted)}
          onSubjectMoved={onSubjectMoved}
          onClose={closeDialog}
        />
      ) : null}
    </section>
  );
}
