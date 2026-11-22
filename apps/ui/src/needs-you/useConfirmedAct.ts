// The one-press act (r1.s5.w4, Approach 1; ruling G1; r2.s11.w5, Approach 6). One hook for Approve merge, Send,
// Resume and one button for each option. A press runs two calls in order: `confirmCode` with a fresh command ID
// and the act's subject, then the act (`ack`, `answer` or `clear`) with that code and a second fresh command ID.
// An option press binds the option's ID and sends the option and nothing of its own (an empty `text`, no `to`); a
// note rides a `resume` option as one typed `answer` that binds the note's SHA-256 and the option's stage. The confirm panel is gone, so the
// `client-api-auth` control (a fresh, one-use confirm code bound to this device and this act) stays as a
// step the app takes behind the press.
//
// The code lives in this hook's memory for that one press: never rendered, stored, logged or put in a URL.
// The rules of the old dialog (`components/ConfirmDialog.tsx`, kept as a component) carry over:
//   - no call without a press;
//   - `unreachable` on the ask shows the line and Retry, which asks again with a new command ID;
//   - `unreachable` on the spend shows the line and Retry, which spends the same code with the same ID;
//   - a refusal shows `refusalLine(err)` on the card and drops the code; `subject_moved` reads `requests`
//     once and drops the press (an option of an older revision is never sent again); any other refusal keeps the
//     press for Try again, which asks a new code; a replay reads `requests` once and claims no outcome; a down
//     kind leaves the line to the 3c banner;
//   - while a press is in flight a second press starts nothing.
// The hook lives in the page, keyed by request ID: a request frame that closes the request while its spend
// is in flight does not drop the spend's answer, and an answer that arrives after the page closed still
// applies to the shared requests state.
//
// This is a freshness and binding check, not a check that a person is present (Q2, C1): a script that holds
// the device's bridge can make both calls. The page adds no user-presence check.
import type { ConfirmCodeArgs, DecisionOption, RequestActed, RequestSummary } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { newCommandId } from "../components/command-id";
import { refusalLine } from "../components/ConfirmDialog";
import { sha256Hex } from "../components/sha256";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { useRequests } from "../state/requests";
import { shortSha } from "./format";

export type ActKind = "answer" | "ack" | "clear";

/**
 * One press. The first three are today's acts (a card with no decision block). The others come from a block
 * card: `send` a typed answer to a seat, `option` one button of the block, `note` a `resume` option with a note
 * (one typed `answer`), `resume` a plain `clear` of a stop that takes no text, and `approve` the merge.
 * `unit` is the words of the answered row.
 */
export type Act =
  | { kind: "answer"; text: string }
  | { kind: "clear" }
  | { kind: "ack" }
  | { kind: "send"; text: string; unit: string }
  | { kind: "option"; option: DecisionOption; unit: string }
  | { kind: "note"; option: DecisionOption; text: string; unit: string }
  | { kind: "resume"; unit: string }
  | { kind: "approve"; option?: string };

/** The verb an act calls. */
export function verbOf(act: Act): ActKind {
  switch (act.kind) {
    case "answer":
    case "send":
    case "note":
      return "answer";
    case "clear":
    case "resume":
      return "clear";
    case "ack":
    case "approve":
      return "ack";
    case "option":
      return act.option.effect.verb === "resume" ? "clear" : act.option.effect.verb;
  }
}

/** The control a press came from: an option's ID, `send`, `resume` or `approve`. The card shows it working. */
export function controlOf(act: Act): string {
  switch (act.kind) {
    case "option":
    case "note":
      return act.option.id;
    case "answer":
    case "send":
      return "send";
    case "clear":
    case "resume":
      return "resume";
    case "ack":
    case "approve":
      return "approve";
  }
}

