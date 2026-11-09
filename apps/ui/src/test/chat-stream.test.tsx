// AC-5: one stream at a time and when it opens (A8, A10), what each `closed` kind does, the sidebar rows
// that `hello` and `session` frames drive, the no-polling rule on a chat page, and the one storage key.
import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StreamEvent } from "../transport/types";
import { advance, alpha, boot, bootChat, bootChatFake, main, push, seeded } from "./chat-helpers";
import { ADDRESS, chatSession, frames, item } from "./fake-transport";

const sidebar = () => within(screen.getByRole("region", { name: "Sidebar" }));
const rail = () => within(screen.getByRole("navigation"));
const closed = (kind: "unreachable" | "unauthorized" | "key_changed" | "rate_limited" | "server" | "bad_request" | "forbidden" | "refused", message: string): StreamEvent => ({
  event: "closed",
  error: { kind, message },
});

async function emit(fake: ReturnType<typeof seeded>, event: StreamEvent) {
  await act(async () => {
    fake.emit(event);
  });
}

const notice = (seq: number) => frames.item(item("S-1", seq, { kind: "notice", code: "info", text: `item ${seq}` }));

function beta(patch = {}) {
  return chatSession("S-2", { project_id: "P-2", project_name: "beta", worktree_dir: "/home/dev/beta", ...patch });
}

describe("one stream at a time", () => {
  it("opens the overview when the shell mounts, and only one stream is open", async () => {
    const fake = seeded();
    await boot(fake);
    expect(fake.count("openStream")).toBe(1);
    expect(fake.streams[0]?.options).toEqual({});
    expect(fake.open()).toHaveLength(1);
  });

  it("follows the chat that opens, with `after` its highest held seq, and goes back to the overview", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [notice(1), notice(2), notice(5)]);
    expect(fake.count("openStream")).toBe(2);
    expect(fake.streams[1]?.options).toEqual({ session: "S-1", after: 0 });
    expect(fake.streams[0]?.closed).toBe(true);
    expect(fake.open()).toHaveLength(1);

    // No chat on screen: the overview again.
    await user.click(rail().getByRole("button", { name: "Projects" }));
    await waitFor(() => expect(fake.count("openStream")).toBe(3));
    expect(fake.streams[2]?.options).toEqual({});
    expect(fake.streams[1]?.closed).toBe(true);
    expect(fake.open()).toHaveLength(1);

    // The chat's tab brings it back, after the highest seq the UI holds (5).
    await user.click(screen.getByRole("tab", { name: "Orchestrator · alpha" }));
    await waitFor(() => expect(fake.count("openStream")).toBe(4));
    expect(fake.streams[3]?.options).toEqual({ session: "S-1", after: 5 });
    expect(fake.open()).toHaveLength(1);
  });

  it("moves the stream when another chat is on screen, one stream open each time", async () => {
    const fake = seeded({ sessions: [alpha(), beta()] });
    const user = await bootChat(fake, "S-1", [notice(3)]);
    await user.click(sidebar().getByRole("button", { name: /session in beta/ }));
    await waitFor(() => expect(fake.current()?.options).toEqual({ session: "S-2", after: 0 }));
    expect(fake.open()).toHaveLength(1);
    await user.click(screen.getByRole("tab", { name: "Orchestrator · alpha" }));
    await waitFor(() => expect(fake.current()?.options).toEqual({ session: "S-1", after: 3 }));
    expect(fake.open()).toHaveLength(1);
  });

  it("closing the tab on screen goes back to the previous page and to the overview", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Close Orchestrator · alpha" }));
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(within(screen.getByRole("banner")).getByText("Projects")).toBeTruthy();
    await waitFor(() => expect(fake.current()?.options).toEqual({}));
    expect(fake.open()).toHaveLength(1);
  });

  it("makes no call but the stream's own when a chat opens from the sidebar", async () => {
    const fake = seeded();
    await bootChat(fake);
    // The first chat reads the session list once, for the runtimes of the composer.
    await waitFor(() => expect(fake.count("sessionList")).toBe(1));
    expect(fake.calls.filter((c) => c !== "openStream")).toEqual(["paired", "projectList", "sessionList"]);
  });
});

