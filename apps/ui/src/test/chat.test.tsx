// AC-2: the chat screen: the header, the message stream, send, interrupt and end.
import type { ChatItemBody } from "@pulse-factory/api-client";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { clock } from "../components/format";
import { alpha, bootChat, main, push, seeded } from "./chat-helpers";
import { claudeRuntime, frames, item, project } from "./fake-transport";

const field = (label: string) => screen.getByLabelText(label);
const box = () => screen.getByRole("textbox", { name: "Message Orchestrator" }) as HTMLTextAreaElement;
const sendButton = () => screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
const at = (text: string) => clock(Date.parse(text));

function text(value: string) {
  return { text: value, bytes: value.length, truncated: false };
}

function itemFrame(seq: number, body: ChatItemBody, patch = {}) {
  return frames.item(item("S-1", seq, body, patch));
}

describe("the chat header", () => {
  it("shows the project, the worktree with its full path, the branch, the state and the cost", async () => {
    const fake = seeded({ sessions: [alpha({ branch: "feature-x", state: "needs_you" })] });
    await bootChat(fake);

    expect(field("Project").textContent).toBe("alpha");
    expect(field("Worktree").textContent).toBe("alpha");
    expect(field("Worktree").getAttribute("title")).toBe("/home/dev/alpha");
    expect(field("Branch").textContent).toBe("feature-x");
    expect(field("State").textContent).toBe("Needs you");
    expect(field("Cost").textContent).toBe("0 wu");
    expect(within(main().getByRole("group", { name: "Chat header" })).getByText("Orchestrator")).toBeTruthy();
    // The cost is the number, a space and the `wu` chip; the title holds the dollars.
    expect(field("Cost").querySelector(".ui-chip")?.textContent).toBe("wu");
    // The tab is named after the agent and the project.
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
  });

  it("draws the worktree chip with the archive icon and the branch chip with git-branch, as 11a to 11c do", async () => {
    await bootChat(seeded({ sessions: [alpha({ branch: "feature-x" })] }));
    expect(field("Worktree").querySelector("[data-icon=archive]")).not.toBeNull();
    expect(field("Worktree").querySelector("[data-icon=folder]")).toBeNull();
    expect(field("Branch").querySelector("[data-icon=git-branch]")).not.toBeNull();
  });

  it("caps the thread and the composer at the canvas's 736px column (672px of content inside 32px padding)", async () => {
    await bootChat(seeded({ sessions: [alpha({ branch: "feature-x" })] }));
    const thread = main().getByRole("log", { name: "Messages" }).querySelector(".column") as HTMLElement;
    const foot = document.querySelector(".chat-foot > .column") as HTMLElement;
    expect(thread.style.maxWidth).toBe("736px");
    expect(foot.style.maxWidth).toBe("736px");
  });

  it("shows `detached` for a detached worktree and `folder` for a plain folder", async () => {
    const detached = seeded({ sessions: [alpha({ branch: null, detached: true })] });
    await bootChat(detached);
    expect(field("Branch").textContent).toBe("detached");
  });

  it("shows `folder` when the project is a plain folder", async () => {
    const folder = { ...project(1, "alpha"), kind: "folder" as const };
    const fake = seeded({ projects: [folder, project(2, "beta")], sessions: [alpha({ branch: null })] });
    await bootChat(fake);
    expect(field("Branch").textContent).toBe("folder");
  });

  it("marks nothing for a session with no process", async () => {
    const none = seeded({ sessions: [alpha({ process: null })] });
    await bootChat(none);
    expect(within(main().getByRole("group", { name: "Chat header" })).queryByText("unqualified")).toBeNull();
  });

  it("shows `unqualified` with its explanation when process.qualified is false", async () => {
    const fake = seeded({
      sessions: [
        alpha({
          process: {
            since: "2026-11-09T10:00:00Z",
            runtime_version: "9.9.9",
            qualified: false,
            turn_since: null,
            waiting_since: null,
            tasks: [],
            releases_at: null,
          },
        }),
      ],
    });
    await bootChat(fake);
    const mark = within(main().getByRole("group", { name: "Chat header" })).getByText("unqualified");
    expect(mark.getAttribute("title")).toMatch(/override binary/);
    // The sidebar row carries the mark too.
    expect(within(screen.getByRole("region", { name: "Sidebar" })).getByText("unqualified")).toBeTruthy();
  });

  it("does not mark a qualified process", async () => {
    const fake = seeded({
      sessions: [
        alpha({
          process: {
            since: "2026-11-09T10:00:00Z",
            runtime_version: "2.1.319",
            qualified: true,
            turn_since: null,
            waiting_since: null,
            tasks: [],
            releases_at: null,
          },
        }),
      ],
    });
    await bootChat(fake);
    expect(screen.queryByText("unqualified")).toBeNull();
  });

  it("reads the branch from the latest record", async () => {
    const fake = seeded();
    await bootChat(fake);
    expect(field("Branch").textContent).toBe("main");
    await push(fake, frames.session(alpha({ branch: "d19-branch", updated_at: "2026-11-09T10:05:00Z" })));
    expect(field("Branch").textContent).toBe("d19-branch");
  });
});

