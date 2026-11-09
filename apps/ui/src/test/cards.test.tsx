// AC-4: the approval and question cards (C2, M3): answered in place, still pending after a reload or a
// reconnect, and closed by the answer item.
import type { ChatItemBody, QuestionRecord } from "@pulse-factory/api-client";
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { clock } from "../components/format";
import { alpha, bootChat, main, push, seeded } from "./chat-helpers";
import { ADDRESS, frames, item } from "./fake-transport";

const text = (value: string) => ({ text: value, bytes: value.length, truncated: false });
const frame = (seq: number, body: ChatItemBody, patch = {}) => frames.item(item("S-1", seq, body, patch));

const approvalRequest = (seq = 1) =>
  frame(seq, { kind: "approval_request", request_id: "r1", tool: "Bash", input: text("touch pf-deny-test.txt"), reason: "writes a file" }, {
    created_at: "2026-11-09T10:14:00Z",
  });

const approvalCard = () => within(screen.getByRole("article", { name: "Approval request" }));
const questionCard = () => within(screen.getByRole("article", { name: "Question" }));

const question = (over: Partial<QuestionRecord> = {}): QuestionRecord => ({
  text: "Which option do you prefer?",
  header: "Option",
  options: [
    { label: "A", description: "the first" },
    { label: "B", description: "the second" },
    { label: "C", description: "the third" },
  ],
  multi_select: false,
  ...over,
});

const questionRequest = (questions: QuestionRecord[], seq = 1) =>
  frame(seq, { kind: "question_request", request_id: "q1", items: questions });

const answered = (seq: number, answers: number, body: ChatItemBody) => frame(seq, body, { answers_seq: answers });

describe("a pending approval", () => {
  it("shows the tool, its input, the reason and when it needs the operator", async () => {
    await bootChat(seeded(), "S-1", [approvalRequest()]);
    const card = approvalCard();
    expect(card.getByText("Bash")).toBeTruthy();
    expect(card.getByText("touch pf-deny-test.txt")).toBeTruthy();
    expect(card.getByText("writes a file")).toBeTruthy();
    expect(card.getByText(`Needs you since ${clock(Date.parse("2026-11-09T10:14:00Z"))}`)).toBeTruthy();
    expect(card.getByRole("button", { name: "Allow" })).toBeTruthy();
    expect(card.getByRole("button", { name: "Deny" })).toBeTruthy();
  });

  it("Allow sends the answer for that request's seq, with a command ID", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [approvalRequest(4)]);
    await user.click(approvalCard().getByRole("button", { name: "Allow" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]).toEqual({
      session_id: "S-1",
      seq: 4,
      answer: { type: "allow" },
      command_id: expect.stringMatching(/^[0-9a-f-]{16,}$/),
    });
  });

  it("Deny sends the typed message", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [approvalRequest()]);
    await user.type(approvalCard().getByRole("textbox", { name: "Message for a denial" }), "no");
    await user.click(approvalCard().getByRole("button", { name: "Deny" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]).toMatchObject({ seq: 1, answer: { type: "deny", value: { message: "no" } } });
  });

  it("Deny sends an empty message when none is typed", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [approvalRequest()]);
    await user.click(approvalCard().getByRole("button", { name: "Deny" }));
    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]?.answer).toEqual({ type: "deny", value: { message: "" } });
  });

  it("stays pending after the answer is sent, and closes when the answer item arrives", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [approvalRequest()]);
    await user.click(approvalCard().getByRole("button", { name: "Allow" }));
    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    // The card follows the items: no answer item yet, so it is not closed.
    expect(screen.getByRole("article", { name: "Approval request" })).toBeTruthy();
    expect(main().queryByText("Allowed")).toBeNull();

    await push(fake, answered(2, 1, { kind: "approval_answer", outcome: "answered", reason: null, decision: "allow", message: null }));
    expect(approvalCard().getByText("Allowed")).toBeTruthy();
    expect(approvalCard().queryByRole("button", { name: "Allow" })).toBeNull();
    expect(approvalCard().queryByRole("button", { name: "Deny" })).toBeNull();
  });

  it("shows the server's text when the request was already answered, and the card follows the items", async () => {
    const fake = seeded();
    fake.fail("sessionAnswer", "refused", "request 1 of S-1 is already answered, closed or lost", 1);
    const user = await bootChat(fake, "S-1", [approvalRequest()]);
    await user.click(approvalCard().getByRole("button", { name: "Allow" }));
    expect(await approvalCard().findByText("request 1 of S-1 is already answered, closed or lost")).toBeTruthy();
    // Still pending until the stream says otherwise.
    expect(approvalCard().getByRole("button", { name: "Allow" })).toBeTruthy();

    await push(fake, answered(2, 1, { kind: "approval_answer", outcome: "answered", reason: null, decision: "deny", message: "no" }));
    expect(approvalCard().getByText("Denied")).toBeTruthy();
    expect(approvalCard().queryByText("request 1 of S-1 is already answered, closed or lost")).toBeNull();
  });

  it("reuses the command ID when the operator retries after unreachable", async () => {
    const fake = seeded();
    fake.fail("sessionAnswer", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake, "S-1", [approvalRequest()]);
    await user.click(approvalCard().getByRole("button", { name: "Allow" }));
    expect(await approvalCard().findByText("the server did not answer")).toBeTruthy();
    await user.click(approvalCard().getByRole("button", { name: "Allow" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(2));
    expect(fake.answerArgs[1]?.command_id).toBe(fake.answerArgs[0]?.command_id);
  });

  it("answers nothing by itself: no call is made for a card that waits", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [approvalRequest()]);
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(fake.count("sessionAnswer")).toBe(0);
    expect(fake.count("sessionSend")).toBe(0);
  });
});