describe("what the stream's events do", () => {
  it("shows `reconnecting` on the open chat and opens nothing", async () => {
    const fake = seeded();
    await bootChat(fake);
    const opens = fake.count("openStream");
    const calls = fake.calls.length;
    await emit(fake, { event: "reconnecting", after: 0, retry_in_ms: 4000, reason: "the server is stopping" });

    expect(main().getByText("Reconnecting… the server is stopping")).toBeTruthy();
    expect(fake.count("openStream")).toBe(opens);
    expect(fake.calls).toHaveLength(calls);
    expect(screen.queryByText(/Not connected/)).toBeNull();

    // The next frame ends the notice.
    await push(fake, frames.hello(fake.sessions, "S-1"));
    expect(screen.queryByText(/Reconnecting/)).toBeNull();
  });

  it("`unreachable`: shows the banner, reopens only when the link is up, with the last seq", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [notice(1), notice(2)]);
    const opens = fake.count("openStream");
    await emit(fake, closed("unreachable", "the bridge closed the stream"));

    expect(screen.getByText(`Not connected to ${ADDRESS}`)).toBeTruthy();
    expect(fake.count("openStream")).toBe(opens);
    // The page stays on the chat, with what it held.
    expect(main().getByText("item 2")).toBeTruthy();
    // The one retry timer is the connection's, and it calls status().
    await user.click(screen.getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.count("openStream")).toBe(opens + 1));
    expect(fake.current()?.options).toEqual({ session: "S-1", after: 2 });
    expect(fake.count("status")).toBe(1);
    expect(screen.queryByText(/Not connected/)).toBeNull();
  });

  it("`unreachable` on the overview reopens the overview when the link is up", async () => {
    const fake = seeded();
    const user = await boot(fake);
    await emit(fake, closed("unreachable", "the bridge closed the stream"));
    expect(fake.open()).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.count("openStream")).toBe(2));
    expect(fake.current()?.options).toEqual({});
  });

  it("`rate_limited`: shows the server's text and reopens when the link is up", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [notice(1)]);
    const opens = fake.count("openStream");
    await emit(fake, closed("rate_limited", "too many failed calls; wait a minute"));
    expect(screen.getByText("too many failed calls; wait a minute")).toBeTruthy();
    expect(fake.count("openStream")).toBe(opens);
    await user.click(screen.getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.count("openStream")).toBe(opens + 1));
    expect(fake.current()?.options).toEqual({ session: "S-1", after: 1 });
  });

  it.each([
    ["unauthorized", "this device was revoked; pair it again", `This device is no longer paired with ${ADDRESS}.`],
    ["key_changed", "server key changed: re-pair", "server key changed: re-pair"],
  ] as const)("`%s`: shows the banner and never reopens", async (kind, message, banner) => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [notice(1)]);
    const opens = fake.count("openStream");
    await emit(fake, closed(kind, message));

    expect(screen.getByText(banner)).toBeTruthy();
    expect(screen.queryByText(/Retrying/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Retry now" })).toBeNull();
    expect(screen.getByRole("button", { name: "Pair again" })).toBeTruthy();
    // Even a move to another page or a tab opens no stream while the device is unpaired.
    await user.click(rail().getByRole("button", { name: "Projects" }));
    await user.click(screen.getByRole("tab", { name: "Orchestrator · alpha" }));
    expect(fake.count("openStream")).toBe(opens);
    expect(fake.open()).toHaveLength(0);
  });

  it.each(["server", "bad_request", "forbidden", "refused"] as const)(
    "`%s`: shows one error line with Reconnect, and only its click reopens",
    async (kind) => {
      const fake = seeded();
      const user = await bootChat(fake, "S-1", [notice(1), notice(4)]);
      const opens = fake.count("openStream");
      await emit(fake, closed(kind, `the stream failed: ${kind}`));

      const line = within(main().getByRole("alert"));
      expect(line.getByText(`the stream failed: ${kind}`)).toBeTruthy();
      // No disconnect, no banner, no new stream by itself.
      expect(screen.queryByText(/Not connected/)).toBeNull();
      expect(fake.count("openStream")).toBe(opens);

      await user.click(line.getByRole("button", { name: "Reconnect" }));
      await waitFor(() => expect(fake.count("openStream")).toBe(opens + 1));
      expect(fake.current()?.options).toEqual({ session: "S-1", after: 4 });
      expect(main().queryByRole("alert")).toBeNull();
    },
  );

  it("a stream error on the overview shows its line in the sidebar, with Reconnect", async () => {
    const fake = seeded();
    const user = await boot(fake);
    await emit(fake, closed("server", "the server sent a frame this client cannot read"));
    const line = within(sidebar().getByRole("alert"));
    expect(line.getByText("the server sent a frame this client cannot read")).toBeTruthy();
    await user.click(line.getByRole("button", { name: "Reconnect" }));
    await waitFor(() => expect(fake.count("openStream")).toBe(2));
    expect(fake.current()?.options).toEqual({});
  });

  it("moving to another chat after a stream error opens that chat's stream", async () => {
    const fake = seeded({ sessions: [alpha(), beta()] });
    const user = await bootChat(fake);
    await emit(fake, closed("server", "boom"));
    await user.click(sidebar().getByRole("button", { name: /session in beta/ }));
    await waitFor(() => expect(fake.current()?.options).toEqual({ session: "S-2", after: 0 }));
  });
});