describe("context lost and ended (C15)", () => {
  it("says the context is lost, offers End session and cannot send", async () => {
    const fake = seeded({ sessions: [alpha({ context_lost: "the transcript is missing" })] });
    await bootChat(fake);

    expect(
      main().getByText("Context lost: the transcript is missing. End this session and start a new one."),
    ).toBeTruthy();
    expect(main().getAllByRole("button", { name: "End session" }).length).toBeGreaterThan(0);
    expect(box().disabled).toBe(true);
    expect(sendButton().disabled).toBe(true);
    expect(fake.count("sessionSend")).toBe(0);
  });

  it("shows a context_lost notice inline like any notice", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [
      itemFrame(1, { kind: "notice", code: "context_lost", text: "The runtime cannot resume this session." }),
    ]);
    expect(main().getByText("The runtime cannot resume this session.")).toBeTruthy();
  });

  it("shows an ended session as read only", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [
      itemFrame(1, { kind: "approval_request", request_id: "r1", tool: "Bash", input: text("touch x"), reason: null }),
    ]);
    expect(main().getByRole("button", { name: "Allow" })).toBeTruthy();

    await push(fake, frames.session(alpha({ ended_at: "2026-11-09T11:00:00Z", updated_at: "2026-11-09T11:00:00Z" })));
    // The ended panel takes the composer's place.
    expect(main().getByText("This session has ended.")).toBeTruthy();
    expect(main().getByText("The chat stays. Start session opens a new chat in the same project.")).toBeTruthy();
    expect(screen.queryByRole("textbox", { name: "Message Orchestrator" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    expect(main().queryByRole("button", { name: "Allow" })).toBeNull();
    expect(main().queryByRole("button", { name: "Deny" })).toBeNull();
    // The session leaves the sidebar rows, and its tab stays.
    expect(within(screen.getByRole("region", { name: "Sidebar" })).queryByText("Orchestrator")).toBeNull();
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
  });
});

