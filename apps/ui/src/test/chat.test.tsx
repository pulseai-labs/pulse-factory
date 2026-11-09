// AC-2: the chat screen: the header, the message stream, send, interrupt and end.
import type { ChatItemBody } from "@pulse-factory/api-client";
import { screen, waitFor, within } from "@testing-library/react";
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
    // The tab is named after the agent and the project.
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
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
    expect(main().getByText("This session ended")).toBeTruthy();
    expect(box().disabled).toBe(true);
    expect(sendButton().disabled).toBe(true);
    expect(main().queryByRole("button", { name: "Allow" })).toBeNull();
    expect(main().queryByRole("button", { name: "Deny" })).toBeNull();
    // The session leaves the sidebar rows, and its tab stays.
    expect(within(screen.getByRole("region", { name: "Sidebar" })).queryByText("Orchestrator")).toBeNull();
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
  });
});

describe("the message stream", () => {
  it("renders each kind of item, in seq order", async () => {
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
      itemFrame(6, { kind: "notice", code: "idle_release", text: "The chat process was released." }),
      itemFrame(7, {
        kind: "turn_result",
        cause: "result",
        usage: { input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 1 },
        is_error: false,
        stop: "end_turn",
        duration_ms: 72_000,
        cache_ttl_seconds: 3600,
      }),
      itemFrame(8, {
        kind: "turn_result",
        cause: "result",
        usage: { input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 1 },
        is_error: true,
        stop: "max_turns",
        duration_ms: 5_000,
        cache_ttl_seconds: 3600,
      }),
      itemFrame(9, { kind: "redacted", item_kind: "operator_message" }),
      itemFrame(10, { kind: "unknown", item_kind: "hologram" }),
    ]);

    const log = within(main().getByRole("log", { name: "Messages" }));
    const kinds = log.getAllByRole("article").map((a) => a.getAttribute("aria-label"));
    expect(kinds).toEqual([
      "Operator message",
      "Reply",
      "Tool call",
      "Background task",
      "Background task",
      "Notice",
      "Turn result",
      "Turn result",
      "Redacted item",
      "Unsupported item",
    ]);

    const operator = within(log.getAllByRole("article")[0] as HTMLElement);
    expect(operator.getByText("What does this project hold?")).toBeTruthy();
    expect(operator.getByText(at(first))).toBeTruthy();

    // Line breaks stay, as text.
    const reply = log.getByText((_, el) => el?.tagName === "P" && el.textContent === "Line one\nLine two");
    expect(reply).toBeTruthy();

    const tool = within(log.getByRole("article", { name: "Tool call" }));
    expect(tool.getByText("Bash")).toBeTruthy();
    expect(tool.getByText("ls -la")).toBeTruthy();
    expect(tool.getByText(/a\s+b/)).toBeTruthy();
    expect(tool.getByText("truncated, 5000 bytes")).toBeTruthy();
    expect(tool.getByText("error")).toBeTruthy();
    expect(tool.queryByText("running")).toBeNull();

    expect(log.getByText("Background task started: cargo build")).toBeTruthy();
    expect(log.getByText("Background task finished: cargo build (built in 40 s)")).toBeTruthy();
    expect(log.getByText("The chat process was released.")).toBeTruthy();
    expect(log.getByText("Worked for 1m 12s")).toBeTruthy();
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

  it("interrupts only while the state is working", async () => {
    const idle = seeded();
    await bootChat(idle);
    expect(main().queryByRole("button", { name: "Interrupt" })).toBeNull();
  });

  it("sends an interrupt with a command ID while the state is working", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    const user = await bootChat(fake);
    await user.click(main().getByRole("button", { name: "Interrupt" }));
    await waitFor(() => expect(fake.interruptArgs).toHaveLength(1));
    expect(fake.interruptArgs[0]?.session_id).toBe("S-1");
    expect(fake.interruptArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    // The answer's record says idle: the button goes.
    await waitFor(() => expect(main().queryByRole("button", { name: "Interrupt" })).toBeNull());
  });

  it("asks once before it ends the session, then shows it ended", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await user.click(main().getByRole("button", { name: "End session" }));
    expect(fake.count("sessionEnd")).toBe(0);
    expect(main().getByText("End this session?")).toBeTruthy();

    await user.click(main().getByRole("button", { name: "Keep it" }));
    expect(main().queryByText("End this session?")).toBeNull();
    expect(fake.count("sessionEnd")).toBe(0);

    await user.click(main().getByRole("button", { name: "End session" }));
    await user.click(main().getByRole("button", { name: "Yes, end it" }));
    await waitFor(() => expect(fake.endArgs).toHaveLength(1));
    expect(fake.endArgs[0]?.session_id).toBe("S-1");
    expect(fake.endArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    expect(await main().findByText("This session ended")).toBeTruthy();
  });

});
