// The one-press act (r1.s5.w4, Approach 1; ruling G1). One hook for Approve merge, Send and Resume. A press
// runs two calls in order: `confirmCode` with a fresh command ID and the act's subject, then the act (`ack`,
// `answer` or `clear`) with that code and a second fresh command ID. The confirm panel is gone, so the
// `client-api-auth` control (a fresh, one-use confirm code bound to this device and this act) stays as a
// step the app takes behind the press.
//
// The code lives in this hook's memory for that one press: never rendered, stored, logged or put in a URL.
// The rules of the old dialog (`components/ConfirmDialog.tsx`, kept as a component) carry over:
//   - no call without a press;
//   - `unreachable` on the ask shows the line and Retry, which asks again with a new command ID;
//   - `unreachable` on the spend shows the line and Retry, which spends the same code with the same ID;
//   - a refusal shows `refusalLine(err)` on the card and drops the code; `subject_moved` reads `requests`
//     once; a replay reads `requests` once and claims no outcome; a down kind leaves the line to the 3c banner;
//   - while a press is in flight a second press starts nothing.
// The hook lives in the page, keyed by request ID: a request frame that closes the request while its spend
// is in flight does not drop the spend's answer, and an answer that arrives after the page closed still
// applies to the shared requests state.
//
// This is a freshness and binding check, not a check that a person is present (Q2, C1): a script that holds
// the device's bridge can make both calls. The page adds no user-presence check.
import type { ConfirmCodeArgs, RequestActed, RequestSummary } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { newCommandId } from "../components/command-id";
import { refusalLine } from "../components/ConfirmDialog";
import { sha256Hex } from "../components/sha256";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { useRequests } from "../state/requests";
import { shortSha } from "./format";

export type ActKind = "answer" | "ack" | "clear";

/** What a card shows of its press: the line and, after `unreachable`, a Retry. */
export interface ActLine {
  phase: "asking" | "ask_unreachable" | "spending" | "spend_unreachable" | "ended";
  message: string | null;
}

/** What a landed act did, for the card's mint row and the Answered today row. */
export interface Done {
  /** The answer text, "Merge approved", "Not merged: <outcome>" or "Resumed at <stage>". */
  what: string;
  /** A second line: "Merged at <short SHA>" after an approved merge. */
  detail: string | null;
  /** The device name when this device acted. */
  device: string;
}

interface Pending {
  kind: ActKind;
  request: RequestSummary;
  text: string;
  /** In memory only. `null` until the ask answers. */
  code: string | null;
  /** The command ID of the spend: kept for a retry after `unreachable`, and only then. */
  spendId: string | null;
}

export interface ConfirmedActs {
  /** The line of a request's press in flight or ended, by request ID. */
  lines: Record<string, ActLine>;
  /** What each act this page made did, by request ID. */
  done: Record<string, Done>;
  /** A request that a replay found closed: "Done before: <status> at <time>". */
  notices: Record<string, string>;
  /** Press: ask a code, then spend it at once. A press while another is in flight for this request does nothing. */
  press(request: RequestSummary, kind: ActKind, text?: string): void;
  /** Retry after `unreachable`: the ask again (new command ID), or the spend again (same code, same ID). */
  retry(id: string): void;
  /** A press is in flight for this request: the card's buttons are disabled. */
  inFlight(id: string): boolean;
}