describe("the message stream", () => {
  const USAGE = { input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 1 };
  const result = (patch: Partial<Extract<ChatItemBody, { kind: "turn_result" }>> = {}): ChatItemBody => ({
    kind: "turn_result",
    cause: "result",
    usage: USAGE,
    is_error: false,
    stop: "end_turn",
    duration_ms: 72_000,
    cache_ttl_seconds: 3600,
    ...patch,
  });

  it("renders each kind of item as one article with its seq, the tool calls folded into the turn's disclosure", async () => {
    const fake = seeded();
    const first = "2026-11-09T10:00:00Z";
    await bootChat(fake, "S-1", [
      // Delivered out of order on purpose: the stream shows them by seq.
      itemFrame(2, { kind: "assistant_text", text: "Line one\nLine two" }),
      itemFrame(1, { kind: "operator_message", text: "What does this project hold?" }, { created_at: first }),
      itemFrame(3, {
        kind: "tool_call",
        call_id: "c1",
        tool: "Bash",
        input: text("ls -la"),
        output: { text: "a\nb", bytes: 5000, truncated: true },
        is_error: true,
        finished: true,
      }),
      itemFrame(4, {
        kind: "background_task",
        event: "started",
        task_id: "t1",
        task_kind: "bash",
        description: "cargo build",
        end: null,
        summary: null,
      }),
      itemFrame(5, {
        kind: "background_task",
        event: "finished",
        task_id: "t1",
        task_kind: "bash",
        description: "cargo build",
        end: "completed",
        summary: "built in 40 s",
      }),
      itemFrame(6, { kind: "notice", code: "info", text: "The chat process was released." }),
      itemFrame(7, result()),
      itemFrame(8, result({ is_error: true, stop: "max_turns", duration_ms: 5_000 })),
      itemFrame(9, { kind: "redacted", item_kind: "operator_message" }),
      itemFrame(10, { kind: "unknown", item_kind: "hologram" }),
    ]);

    const log = within(main().getByRole("log", { name: "Messages" }));
    // The turn is closed once its result is held, so its tool calls are folded.
    expect(log.queryByRole("article", { name: "Tool call" })).toBeNull();
    await userEvent.setup().click(log.getByRole("button", { name: /Worked for 1m 12s/ }));

    const articles = log.getAllByRole("article");
    // The disclosure comes before the reply's later items; every item is one article with its seq.
    expect(articles.map((a) => a.getAttribute("aria-label"))).toEqual([
      "Operator message",
      "Reply",
      "Turn result",
      "Tool call",
      "Background task",
      "Background task",
      "Notice",
      "Turn result",
      "Redacted item",
      "Unsupported item",
    ]);
    expect(articles.map((a) => a.getAttribute("data-seq")).sort((a, b) => Number(a) - Number(b))).toEqual(
      ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"],
    );

    const operator = within(articles[0] as HTMLElement);
    expect(operator.getByText("What does this project hold?")).toBeTruthy();
    expect(operator.getByText(at(first))).toBeTruthy();

    // Line breaks stay, as text.
    const reply = log.getByText((_, el) => el?.tagName === "P" && el.textContent === "Line one\nLine two");
    expect(reply).toBeTruthy();

    const tool = within(log.getByRole("article", { name: "Tool call" }));
    expect(tool.getByText("Bash")).toBeTruthy();
    expect(tool.getAllByText("ls -la").length).toBeGreaterThan(0);
    expect(tool.getByText(/a\s+b/)).toBeTruthy();
    expect(tool.getByText("truncated, 5000 bytes")).toBeTruthy();
    expect(tool.getByText("error")).toBeTruthy();
    expect(tool.queryByText("running")).toBeNull();

    expect(log.getByText("Background task started: cargo build")).toBeTruthy();
    expect(log.getByText("Background task finished: cargo build (built in 40 s)")).toBeTruthy();
    expect(log.getByText("The chat process was released.")).toBeTruthy();
    expect(log.getByText("The turn failed: max_turns")).toBeTruthy();
    expect(log.getByText("Redacted operator_message")).toBeTruthy();
    expect(log.getByText("An item this app cannot show (hologram)")).toBeTruthy();
  });

  it("renders an answer item inside its card, not as an entry of its own", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [
      itemFrame(1, { kind: "approval_request", request_id: "r1", tool: "Bash", input: text("touch x"), reason: null }),
      itemFrame(
        2,
        { kind: "approval_answer", outcome: "answered", reason: null, decision: "deny", message: "no" },
        { answers_seq: 1 },
      ),
    ]);
    const log = within(main().getByRole("log", { name: "Messages" }));
    expect(log.getAllByRole("article")).toHaveLength(1);
    const card = within(log.getByRole("article", { name: "Approval request" }));
    expect(card.getByText("Denied")).toBeTruthy();
    expect(card.getByText("no")).toBeTruthy();
    expect(card.queryByRole("button", { name: "Allow" })).toBeNull();
  });

  it("shows the deltas of the turn in progress, then clears them at the assistant_text item", async () => {
    const fake = seeded();
    await bootChat(fake);
    await push(fake, frames.delta("S-1", "Hel"), frames.delta("S-1", "lo"));
    const progress = within(main().getByRole("article", { name: "Reply in progress" }));
    expect(progress.getByText("Hello")).toBeTruthy();

    await push(fake, itemFrame(2, { kind: "assistant_text", text: "Hello there" }));
    expect(main().queryByRole("article", { name: "Reply in progress" })).toBeNull();
    expect(within(main().getByRole("article", { name: "Reply" })).getByText("Hello there")).toBeTruthy();
  });

  it("clears the deltas at the turn result too", async () => {
    const fake = seeded();
    await bootChat(fake);
    await push(fake, frames.delta("S-1", "half a repl"));
    expect(main().getByRole("article", { name: "Reply in progress" })).toBeTruthy();
    await push(
      fake,
      itemFrame(3, {
        kind: "turn_result",
        cause: "interrupt",
        usage: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 0 },
        is_error: false,
        stop: "interrupted",
        duration_ms: 1000,
        cache_ttl_seconds: null,
      }),
    );
    expect(main().queryByRole("article", { name: "Reply in progress" })).toBeNull();
  });

  it("shows a running tool call until its finished item arrives", async () => {
    const fake = seeded();
    await bootChat(fake);
    await push(fake, frames.toolRunning("S-1", "c1", "Bash"));
    const running = within(main().getByRole("article", { name: "Tool call" }));
    expect(running.getByText("Bash")).toBeTruthy();
    expect(running.getByText("running")).toBeTruthy();

    await push(
      fake,
      itemFrame(2, {
        kind: "tool_call",
        call_id: "c1",
        tool: "Bash",
        input: text("touch x"),
        output: text("done"),
        is_error: false,
        finished: true,
      }),
    );
    const calls = main().getAllByRole("article", { name: "Tool call" });
    expect(calls).toHaveLength(1);
    expect(within(calls[0] as HTMLElement).queryByText("running")).toBeNull();
    expect(within(calls[0] as HTMLElement).getByText("done")).toBeTruthy();
  });

  it("replaces a held item with a redaction's re-send, and never doubles an item", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [itemFrame(1, { kind: "operator_message", text: "my secret" })]);
    expect(main().getByText("my secret")).toBeTruthy();

    // A reopened stream sends the same item again: still one entry.
    await push(fake, itemFrame(1, { kind: "operator_message", text: "my secret" }));
    expect(main().getAllByRole("article")).toHaveLength(1);

    await push(
      fake,
      itemFrame(1, { kind: "redacted", item_kind: "operator_message" }, { redacted_at: "2026-11-09T10:30:00Z" }),
    );
    expect(main().queryByText("my secret")).toBeNull();
    expect(main().getByText("Redacted operator_message")).toBeTruthy();
    expect(main().getAllByRole("article")).toHaveLength(1);
  });

  it("sets no text as HTML", async () => {
    const fake = seeded();
    const html = '<img src=x onerror="window.__owned = 1"><b>bold</b>';
    await bootChat(fake, "S-1", [
      itemFrame(1, { kind: "assistant_text", text: html }),
      itemFrame(2, { kind: "notice", code: "x", text: "<i>notice</i>" }),
    ]);
    expect(main().getByText(html)).toBeTruthy();
    expect(main().getByText("<i>notice</i>")).toBeTruthy();
    expect(main().getByRole("log", { name: "Messages" }).querySelector("img, b, i")).toBeNull();
    expect((window as unknown as { __owned?: number }).__owned).toBeUndefined();
  });
});