/** What a card shows of its press: the line and, after `unreachable`, a Retry. */
export interface ActLine {
  phase: "asking" | "ask_unreachable" | "spending" | "spend_unreachable" | "ended";
  message: string | null;
  /** The control that was pressed (`controlOf`). */
  control: string;
  /** A refusal that leaves the press to try again: the same act, a new code. */
  again: boolean;
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
  act: Act;
  request: RequestSummary;
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
  press(request: RequestSummary, act: Act): void;
  /** Retry after `unreachable`: the ask again (new command ID), or the spend again (same code, same ID). Try again after a refusal: the ask again. */
  retry(id: string): void;
  /** A press is in flight for this request: the card's buttons are disabled. */
  inFlight(id: string): boolean;
}

/** What a landed act did, in the words of the mint row and of the answered row (Approach 7). */
function wordsOf(p: Pending, acted: RequestActed, who: string): Done {
  const { act, request } = p;
  if (act.kind === "ack" || act.kind === "approve") {
    return acted.merged === true
      ? { what: "Merge approved", detail: `Merged at ${shortSha(acted.head_sha ?? request.head_sha ?? "")}`, device: who }
      : { what: `Not merged: ${acted.outcome ?? "the merge did not land"}`, detail: null, device: who };
  }
  const stage = (to: string | undefined) => acted.to_stage ?? to ?? request.stage;
  switch (act.kind) {
    case "clear":
      return { what: `Resumed at ${stage(undefined)}`, detail: null, device: who };
    case "answer":
      return { what: act.text, detail: null, device: who };
    case "send":
      return { what: `Typed ${act.text}`, detail: null, device: who };
    case "resume":
      return { what: `Resumed ${act.unit} at ${stage(undefined)}`, detail: null, device: who };
    case "note": {
      const to = act.option.effect.verb === "resume" ? act.option.effect.to : undefined;
      return { what: `Resumed ${act.unit} at ${stage(to)} \u00b7 note: ${act.text}`, detail: null, device: who };
    }
    case "option": {
      const effect = act.option.effect;
      if (effect.verb === "answer") return { what: `Pressed ${act.option.label}`, detail: null, device: who };
      return {
        what: `Resumed ${act.unit} at ${stage(effect.verb === "resume" ? effect.to : undefined)}`,
        detail: null,
        device: who,
      };
    }
  }
}