describe("a pending question", () => {
  it("shows each question with its header, text and numbered options", async () => {
    await bootChat(seeded(), "S-1", [questionRequest([question()])]);
    const card = questionCard();
    expect(card.getByText("Option")).toBeTruthy();
    expect(card.getByText("Which option do you prefer?")).toBeTruthy();
    expect(card.getByText("1.")).toBeTruthy();
    expect(card.getByText("3.")).toBeTruthy();
    expect(card.getByRole("radio", { name: "B" })).toBeTruthy();
    expect(card.getByText(/the second/)).toBeTruthy();
    expect(card.getByRole("textbox", { name: "Other answer" })).toBeTruthy();
    expect((card.getByRole("button", { name: "Answer" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("sends the chosen label under the question's text", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [questionRequest([question()], 3)]);
    await user.click(questionCard().getByRole("radio", { name: "B" }));
    await user.click(questionCard().getByRole("button", { name: "Answer" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]).toMatchObject({
      session_id: "S-1",
      seq: 3,
      answer: { type: "answers", value: { "Which option do you prefer?": "B" } },
    });
  });

  it("takes one choice for a single-select question", async () => {
    const user = await bootChat(seeded(), "S-1", [questionRequest([question()])]);
    await user.click(questionCard().getByRole("radio", { name: "A" }));
    await user.click(questionCard().getByRole("radio", { name: "C" }));
    expect((questionCard().getByRole("radio", { name: "A" }) as HTMLInputElement).checked).toBe(false);
    expect((questionCard().getByRole("radio", { name: "C" }) as HTMLInputElement).checked).toBe(true);
  });

  it("joins several choices of a multi-select question with a comma and a space", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [questionRequest([question({ multi_select: true })])]);
    await user.click(questionCard().getByRole("checkbox", { name: "A" }));
    await user.click(questionCard().getByRole("checkbox", { name: "C" }));
    await user.click(questionCard().getByRole("button", { name: "Answer" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]?.answer).toEqual({ type: "answers", value: { "Which option do you prefer?": "A, C" } });
  });

  it("sends a typed answer from the Other field", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [questionRequest([question()])]);
    await user.click(questionCard().getByRole("radio", { name: "A" }));
    await user.type(questionCard().getByRole("textbox", { name: "Other answer" }), "something else");
    // A typed answer replaces the choice of a single-select question.
    expect((questionCard().getByRole("radio", { name: "A" }) as HTMLInputElement).checked).toBe(false);
    await user.click(questionCard().getByRole("button", { name: "Answer" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]?.answer).toEqual({ type: "answers", value: { "Which option do you prefer?": "something else" } });
  });

  it("answers every question of the request in one call, and waits until each has an answer", async () => {
    const fake = seeded();
    const second = question({ text: "Which colour?", header: "Colour", options: [{ label: "red", description: "" }, { label: "blue", description: "" }] });
    const user = await bootChat(fake, "S-1", [questionRequest([question(), second])]);
    await user.click(within(questionCard().getByRole("group", { name: /Which option/ })).getByRole("radio", { name: "A" }));
    expect((questionCard().getByRole("button", { name: "Answer" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(within(questionCard().getByRole("group", { name: /Which colour/ })).getByRole("radio", { name: "blue" }));
    await user.click(questionCard().getByRole("button", { name: "Answer" }));

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]?.answer).toEqual({
      type: "answers",
      value: { "Which option do you prefer?": "A", "Which colour?": "blue" },
    });
  });

  it("shows the answered question with its answers", async () => {
    await bootChat(seeded(), "S-1", [
      questionRequest([question()]),
      answered(2, 1, { kind: "question_answer", outcome: "answered", reason: null, answers: { "Which option do you prefer?": "B" } }),
    ]);
    const card = questionCard();
    expect(card.getByText("Which option do you prefer?")).toBeTruthy();
    expect(card.getByText("B")).toBeTruthy();
    expect(card.queryByRole("button", { name: "Answer" })).toBeNull();
    expect(card.queryByRole("radio")).toBeNull();
  });
});

describe("how a closed card ends (M3)", () => {
  it("shows withdrawn with the reason", async () => {
    await bootChat(seeded(), "S-1", [
      approvalRequest(),
      answered(2, 1, { kind: "approval_answer", outcome: "withdrawn", reason: "the turn was interrupted", decision: null, message: null }),
    ]);
    expect(approvalCard().getByText("Withdrawn by the agent")).toBeTruthy();
    expect(approvalCard().getByText("the turn was interrupted")).toBeTruthy();
    expect(approvalCard().queryByRole("button", { name: "Allow" })).toBeNull();
  });

  it("shows lost with the reason", async () => {
    await bootChat(seeded(), "S-1", [
      approvalRequest(),
      answered(2, 1, { kind: "approval_answer", outcome: "lost", reason: "the controller restarted", decision: null, message: null }),
    ]);
    expect(approvalCard().getByText("Lost: the controller restarted")).toBeTruthy();
    expect(approvalCard().queryByRole("button", { name: "Deny" })).toBeNull();
  });

  it("shows a withdrawn and a lost question too", async () => {
    await bootChat(seeded(), "S-1", [
      questionRequest([question()], 1),
      answered(2, 1, { kind: "question_answer", outcome: "withdrawn", reason: "the agent moved on", answers: null }),
      questionRequest([question({ text: "Another?" })], 3),
      answered(4, 3, { kind: "question_answer", outcome: "lost", reason: "the session ended", answers: null }),
    ]);
    const cards = screen.getAllByRole("article", { name: "Question" }).map((c) => within(c));
    expect(cards).toHaveLength(2);
    expect(cards[0]?.getByText("Withdrawn by the agent")).toBeTruthy();
    expect(cards[0]?.getByText("the agent moved on")).toBeTruthy();
    expect(cards[1]?.getByText("Lost: the session ended")).toBeTruthy();
  });
});

describe("a pending card survives a reconnect and a reload (C2)", () => {
  it("stays pending across a closed `unreachable`, the link's return and a new stream, until an answer item arrives", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [
      approvalRequest(1),
      frame(2, { kind: "notice", code: "info", text: "a later item" }),
    ]);
    expect(approvalCard().getByRole("button", { name: "Allow" })).toBeTruthy();

    // The local leg drops: the 3c banner shows, and no stream is open.
    await act(async () => {
      fake.emit({ event: "closed", error: { kind: "unreachable", message: "the bridge closed the stream" } });
    });
    expect(screen.getByText(`Not connected to ${ADDRESS}`)).toBeTruthy();
    expect(fake.open()).toHaveLength(0);
    expect(approvalCard().getByRole("button", { name: "Allow" })).toBeTruthy();

    // The link comes back: a new stream opens after the highest seq the UI holds, and replays.
    await user.click(screen.getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.current()?.options).toEqual({ session: "S-1", after: 2 }));
    await push(fake, frames.hello(fake.sessions, "S-1", 2), frame(3, { kind: "notice", code: "info", text: "after the gap" }));
    expect(screen.queryByText(/Not connected/)).toBeNull();
    expect(main().getByText("after the gap")).toBeTruthy();
    expect(screen.getAllByRole("article", { name: "Approval request" })).toHaveLength(1);
    expect(approvalCard().getByRole("button", { name: "Allow" })).toBeTruthy();

    // The server may send the request again (a reopen cannot double an item): still one pending card.
    await push(fake, approvalRequest(1));
    expect(screen.getAllByRole("article", { name: "Approval request" })).toHaveLength(1);

    await push(fake, answered(4, 1, { kind: "approval_answer", outcome: "answered", reason: null, decision: "allow", message: null }));
    expect(approvalCard().getByText("Allowed")).toBeTruthy();
    expect(approvalCard().queryByRole("button", { name: "Allow" })).toBeNull();
  });

  it("is pending again after a remount that replays the items from 0", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [approvalRequest(1), frame(2, { kind: "notice", code: "info", text: "n" })]);
    expect(approvalCard().getByRole("button", { name: "Allow" })).toBeTruthy();

    // A reload: everything is dropped, and the operator opens the chat again from its row.
    cleanup();
    await bootChat(fake, "S-1", []);
    expect(fake.current()?.options).toEqual({ session: "S-1", after: 0 });
    expect(screen.queryByRole("article", { name: "Approval request" })).toBeNull();

    await push(fake, approvalRequest(1), frame(2, { kind: "notice", code: "info", text: "n" }));
    expect(approvalCard().getByRole("button", { name: "Allow" })).toBeTruthy();
    expect(screen.getAllByRole("article", { name: "Approval request" })).toHaveLength(1);

    await push(fake, answered(3, 1, { kind: "approval_answer", outcome: "lost", reason: "the controller restarted", decision: null, message: null }));
    expect(approvalCard().getByText("Lost: the controller restarted")).toBeTruthy();
  });

  it("derives a card's state from the items: a request answered earlier is not pending", async () => {
    // The request and its answer in either order of arrival give a closed card.
    const fake = seeded({ sessions: [alpha()] });
    await bootChat(fake, "S-1", [
      answered(2, 1, { kind: "approval_answer", outcome: "answered", reason: null, decision: "allow", message: null }),
      approvalRequest(1),
    ]);
    expect(approvalCard().getByText("Allowed")).toBeTruthy();
    expect(approvalCard().queryByRole("button", { name: "Allow" })).toBeNull();
  });
});