describe("send, interrupt and end", () => {
  it("sends the text with a command ID, then clears the box", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await user.type(box(), "What is in here?");
    await user.click(sendButton());

    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.sendArgs[0]).toMatchObject({ session_id: "S-1", text: "What is in here?" });
    expect(fake.sendArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    await waitFor(() => expect(box().value).toBe(""));
  });

  it("sends on Enter and adds a line on Shift+Enter", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await user.type(box(), "one{Shift>}{Enter}{/Shift}two{Enter}");
    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.sendArgs[0]?.text).toBe("one\ntwo");
  });

  it("sends nothing for an empty box", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await user.click(sendButton());
    await user.type(box(), "   {Enter}");
    expect(fake.count("sessionSend")).toBe(0);
  });

  it("allows a send during a turn, with the runtime's hint", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    const user = await bootChat(fake);
    expect(main().getByText("steers the running turn")).toBeTruthy();
    await user.type(box(), "wait, also this{Enter}");
    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
  });

  it("says a message runs as the next turn when the runtime does not steer", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    fake.runtimes = [claudeRuntime([], { mid_turn: "next_turn" })];
    await bootChat(fake);
    expect(await main().findByText("runs as the next turn")).toBeTruthy();
  });

  it("keeps the text and the command ID after unreachable, and Retry reuses both", async () => {
    const fake = seeded();
    fake.fail("sessionSend", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake);
    await user.type(box(), "first try");
    await user.click(sendButton());

    expect(await main().findByText("the server did not answer")).toBeTruthy();
    expect(box().value).toBe("first try");
    await user.click(main().getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(fake.sendArgs).toHaveLength(2));
    expect(fake.sendArgs[1]).toMatchObject({ text: "first try" });
    expect(fake.sendArgs[1]?.command_id).toBe(fake.sendArgs[0]?.command_id);
    await waitFor(() => expect(box().value).toBe(""));
    expect(main().queryByRole("button", { name: "Retry" })).toBeNull();

    // The next message is a new command.
    await user.type(box(), "second{Enter}");
    await waitFor(() => expect(fake.sendArgs).toHaveLength(3));
    expect(fake.sendArgs[2]?.command_id).not.toBe(fake.sendArgs[0]?.command_id);
  });

  it("uses a new command ID when the text changes after unreachable", async () => {
    const fake = seeded();
    fake.fail("sessionSend", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake);
    await user.type(box(), "first");
    await user.click(sendButton());
    await main().findByText("the server did not answer");
    await user.type(box(), " and more");
    await user.click(sendButton());
    await waitFor(() => expect(fake.sendArgs).toHaveLength(2));
    expect(fake.sendArgs[1]?.command_id).not.toBe(fake.sendArgs[0]?.command_id);
  });

  it("shows the note of an answer, and the next send gets a new command ID", async () => {
    const fake = seeded();
    fake.noteOnce("sessionSend", "the chat process did not open: the claude binary is missing");
    const user = await bootChat(fake);
    await user.type(box(), "hello{Enter}");

    expect(await main().findByText("the chat process did not open: the claude binary is missing")).toBeTruthy();
    expect(fake.sendArgs).toHaveLength(1);
    await user.type(box(), "hello{Enter}");
    await waitFor(() => expect(fake.sendArgs).toHaveLength(2));
    expect(fake.sendArgs[1]?.command_id).not.toBe(fake.sendArgs[0]?.command_id);
  });

  it("shows the server's text for a refused send and keeps the text", async () => {
    const fake = seeded();
    fake.fail("sessionSend", "refused", "no room under the chat process cap (4)", 1);
    const user = await bootChat(fake);
    await user.type(box(), "hi");
    await user.click(sendButton());
    expect(await main().findByText("no room under the chat process cap (4)")).toBeTruthy();
    expect(box().value).toBe("hi");
    expect(main().queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("shows Stop only while the state is working, and Send otherwise", async () => {
    const idle = seeded();
    await bootChat(idle);
    expect(main().queryByRole("button", { name: "Stop" })).toBeNull();
    expect(main().queryByRole("button", { name: "Interrupt" })).toBeNull();
    expect(sendButton()).toBeTruthy();
  });

  it("Stop sends an interrupt with a command ID while the state is working, and takes Send's place", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    const user = await bootChat(fake);
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    await user.click(main().getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(fake.interruptArgs).toHaveLength(1));
    expect(fake.interruptArgs[0]?.session_id).toBe("S-1");
    expect(fake.interruptArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    // The answer's record says idle: Stop goes and Send comes back.
    await waitFor(() => expect(main().queryByRole("button", { name: "Stop" })).toBeNull());
    expect(sendButton()).toBeTruthy();
  });
});

describe("End session (11b)", () => {
  const strip = () => within(screen.getByRole("group", { name: "Confirm end" }));

  it("opens a strip under the header; Keep it calls nothing and End session calls sessionEnd once", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    expect(screen.queryByRole("group", { name: "Confirm end" })).toBeNull();
    await user.click(main().getByRole("button", { name: "End session" }));
    expect(fake.count("sessionEnd")).toBe(0);
    expect(strip().getByText("End this session?")).toBeTruthy();
    expect(strip().getByText(/process on .* stops, mid-turn if it is working\. The chat stays, and you can start the session again from here\./)).toBeTruthy();
    expect(strip().getByText(/The Orchestrator's process/)).toBeTruthy();

    await user.click(strip().getByRole("button", { name: "Keep it" }));
    expect(screen.queryByRole("group", { name: "Confirm end" })).toBeNull();
    expect(fake.count("sessionEnd")).toBe(0);

    await user.click(main().getByRole("button", { name: "End session" }));
    await user.click(strip().getByRole("button", { name: "End session" }));
    await waitFor(() => expect(fake.endArgs).toHaveLength(1));
    expect(fake.endArgs[0]?.session_id).toBe("S-1");
    expect(fake.endArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    expect(await main().findByText("This session has ended.")).toBeTruthy();
    expect(fake.count("sessionEnd")).toBe(1);
    expect(screen.queryByRole("group", { name: "Confirm end" })).toBeNull();
  });

  it("opens from the context-lost line too, and uses no dialog", async () => {
    const fake = seeded({ sessions: [alpha({ context_lost: "the transcript is missing" })] });
    const user = await bootChat(fake);
    const lost = main().getByRole("status");
    await user.click(within(lost).getByRole("button", { name: "End session" }));
    expect(strip().getByText("End this session?")).toBeTruthy();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(fake.count("sessionEnd")).toBe(0);
  });

  it("shows the server's text when the end is refused, and keeps the session", async () => {
    const fake = seeded();
    fake.fail("sessionEnd", "refused", "the session is busy", 1);
    const user = await bootChat(fake);
    await user.click(main().getByRole("button", { name: "End session" }));
    await user.click(strip().getByRole("button", { name: "End session" }));
    expect(await main().findByText("the session is busy")).toBeTruthy();
    expect(main().queryByText("This session has ended.")).toBeNull();
  });
});

describe("the ended chat (11a)", () => {
  const ENDED = { ended_at: "2026-11-09T11:00:00Z", updated_at: "2026-11-09T11:00:00Z" };
  /** Opens the live chat, then the session ends (an ended session has no sidebar row to open it from). */
  async function endedChat() {
    const fake = seeded();
    const user = await bootChat(fake);
    await push(fake, frames.session(alpha(ENDED)));
    return { fake, user };
  }
  const head = () => within(main().getByRole("group", { name: "Chat header" }));

  it("shows the Ended pill and Start session in End session's place, and the panel instead of the composer", async () => {
    await endedChat();
    expect(field("State").textContent).toBe("Ended");
    expect(head().getByRole("button", { name: "Start session" })).toBeTruthy();
    expect(head().queryByRole("button", { name: "End session" })).toBeNull();
    expect(main().getByText("This session has ended.")).toBeTruthy();
    expect(screen.queryByRole("textbox", { name: "Message Orchestrator" })).toBeNull();
  });

  it("Start session in the header calls sessionStart with the project and the agent, and opens the new session", async () => {
    const { fake, user } = await endedChat();
    await user.click(head().getByRole("button", { name: "Start session" }));
    await waitFor(() => expect(fake.startArgs).toHaveLength(1));
    expect(fake.startArgs[0]).toEqual({ project_id: "P-1", agent_id: "orchestrator", command_id: expect.stringMatching(/^[0-9a-f-]{16,}$/) });
    await waitFor(() => expect(fake.current()?.options.session).not.toBe("S-1"));
  });

  it("the panel's Start session does the same", async () => {
    const { fake, user } = await endedChat();
    const buttons = main().getAllByRole("button", { name: "Start session" });
    expect(buttons).toHaveLength(2);
    await user.click(buttons[1] as HTMLElement);
    await waitFor(() => expect(fake.startArgs).toHaveLength(1));
    expect(fake.startArgs[0]).toMatchObject({ project_id: "P-1", agent_id: "orchestrator" });
  });
});

describe("the chat header (11a)", () => {
  it("shows the avatar, the agent, the project after a dot, and the chips with their icons", async () => {
    await bootChat(seeded());
    const head = within(main().getByRole("group", { name: "Chat header" }));
    expect(head.getByRole("heading", { name: "Orchestrator" })).toBeTruthy();
    expect(head.getByText(/^·/).textContent).toContain("alpha");
    expect(main().getByRole("group", { name: "Chat header" }).querySelector("[data-agent='orchestrator']")).not.toBeNull();
    expect(field("Worktree").classList.contains("ui-chip")).toBe(true);
    expect(field("Branch").classList.contains("ui-chip")).toBe(true);
  });

  it("the gear opens the Agents page", async () => {
    const user = await bootChat(seeded());
    await user.click(main().getByRole("button", { name: "Agent settings" }));
    expect(await main().findByRole("region", { name: "Agent" })).toBeTruthy();
  });
});

describe("the stream's days, turns and notices (11a)", () => {
  const USAGE = { input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 1 };
  const done = (patch: Partial<Extract<ChatItemBody, { kind: "turn_result" }>> = {}): ChatItemBody => ({
    kind: "turn_result",
    cause: "result",
    usage: USAGE,
    is_error: false,
    stop: "end_turn",
    duration_ms: 72_000,
    cache_ttl_seconds: 3600,
    ...patch,
  });
  const tool = (seq: number, callId: string, name: string, input: string, finished = true) =>
    itemFrame(seq, { kind: "tool_call", call_id: callId, tool: name, input: text(input), output: text("ok"), is_error: false, finished });
  const log = () => within(main().getByRole("log", { name: "Messages" }));

  it("puts a divider before the first item of each UTC day: Today, Yesterday and the date", async () => {
    const now = new Date();
    const day = (offset: number) => new Date(now.getTime() - offset * 86_400_000).toISOString();
    const fake = seeded();
    await bootChat(fake, "S-1", [
      itemFrame(1, { kind: "operator_message", text: "old" }, { created_at: "2026-01-07T10:00:00Z" }),
      itemFrame(2, { kind: "operator_message", text: "old too" }, { created_at: "2026-01-07T11:00:00Z" }),
      itemFrame(3, { kind: "operator_message", text: "yesterday" }, { created_at: day(1) }),
      itemFrame(4, { kind: "operator_message", text: "today" }, { created_at: day(0) }),
    ]);
    const dividers = Array.from(document.querySelectorAll(".day-divider")).map((d) => d.textContent);
    expect(dividers).toEqual(["Jan 7", "Yesterday", "Today"]);
  });

  it("opens a turn with the avatar, the agent and the time of its first item", async () => {
    await bootChat(seeded(), "S-1", [
      itemFrame(1, { kind: "operator_message", text: "go" }),
      itemFrame(2, { kind: "assistant_text", text: "Done." }, { created_at: "2026-11-09T10:03:00Z" }),
      itemFrame(3, done()),
    ]);
    const head = document.querySelector(".turn-head") as HTMLElement;
    expect(head.querySelector("[data-agent='orchestrator']")).not.toBeNull();
    expect(within(head).getByText("Orchestrator")).toBeTruthy();
    expect(within(head).getByText(at("2026-11-09T10:03:00Z"))).toBeTruthy();
  });

  it("folds only the tool calls into 'Worked for', closed after the result; replies and cards stay outside, in seq order", async () => {
    await bootChat(seeded(), "S-1", [
      itemFrame(1, { kind: "operator_message", text: "go" }),
      tool(2, "c1", "Read", "docs/ROADMAP.md"),
      itemFrame(3, { kind: "assistant_text", text: "I read it." }),
      tool(4, "c2", "Grep", "sessions"),
      itemFrame(5, { kind: "question_request", request_id: "Q-1", items: [{ text: "Which?", header: "", options: [{ label: "A", description: "" }], multi_select: false }] }),
      itemFrame(6, done()),
    ]);
    const toggle = log().getByRole("button", { name: /Worked for 1m 12s/ });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(log().queryAllByRole("article", { name: "Tool call" })).toHaveLength(0);
    // Reply and question stay outside the disclosure.
    expect(log().getByRole("article", { name: "Reply" })).toBeTruthy();
    expect(log().getByRole("article", { name: "Question" })).toBeTruthy();
    expect(log().getAllByRole("article", { name: "Turn result" })).toHaveLength(1);
    await userEvent.setup().click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const disclosure = within(log().getByRole("article", { name: "Turn result" }));
    expect(disclosure.getAllByRole("article", { name: "Tool call" })).toHaveLength(2);
    // The disclosure holds tool calls only.
    expect(disclosure.queryByRole("article", { name: "Reply" })).toBeNull();
    expect(disclosure.queryByRole("article", { name: "Question" })).toBeNull();
    // A tool row shows the tool's name and a one-line target.
    expect(disclosure.getByText("Read")).toBeTruthy();
    expect(disclosure.getByText("docs/ROADMAP.md", { selector: ".tool-target" })).toBeTruthy();
  });

  it("takes the target of a tool call from its JSON input", async () => {
    await bootChat(seeded(), "S-1", [
      itemFrame(1, { kind: "operator_message", text: "go" }),
      tool(2, "c1", "Bash", JSON.stringify({ command: "cargo test -p store", description: "run" })),
      itemFrame(3, done()),
    ]);
    await userEvent.setup().click(log().getByRole("button", { name: /Worked for/ }));
    expect(log().getByText("cargo test -p store", { selector: ".tool-target" })).toBeTruthy();
  });

  it("is open and says Working… while the turn runs, with no Turn result label", async () => {
    await bootChat(seeded({ sessions: [alpha({ state: "working" })] }), "S-1", [
      itemFrame(1, { kind: "operator_message", text: "go" }),
      tool(2, "c1", "Read", "a.md"),
    ]);
    const toggle = log().getByRole("button", { name: /Working…/ });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(log().queryByRole("article", { name: "Turn result" })).toBeNull();
    expect(log().getByRole("article", { name: "Tool call" })).toBeTruthy();
    // The line under the last item says the agent works.
    expect(log().getByText("Orchestrator is working")).toBeTruthy();
  });

  it("closes the disclosure when the result arrives", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    await bootChat(fake, "S-1", [itemFrame(1, { kind: "operator_message", text: "go" }), tool(2, "c1", "Read", "a.md")]);
    expect(log().getByRole("article", { name: "Tool call" })).toBeTruthy();
    await push(fake, itemFrame(3, done({ duration_ms: 45_000 })), frames.session(alpha({ state: "idle", updated_at: "2026-11-09T10:09:00Z" })));
    expect(log().getByRole("button", { name: /Worked for 45s/ }).getAttribute("aria-expanded")).toBe("false");
    expect(log().queryByRole("article", { name: "Tool call" })).toBeNull();
    expect(log().queryByText("Orchestrator is working")).toBeNull();
  });

  it("says 'You stopped the turn' for an interrupted turn and 'The turn failed' for a failed one", async () => {
    await bootChat(seeded(), "S-1", [
      itemFrame(1, { kind: "operator_message", text: "go" }),
      itemFrame(2, done({ is_error: true, stop: "interrupted", cause: "interrupt", duration_ms: 3000 }), { created_at: "2026-11-09T10:52:00Z" }),
      itemFrame(3, { kind: "operator_message", text: "again" }),
      itemFrame(4, done({ is_error: true, stop: "max_turns" })),
    ]);
    expect(log().getByText(`You stopped the turn · ${at("2026-11-09T10:52:00Z")}. The session stays open.`)).toBeTruthy();
    expect(log().getByText("The turn failed: max_turns")).toBeTruthy();
  });

  it("shows the idle release as a centred system line with its time", async () => {
    await bootChat(seeded(), "S-1", [
      itemFrame(1, { kind: "notice", code: "idle_release", text: "The chat process was released." }, { created_at: "2026-11-09T13:20:00Z" }),
    ]);
    expect(log().getByText(`Session released after 30 idle minutes · ${at("2026-11-09T13:20:00Z")} · the next message resumes it`)).toBeTruthy();
    expect(log().queryByText("The chat process was released.")).toBeNull();
    expect(log().getByRole("article", { name: "Notice" }).className).toContain("entry-notice");
  });

  it("renders a reply as Markdown, and the live delta as plain text", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [itemFrame(1, { kind: "assistant_text", text: "Both change `crates/store` and **sessions**." })]);
    const reply = within(log().getByRole("article", { name: "Reply" }));
    expect(reply.getByText("crates/store").tagName).toBe("CODE");
    expect(reply.getByText("sessions").tagName).toBe("STRONG");
    await push(fake, frames.delta("S-1", "half **a**"));
    expect(within(log().getByRole("article", { name: "Reply in progress" })).getByText("half **a**")).toBeTruthy();
  });

  it("keeps the operator's time below the bubble", async () => {
    await bootChat(seeded(), "S-1", [itemFrame(1, { kind: "operator_message", text: "hello" }, { created_at: "2026-11-09T14:02:00Z" })]);
    const article = log().getByRole("article", { name: "Operator message" });
    const children = Array.from(article.children);
    expect(children[0]?.textContent).toBe("hello");
    expect(children[1]?.tagName).toBe("TIME");
    expect(children[1]?.textContent).toBe(at("2026-11-09T14:02:00Z"));
  });
});
