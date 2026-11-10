// The confirm dialog of Needs you (r1.s4.w3; 3f, functional). One act is two clicks: the first asked for
// a one-use confirm code, and this dialog shows what the code binds, and whom the act takes the claim
// from; the second click spends it. Nothing here moves the claim: the claim moves at the final click
// (C2), and Cancel, Escape or leaving the page drops the code and sends nothing.
//
// The code is in memory only: it is never rendered, stored, logged or put in a URL. This is a
// freshness and binding check, not a check that a person is present (C1, Q2): a script that holds the
// device's bridge can do both clicks. The dialog adds no user-presence check.
import type { ConfirmCodeArgs, ConfirmIssued, RequestActed, RequestSummary } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { TransportError } from "../transport/types";
import { newCommandId } from "./command-id";
import { sha256Hex } from "./sha256";

export type DialogKind = "merge" | "answer";

type Phase =
  | { name: "asking" }
  /** The first click got no answer: Retry asks again with a new command ID (W7). */
  | { name: "ask_unreachable"; message: string }
  | { name: "ready"; issued: ConfirmIssued; code: string }
  | { name: "spending"; issued: ConfirmIssued; code: string }
  /** The second click got no answer: Retry sends the same call with the same code and command ID. */
  | { name: "spend_unreachable"; issued: ConfirmIssued; code: string; message: string }
  /** A refusal ends the dialog: one line, and the code is dropped. */
  | { name: "ended"; message: string };

/** The line of a refusal: by its word, not by the server's text, except where the text names the facts. */
export function refusalLine(err: TransportError): string {
  switch (err.refusal) {
    case "code_expired":
      return "The confirm code expired. Confirm again.";
    case "code_used":
    case "code_voided":
    case "code_unknown":
    case "code_foreign":
    case "code_subject":
      return `This confirm code cannot be used (${err.refusal}). Confirm again.`;
    case "subject_moved":
      return "The request changed since the code was issued. Read it again and confirm.";
    case "claim_moved":
      // The server's text names the new holder.
      return `${err.message} Confirm again.`;
    default:
      // `device_revoked`, and an engine refusal after the admission (checks red, a halted run) with no word.
      return err.message;
  }
}

