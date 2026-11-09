// Helpers of the chat tests: a seeded fake, frames pushed inside `act`, and a booted chat.
import type { ChatFrame, ChatSessionRecord, ProjectRecord } from "@pulse-factory/api-client";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, vi } from "vitest";
import { App } from "../App";
import { FakeTransport, chatSession, claudeRuntime, frames, orchestrator, project } from "./fake-transport";

export type User = ReturnType<typeof userEvent.setup>;

/** A fake with the Claude Code runtime, the Orchestrator, two projects and one session of alpha. */
export function seeded(
  opts: { sessions?: ChatSessionRecord[]; projects?: ProjectRecord[] } = {},
): FakeTransport {
  const fake = new FakeTransport({ projects: opts.projects ?? [project(1, "alpha"), project(2, "beta")] });
  fake.runtimes = [claudeRuntime()];
  fake.agents = [orchestrator()];
  fake.sessions = opts.sessions ?? [alpha()];
  return fake;
}

export function alpha(patch: Partial<ChatSessionRecord> = {}): ChatSessionRecord {
  return chatSession("S-1", {
    project_id: "P-1",
    project_name: "alpha",
    worktree_dir: "/home/dev/alpha",
    ...patch,
  });
}

/** Deliver frames to the newest open stream, inside `act`. */
export async function push(fake: FakeTransport, ...list: ChatFrame[]): Promise<void> {
  await act(async () => {
    for (const frame of list) fake.frame(frame);
  });
}

/** Render the app on the Projects page and wait for the overview stream. */
export async function boot(fake: FakeTransport): Promise<User> {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.current()).toBeDefined());
  return user;
}

/** Boot, show the sessions in the sidebar, and open one chat from its row. */
export async function bootChat(fake: FakeTransport, id = "S-1", items: ChatFrame[] = []): Promise<User> {
  const user = await boot(fake);
  await push(fake, frames.hello(fake.sessions));
  const record = fake.sessions.find((s) => s.id === id);
  if (!record) throw new Error(`no session ${id} on the fake`);
  const sidebar = within(screen.getByRole("region", { name: "Sidebar" }));
  await user.click(await sidebar.findByRole("button", { name: new RegExp(`session in ${record.project_name}`) }));
  await waitFor(() => expect(fake.current()?.options.session).toBe(id));
  await push(fake, frames.hello(fake.sessions, id), ...items);
  return user;
}

export const main = () => within(screen.getByRole("main"));

/** Advance fake timers inside `act`. */
export async function advance(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

/** Like `bootChat`, for tests under fake timers: Testing Library's async helpers cannot run there. */
export async function bootChatFake(fake: FakeTransport, id = "S-1"): Promise<void> {
  render(<App transport={fake} />);
  await advance(0);
  await push(fake, frames.hello(fake.sessions));
  const record = fake.sessions.find((s) => s.id === id);
  if (!record) throw new Error(`no session ${id} on the fake`);
  const sidebar = within(screen.getByRole("region", { name: "Sidebar" }));
  fireEvent.click(sidebar.getByRole("button", { name: new RegExp(`session in ${record.project_name}`) }));
  await advance(0);
  await push(fake, frames.hello(fake.sessions, id));
  await advance(0);
}
