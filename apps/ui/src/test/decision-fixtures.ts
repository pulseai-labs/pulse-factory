// Typed decision blocks and card environments for the Needs you tests (r2.s11.w5). The blocks hold the real
// summary sentences of the controller (`summary_of`) and stored roles and stages, not the frames' examples.
import type { Decision, DecisionOption, RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import { vi } from "vitest";
import type { CardEnv } from "../needs-you/cards";

/** A block with every field of `Decision`: a seat's stop file that takes text, one run, one unit. */
export function decisionBlock(patch: Partial<Decision> = {}): Decision {
  return {
    v: 1,
    cause: "stop_file",
    summary: "Unit u1 stopped at stage implement and wrote a stop file.",
    options: [],
    refs: { project: "P-1", run: "R-1", unit: "u1", stage: "implement" },
    attempt: 1,
    asked_by: { kind: "seat", role: "implementer", unit: "u1" },
    takes_text: true,
    ...patch,
  };
}

/** An option that sends an answer. */
export function answerOption(id: string, label: string, patch: Partial<DecisionOption> = {}): DecisionOption {
  return { id, label, detail: `Sends the answer for ${id}.`, effect: { verb: "answer", text: label }, by: "seat", ...patch };
}

/** An option that resumes the unit, at `to` when it names a stage. */
export function resumeOption(id: string, label: string, to?: string, patch: Partial<DecisionOption> = {}): DecisionOption {
  return {
    id,
    label,
    detail: `Resumes the unit (${id}).`,
    effect: to === undefined ? { verb: "resume" } : { verb: "resume", to },
    by: "controller",
    ...patch,
  };
}

/** The approval of a merge. */
export function approveOption(): DecisionOption {
  return { id: "approve", label: "Approve merge", detail: "Merges the PR.", effect: { verb: "ack" }, by: "controller" };
}

/** The `RequestDetail` a `show` answers for a request and its block. */
export function detailOf(request: RequestSummary, decision: Decision | undefined, body: unknown = {}, answer: unknown = null): RequestDetail {
  return {
    schema_version: 1,
    ...request,
    body,
    answer,
    answer_generation: null,
    ...(decision ? { decision } : {}),
  };
}

/** A card environment with no call behind it: `onAct` records the acts. */
export function cardEnv(patch: Partial<CardEnv> = {}): CardEnv {
  return {
    detail: undefined,
    bodyError: null,
    spine: "s1",
    line: undefined,
    done: undefined,
    notice: undefined,
    inFlight: false,
    compact: false,
    projects: [{ id: "P-1", name: "pulse-factory", root: "/home/dev/pulse-factory", created_at: "2026-11-07T10:00:00Z", removed_at: null, kind: "git", remote_key: null, kind_error: null }],
    serverName: "draco-desk",
    onAct: vi.fn(),
    onRetry: vi.fn(),
    onRefresh: vi.fn(),
    ...patch,
  };
}