export function useConfirmedAct(): ConfirmedActs {
  const { call, server } = useConnection();
  const { apply, state } = useRequests();
  const [lines, setLines] = useState<Record<string, ActLine>>({});
  const [done, setDone] = useState<Record<string, Done>>({});
  const [notices, setNotices] = useState<Record<string, string>>({});
  const pending = useRef(new Map<string, Pending>());
  const busy = useRef(new Set<string>());
  const live = useRef(true);
  const deviceName = server.device.name;
  const deviceId = server.device.id;
  // The requests as the state holds them now, for the check that a retry never sends an older revision.
  const held = useRef(state.byId);
  held.current = state.byId;

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
      const outcome = wordsOf(p, acted, who);
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
      const control = controlOf(p.act);
      busy.current.add(id);
      setLine(id, { phase: "spending", message: null, control, again: false });
      const commandId = (p.spendId ??= newCommandId());
      const { request, act } = p;
      const base = { request: request.id, rev: request.revision, code, command_id: commandId };
      try {
        const acted = await call((t) => {
          switch (act.kind) {
            case "ack":
              return t.ack(base);
            case "approve":
              return t.ack(act.option === undefined ? base : { ...base, option: act.option });
            case "clear":
            case "resume":
              return t.clear(base);
            case "answer":
            case "send":
              return t.answer({ ...base, text: act.text });
            case "note": {
              const to = act.option.effect.verb === "resume" ? act.option.effect.to : undefined;
              return t.answer(to === undefined ? { ...base, text: act.text } : { ...base, text: act.text, to });
            }
            case "option":
              // An option press carries the option and nothing of its own: an empty text, no stage.
              return act.option.effect.verb === "answer"
                ? t.answer({ ...base, text: "", option: act.option.id })
                : act.option.effect.verb === "resume"
                  ? t.clear({ ...base, option: act.option.id })
                  : t.ack({ ...base, option: act.option.id });
          }
        });
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
          setLine(id, { phase: "spend_unreachable", message: err.message, control, again: false });
          return;
        }
        // The code is spent or refused: drop it. The press stays for Try again unless the request moved.
        p.code = null;
        p.spendId = null;
        if (isDownKind(err.kind)) {
          pending.current.delete(id);
          setLine(id, null);
          return;
        }
        const moved = err.refusal === "subject_moved";
        if (moved) {
          pending.current.delete(id);
          void readRequests();
        }
        setLine(id, { phase: "ended", message: refusalLine(err), control, again: !moved });
      }
    },
    [call, landed, replayed, readRequests, setLine],
  );

  const ask = useCallback(
    async (id: string) => {
      const p = pending.current.get(id);
      if (p === undefined) return;
      const control = controlOf(p.act);
      busy.current.add(id);
      p.code = null;
      p.spendId = null;
      setLine(id, { phase: "asking", message: null, control, again: false });
      try {
        const { request, act } = p;
        const at = { request: request.id, revision: request.revision };
        let subject: ConfirmCodeArgs["subject"];
        switch (act.kind) {
          case "answer":
          case "send":
            subject = { kind: "request", ...at, verb: "answer", text_sha256: await sha256Hex(act.text) };
            break;
          case "note": {
            const to = act.option.effect.verb === "resume" ? act.option.effect.to : undefined;
            const text_sha256 = await sha256Hex(act.text);
            subject = to === undefined ? { kind: "request", ...at, verb: "answer", text_sha256 } : { kind: "request", ...at, verb: "answer", text_sha256, to };
            break;
          }
          case "option":
            subject = { kind: "request", ...at, verb: verbOf(act), option: act.option.id };
            break;
          case "approve":
            subject = act.option === undefined ? { kind: "request", ...at, verb: "ack" } : { kind: "request", ...at, verb: "ack", option: act.option };
            break;
          default:
            subject = { kind: "request", ...at, verb: verbOf(act) };
        }
        // A fresh command ID for each ask, so a lost answer is never replayed (a replay answers `code: null`).
        const args: ConfirmCodeArgs = { subject, command_id: newCommandId() };
        const issued = await call((t) => t.confirmCode(args));
        if (issued.code === null) {
          busy.current.delete(id);
          setLine(id, { phase: "ended", message: "The server gave no confirm code. Confirm again.", control, again: true });
          return;
        }
        p.code = issued.code;
        await spend(id);
      } catch (e) {
        busy.current.delete(id);
        const err = asTransportError(e);
        if (err.kind === "unreachable") {
          setLine(id, { phase: "ask_unreachable", message: err.message, control, again: false });
        } else if (isDownKind(err.kind)) {
          // The 3c banner shows it; there is nothing to spend.
          pending.current.delete(id);
          setLine(id, null);
        } else {
          setLine(id, { phase: "ended", message: err.message, control, again: true });
        }
      }
    },
    [call, spend, setLine],
  );

  const press = useCallback(
    (request: RequestSummary, act: Act) => {
      if (busy.current.has(request.id)) return;
      pending.current.set(request.id, { act, request, code: null, spendId: null });
      void ask(request.id);
    },
    [ask],
  );

  const retry = useCallback(
    (id: string) => {
      if (busy.current.has(id)) return;
      const p = pending.current.get(id);
      if (p === undefined) return;
      // An option of an older revision is never sent again (G8): a request that moved drops the press.
      const now = held.current[id];
      if (now !== undefined && now.revision !== p.request.revision) {
        pending.current.delete(id);
        setLine(id, null);
        return;
      }
      if (p.code !== null) void spend(id);
      else void ask(id);
    },
    [ask, spend, setLine],
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