export function useConfirmedAct(): ConfirmedActs {
  const { call, server } = useConnection();
  const { apply } = useRequests();
  const [lines, setLines] = useState<Record<string, ActLine>>({});
  const [done, setDone] = useState<Record<string, Done>>({});
  const [notices, setNotices] = useState<Record<string, string>>({});
  const pending = useRef(new Map<string, Pending>());
  const busy = useRef(new Set<string>());
  const live = useRef(true);
  const deviceName = server.device.name;
  const deviceId = server.device.id;

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const setLine = useCallback((id: string, line: ActLine | null) => {
    if (!live.current) return;
    setLines((all) => {
      if (line === null) {
        if (!(id in all)) return all;
        const rest = { ...all };
        delete rest[id];
        return rest;
      }
      return { ...all, [id]: line };
    });
  }, []);

  /** A read of `requests`, as the server has them: for `subject_moved` and a replay. It applies to the shared state. */
  const readRequests = useCallback(async () => {
    try {
      const list = await call((t) => t.requests({ all: true }));
      apply({ type: "read", list });
      return list;
    } catch {
      // A disconnect shows the 3c banner; the page's own Refresh reads again.
      return null;
    }
  }, [call, apply]);

  const landed = useCallback(
    (p: Pending, acted: RequestActed) => {
      apply({ type: "acted", acted });
      const who = acted.device === deviceId ? deviceName : acted.device;
      let outcome: Done;
      if (p.kind === "ack") {
        outcome =
          acted.merged === true
            ? { what: "Merge approved", detail: `Merged at ${shortSha(acted.head_sha ?? p.request.head_sha ?? "")}`, device: who }
            : { what: `Not merged: ${acted.outcome ?? "the merge did not land"}`, detail: null, device: who };
      } else if (p.kind === "clear") {
        outcome = { what: `Resumed at ${acted.to_stage ?? p.request.stage}`, detail: null, device: who };
      } else {
        outcome = { what: p.text, detail: null, device: who };
      }
      if (live.current) setDone((d) => ({ ...d, [p.request.id]: outcome }));
    },
    [apply, deviceId, deviceName],
  );

  /** A replay acts on nothing and says nothing of its outcome: read the request as the server has it. */
  const replayed = useCallback(
    async (p: Pending) => {
      const list = await readRequests();
      const row = list?.requests.find((r) => r.id === p.request.id);
      if (row && row.status !== "open" && live.current) {
        setNotices((n) => ({ ...n, [p.request.id]: `Done before: ${row.status} at ${row.answered_at ?? ""}` }));
      }
    },
    [readRequests],
  );

  const spend = useCallback(
    async (id: string) => {
      const p = pending.current.get(id);
      if (p === undefined || p.code === null) return;
      const code = p.code;
      busy.current.add(id);
      setLine(id, { phase: "spending", message: null });
      const commandId = (p.spendId ??= newCommandId());
      const { request } = p;
      try {
        const acted = await call((t) =>
          p.kind === "ack"
            ? t.ack({ request: request.id, rev: request.revision, code, command_id: commandId })
            : p.kind === "clear"
              ? t.clear({ request: request.id, rev: request.revision, code, command_id: commandId })
              : t.answer({ request: request.id, rev: request.revision, text: p.text, code, command_id: commandId }),
        );
        // The act landed: its answer applies even if the card or the page is gone meanwhile.
        pending.current.delete(id);
        busy.current.delete(id);
        setLine(id, null);
        if (acted.replayed) void replayed(p);
        else landed(p, acted);
      } catch (e) {
        busy.current.delete(id);
        const err = asTransportError(e);
        if (err.kind === "unreachable") {
          setLine(id, { phase: "spend_unreachable", message: err.message });
          return;
        }
        pending.current.delete(id);
        if (isDownKind(err.kind)) {
          setLine(id, null);
          return;
        }
        if (err.refusal === "subject_moved") void readRequests();
        setLine(id, { phase: "ended", message: refusalLine(err) });
      }
    },
    [call, landed, replayed, readRequests, setLine],
  );

  const ask = useCallback(
    async (id: string) => {
      const p = pending.current.get(id);
      if (p === undefined) return;
      busy.current.add(id);
      p.code = null;
      p.spendId = null;
      setLine(id, { phase: "asking", message: null });
      try {
        const { request } = p;
        const args: ConfirmCodeArgs = {
          subject:
            p.kind === "answer"
              ? { kind: "request", request: request.id, revision: request.revision, verb: "answer", text_sha256: await sha256Hex(p.text) }
              : { kind: "request", request: request.id, revision: request.revision, verb: p.kind },
          // A fresh command ID for each ask, so a lost answer is never replayed (a replay answers `code: null`).
          command_id: newCommandId(),
        };
        const issued = await call((t) => t.confirmCode(args));
        if (issued.code === null) {
          busy.current.delete(id);
          pending.current.delete(id);
          setLine(id, { phase: "ended", message: "The server gave no confirm code. Confirm again." });
          return;
        }
        p.code = issued.code;
        await spend(id);
      } catch (e) {
        busy.current.delete(id);
        const err = asTransportError(e);
        if (err.kind === "unreachable") {
          setLine(id, { phase: "ask_unreachable", message: err.message });
        } else if (isDownKind(err.kind)) {
          // The 3c banner shows it; there is nothing to spend.
          pending.current.delete(id);
          setLine(id, null);
        } else {
          pending.current.delete(id);
          setLine(id, { phase: "ended", message: err.message });
        }
      }
    },
    [call, spend, setLine],
  );

  const press = useCallback(
    (request: RequestSummary, kind: ActKind, text = "") => {
      if (busy.current.has(request.id)) return;
      pending.current.set(request.id, { kind, request, text, code: null, spendId: null });
      void ask(request.id);
    },
    [ask],
  );

  const retry = useCallback(
    (id: string) => {
      if (busy.current.has(id)) return;
      const p = pending.current.get(id);
      if (p === undefined) return;
      if (p.code !== null) void spend(id);
      else void ask(id);
    },
    [ask, spend],
  );

  const inFlight = useCallback(
    (id: string) => {
      const phase = lines[id]?.phase;
      return phase === "asking" || phase === "spending";
    },
    [lines],
  );

  return { lines, done, notices, press, retry, inFlight };
}
