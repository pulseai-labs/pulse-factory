// Small text helpers of the chat screens.
import type { ChatSessionRecord, ProjectRecord } from "@pulse-factory/api-client";
import { clock } from "./format";

/** The branch cell: the branch the server read live, `detached`, or `folder` for a plain folder. */
export function branchText(session: ChatSessionRecord, projects: ProjectRecord[] | null): string {
  if (session.detached) return "detached";
  if (session.branch !== null) return session.branch;
  const kind = projects?.find((p) => p.id === session.project_id)?.kind;
  return kind === "git" ? "unknown" : "folder";
}

/** The last segment of a path (the worktree's name). */
export function lastSegment(path: string): string {
  const parts = path.split("/").filter((p) => p !== "");
  return parts.at(-1) ?? path;
}

/** A server's UTC text as milliseconds, or `null` when it does not parse. */
export function parseTime(text: string | null): number | null {
  if (text === null) return null;
  const ms = Date.parse(text);
  return Number.isNaN(ms) ? null : ms;
}

/** An item's or a request's time as `HH:MM`. */
export function timeText(text: string): string {
  const ms = parseTime(text);
  return ms === null ? text : clock(ms);
}

/** A duration in milliseconds as `45s`, `1m 12s` or `1h 05m`. */
export function durationText(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  if (minutes < 60) return `${minutes}m ${String(total % 60).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
}