describe("the sidebar rows follow the hello and session frames", () => {
  it("shows each project's live sessions with the agent, the state, the branch and the mark", async () => {
    const fake = seeded({ sessions: [alpha({ branch: "feature-x", state: "needs_you" }), beta({ state: "working" })] });
    await boot(fake);
    expect(sidebar().queryByRole("button", { name: /session in/ })).toBeNull();
    await push(fake, frames.hello(fake.sessions));

    const rows = sidebar().getAllByRole("button", { name: /session in/ });
    expect(rows).toHaveLength(2);
    const first = within(rows[0] as HTMLElement);
    expect(first.getByText("Orchestrator")).toBeTruthy();
    expect(first.getByText("Needs you")).toBeTruthy();
    expect(first.getByText("feature-x")).toBeTruthy();
    expect(within(rows[1] as HTMLElement).getByText("Working")).toBeTruthy();
    // Each row sits under its own project.
    const alphaList = sidebar().getByRole("list", { name: "Sessions of alpha" });
    expect(within(alphaList).getAllByRole("button")).toHaveLength(1);
  });

  it("updates a row from a session frame, and adds a row for a new session", async () => {
    const fake = seeded({ sessions: [alpha()] });
    await boot(fake);
    await push(fake, frames.hello(fake.sessions));
    expect(sidebar().getByText("Idle")).toBeTruthy();

    await push(fake, frames.session(alpha({ state: "working", updated_at: "2026-11-09T10:01:00Z" })));
    expect(sidebar().getByText("Working")).toBeTruthy();
    expect(sidebar().queryByText("Idle")).toBeNull();

    await push(fake, frames.session(beta()));
    expect(sidebar().getAllByRole("button", { name: /session in/ })).toHaveLength(2);
  });

  it("removes a session that ended from the rows", async () => {
    const fake = seeded({ sessions: [alpha(), beta()] });
    await boot(fake);
    await push(fake, frames.hello(fake.sessions));
    expect(sidebar().getAllByRole("button", { name: /session in/ })).toHaveLength(2);

    await push(fake, frames.session(beta({ ended_at: "2026-11-09T11:00:00Z", updated_at: "2026-11-09T11:00:00Z" })));
    const rows = sidebar().getAllByRole("button", { name: /session in/ });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.getAttribute("aria-label")).toMatch(/session in alpha/);
  });

  it("treats a session that is missing from a new hello as ended", async () => {
    const fake = seeded({ sessions: [alpha(), beta()] });
    await boot(fake);
    await push(fake, frames.hello(fake.sessions));
    await push(fake, frames.hello([alpha()]));
    expect(sidebar().getAllByRole("button", { name: /session in/ })).toHaveLength(1);
  });

  it("opens a chat from a row, and focuses its tab when it is already open", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await user.click(rail().getByRole("button", { name: "Projects" }));
    await user.click(sidebar().getByRole("button", { name: /session in alpha/ }));
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" }).getAttribute("aria-selected")).toBe("true");
  });
});

