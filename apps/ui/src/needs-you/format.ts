// Small pure helpers of the Needs you cards (r1.s5.w4): the age of a request, its PR label, the question of
// a `stop` or `gaps` request from its body, and a field of a merge request's body. A body is data: it renders
// as text only.
import type { RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import { clock } from "../components/format";
import { parseTime } from "../components/chat-format";

/** `just now`, `12m`, `3h 05m`, `2d`. */
export function age(openedAt: string, now: number = Date.now()): string {
  const at = parseTime(openedAt);
  if (at === null) return "";
  const minutes = Math.max(0, Math.floor((now - at) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
  return `${Math.floor(hours / 24)}d`;
}

/** `14:36 · 5m`: the time a request opened and its age. */
export function timeAge(openedAt: string, now: number = Date.now()): string {
  const at = parseTime(openedAt);
  return at === null ? openedAt : `${clock(at)} · ${age(openedAt, now)}`;
}

/** The first line of a reason. */
export function firstLine(text: string): string {
  return text.split("\n")[0]?.trim() ?? "";
}

/** `PR #9` when the PR text ends in `/pull/<n>`, else the text as the server sent it. */
export function prLabel(pr: string): string {
  const match = /\/pull\/(\d+)$/.exec(pr);
  return match === null ? pr : `PR #${match[1]}`;
}

export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}

export interface Gap {
  section: string;
  question: string;
  severity: string;
}

/** The question of a `stop` or `gaps` request, from its body: the text is data and renders as text only. */
export function questionOf(body: unknown): { stop: string | null; gaps: Gap[] } {
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

/** A string field of a request: the body's, else the detail's own, else the summary's. */
export function fieldOf(name: "pr" | "base" | "head_sha" | "head_branch", request: RequestSummary, detail: RequestDetail | undefined): string | null {
  const body = detail?.body;
  if (typeof body === "object" && body !== null) {
    const value = (body as Record<string, unknown>)[name];
    if (typeof value === "string" && value !== "") return value;
  }
  if (name === "head_branch") return null;
  const own = (detail ?? request)[name];
  if (typeof own === "string" && own !== "") return own;
  const base = request[name];
  return typeof base === "string" && base !== "" ? base : null;
}

/** The kind of card a request is. */
export type CardKind = "merge" | "run_question" | "parked" | "other";

/**
 * A `merge_ack` is the merge card. A `stop` or `gaps` request is a run question. Any other unit request that
 * takes `clear` is a parked item. Anything else (a run-level request that takes `answer` only) is a run
 * question too, so its text answer stays (G8).
 */
export function cardKind(request: RequestSummary): CardKind {
  if (request.kind === "merge_ack") return "merge";
  if (request.kind === "stop" || request.kind === "gaps") return "run_question";
  if (request.verbs.includes("clear")) return "parked";
  if (request.verbs.includes("answer")) return "run_question";
  return "other";
}