export function ConfirmDialog(props: {
  kind: DialogKind;
  request: RequestSummary;
  /** The answer text, for `answer`. The page locks it while this dialog is open. */
  text: string;
  /** The act went through (not a replay). */
  onActed(acted: RequestActed): void;
  /** The command ID was admitted before: the page reads the request again and claims no outcome. */
  onReplayed(acted: RequestActed): void;
  /** The request changed under the code: the page reads `requests` once. */
  onSubjectMoved(): void;
  onClose(): void;
}) {
  const { kind, request, text, onActed, onReplayed, onSubjectMoved, onClose } = props;
  const { call, server } = useConnection();
  const [phase, setPhase] = useState<Phase>({ name: "asking" });
  const live = useRef(true);
  // The command ID of the second click: kept for a retry after `unreachable`, and only then.
  const spendId = useRef<string | null>(null);
  const label = kind === "merge" ? "Confirm merge" : "Confirm answer";

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  // The first click: ask for a code. A fresh command ID for each ask, so a lost answer is never replayed
  // (a replay answers `code: null`, W7).
  const ask = useCallback(async () => {
    setPhase({ name: "asking" });
    spendId.current = null;
    try {
      const args: ConfirmCodeArgs = {
        subject:
          kind === "merge"
            ? { kind: "request", request: request.id, revision: request.revision, verb: "ack" }
            : {
                kind: "request",
                request: request.id,
                revision: request.revision,
                verb: "answer",
                text_sha256: await sha256Hex(text),
              },
        command_id: newCommandId(),
      };
      const issued = await call((t) => t.confirmCode(args));
      if (!live.current) return;
      if (issued.code === null) {
        setPhase({ name: "ended", message: "The server gave no confirm code. Confirm again." });
        return;
      }
      setPhase({ name: "ready", issued, code: issued.code });
    } catch (e) {
      if (!live.current) return;
      const err = asTransportError(e);
      if (err.kind === "unreachable") {
        setPhase({ name: "ask_unreachable", message: err.message });
      } else if (isDownKind(err.kind)) {
        // The 3c banner shows it; the dialog has nothing to spend.
        onClose();
      } else {
        setPhase({ name: "ended", message: err.message });
      }
    }
  }, [call, kind, request.id, request.revision, text, onClose]);

  const asked = useRef(false);
  useEffect(() => {
    if (asked.current) return;
    asked.current = true;
    void ask();
  }, [ask]);

  // The second click.
  const spend = useCallback(
    async (issued: ConfirmIssued, code: string) => {
      setPhase({ name: "spending", issued, code });
      const commandId = (spendId.current ??= newCommandId());
      try {
        const acted = await call((t) =>
          kind === "merge"
            ? t.ack({ request: request.id, rev: request.revision, code, command_id: commandId })
            : t.answer({ request: request.id, rev: request.revision, text, code, command_id: commandId }),
        );
        // The act landed: its answer applies even if this dialog was unmounted meanwhile (the operator left
        // the page, or the page closed it). The callbacks apply to the shared state, not to this dialog.
        spendId.current = null;
        if (acted.replayed) onReplayed(acted);
        else onActed(acted);
        onClose();
      } catch (e) {
        if (!live.current) return;
        const err = asTransportError(e);
        if (err.kind === "unreachable") {
          setPhase({ name: "spend_unreachable", issued, code, message: err.message });
          return;
        }
        spendId.current = null;
        if (isDownKind(err.kind)) {
          onClose();
          return;
        }
        if (err.refusal === "subject_moved") onSubjectMoved();
        setPhase({ name: "ended", message: refusalLine(err) });
      }
    },
    [call, kind, request.id, request.revision, text, onActed, onReplayed, onSubjectMoved, onClose],
  );

  // Escape closes the dialog, except while the spend is in flight: its answer must reach the page.
  const spending = phase.name === "spending";
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !spending) onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, spending]);

  const issued = "issued" in phase ? phase.issued : null;
  const holder = issued?.holder ?? null;
  const subject = issued?.subject;
  const claimLine = !issued
    ? null
    : holder === null
      ? "No one holds the claim"
      : holder.device !== null && holder.device === server.device.id
        ? "This device holds the claim"
        : `Takes the claim from ${holder.holder}${holder.revoked ? " (revoked)" : ""}`;

  return (
    <div className="confirm" role="dialog" aria-modal="true" aria-label={label}>
      <h4>{label}</h4>
      {phase.name === "asking" ? <p>Asking for a confirm code.</p> : null}
      {phase.name === "ask_unreachable" ? (
        <>
          <p role="alert">{phase.message}</p>
          <div className="card-actions">
            <button className="btn" type="button" onClick={() => void ask()}>
              Retry
            </button>
            <button className="btn" type="button" onClick={onClose}>
              Cancel
            </button>
          </div>
        </>
      ) : null}
      {issued && subject ? (
        <>
          {kind === "merge" ? (
            <dl className="facts">
              <dt>Request</dt>
              <dd>{subject.request}</dd>
              <dt>PR</dt>
              <dd className="mono">{subject.pr ?? ""}</dd>
              <dt>Base</dt>
              <dd className="mono">{subject.base ?? ""}</dd>
              <dt>Head branch</dt>
              <dd className="mono">{subject.head_branch ?? ""}</dd>
              <dt>Head SHA</dt>
              <dd className="mono">{subject.head_sha ?? ""}</dd>
            </dl>
          ) : (
            <dl className="facts">
              <dt>Request</dt>
              <dd>{subject.request}</dd>
              <dt>Your answer</dt>
              <dd>
                <pre className="mono">{text}</pre>
              </dd>
            </dl>
          )}
          <p>{claimLine}</p>
          <dl className="facts">
            <dt>Expires at</dt>
            <dd className="mono">{issued.expires_at ?? ""}</dd>
          </dl>
        </>
      ) : null}
      {phase.name === "ready" || phase.name === "spending" ? (
        <div className="card-actions">
          <button
            className="btn btn-primary"
            type="button"
            disabled={phase.name === "spending"}
            onClick={() => void spend(phase.issued, phase.code)}
          >
            {kind === "merge" ? "Merge" : "Send"}
          </button>
          <button className="btn" type="button" disabled={phase.name === "spending"} onClick={onClose}>
            Cancel
          </button>
        </div>
      ) : null}
      {phase.name === "spend_unreachable" ? (
        <>
          <p role="alert">{phase.message}</p>
          <div className="card-actions">
            <button className="btn btn-primary" type="button" onClick={() => void spend(phase.issued, phase.code)}>
              Retry
            </button>
            <button className="btn" type="button" onClick={onClose}>
              Cancel
            </button>
          </div>
        </>
      ) : null}
      {phase.name === "ended" ? (
        <>
          <p role="alert">{phase.message}</p>
          <div className="card-actions">
            <button className="btn" type="button" onClick={onClose}>
              Close
            </button>
          </div>
        </>
      ) : null}
    </div>
  );
}