describe("the UI keeps nothing but the sidebar's state", () => {
  it("holds only pf.sidebar in localStorage after a chat was used, and nothing in sessionStorage", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [
      frames.item(item("S-1", 1, { kind: "approval_request", request_id: "r", tool: "Bash", input: null, reason: null })),
    ]);
    await user.type(screen.getByRole("textbox", { name: "Message Orchestrator" }), "hello{Enter}");
    await waitFor(() => expect(fake.count("sessionSend")).toBe(1));
    await user.selectOptions(await screen.findByLabelText("Effort"), "low");
    await user.click(screen.getByRole("button", { name: "Allow" }));
    await waitFor(() => expect(fake.count("sessionAnswer")).toBe(1));

    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
    expect(window.sessionStorage.length).toBe(0);
    expect(document.cookie).toBe("");
  });
});

describe("a chat page makes no call by itself", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-09T10:09:00Z"));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  async function wiggle() {
    for (let i = 0; i < 2; i++) {
      await advance(30 * 60 * 1000);
      await act(async () => {
        window.dispatchEvent(new Event("blur"));
        document.dispatchEvent(new Event("visibilitychange"));
        window.dispatchEvent(new Event("focus"));
        window.dispatchEvent(new Event("online"));
      });
    }
  }

  it("calls nothing for an hour on a chat page, blurred or focused, with a request pending", async () => {
    const fake = seeded({ sessions: [alpha({ last_turn_at: "2026-11-09T10:00:00Z", cache_ttl_seconds: 3600 })] });
    await bootChatFake(fake);
    await push(fake, frames.item(item("S-1", 1, { kind: "approval_request", request_id: "r", tool: "Bash", input: null, reason: null })));
    const calls = [...fake.calls];
    expect(calls).toEqual(["paired", "projectList", "openStream", "sessionList", "openStream"]);
    expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();

    await wiggle();
    expect(fake.calls).toEqual(calls);
    expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();
    expect(fake.open()).toHaveLength(1);
  });

  it("calls nothing for an hour after an `unauthorized` stream end, and opens no stream", async () => {
    const fake = seeded();
    await bootChatFake(fake);
    await act(async () => {
      fake.emit(closed("unauthorized", "this device was revoked; pair it again"));
    });
    const calls = [...fake.calls];
    await wiggle();
    expect(fake.calls).toEqual(calls);
    expect(fake.open()).toHaveLength(0);
    // The banner has no timer: nothing retries.
    expect(screen.queryByText(/Retrying/)).toBeNull();
  });

  it("an `unreachable` stream end retries only status(), on the connection's schedule", async () => {
    const fake = seeded();
    await bootChatFake(fake);
    fake.fail("status", "unreachable", "no answer");
    await act(async () => {
      fake.emit(closed("unreachable", "the bridge closed the stream"));
    });
    const opens = fake.count("openStream");
    expect(screen.getByText(/Retrying in 4s/)).toBeTruthy();
    await advance(4000);
    expect(fake.count("status")).toBe(1);
    expect(fake.count("openStream")).toBe(opens);

    // The link comes back: status() succeeds, the session list is read once, and one stream opens.
    fake.heal();
    await advance(8000);
    expect(screen.queryByText(/Not connected/)).toBeNull();
    expect(fake.count("openStream")).toBe(opens + 1);
    const lists = fake.count("sessionList");
    await advance(60 * 60 * 1000);
    expect(fake.count("sessionList")).toBe(lists);
    expect(fake.count("openStream")).toBe(opens + 1);
  });
});
