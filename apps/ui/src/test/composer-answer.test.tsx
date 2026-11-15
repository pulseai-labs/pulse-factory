// r1.s6.w2 (ruling P4): a typed answer to an open question goes in the composer, as 11a's card says ("Or type a
// different answer below"). While the chat holds exactly one open single-question card, the composer's Send
// (and Enter) answers that question with `sessionAnswer`; in every other case Send is `sessionSend`.
import type { ChatItemBody, QuestionRecord } from "@pulse-factory/api-client";
import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { bootChat, push, seeded } from "./chat-helpers";
import { frames, item } from "./fake-transport";

const QUESTION = "Which option do you prefer?";
const COMMAND_ID = /^[0-9a-f-]{16,}$/;

const question = (over: Partial<QuestionRecord> = {}): QuestionRecord => ({
  text: QUESTION,
  header: "Option",
  options: [
    { label: "A", description: "the first" },
    { label: "B", description: "the second" },
  ],
  multi_select: false,
  ...over,
});

const frame = (seq: number, body: ChatItemBody, patch = {}) => frames.item(item("S-1", seq, body, patch));
const request = (questions: QuestionRecord[], seq = 3) => frame(seq, { kind: "question_request", request_id: "Q-31", items: questions });

const box = () => screen.getByRole("textbox", { name: "Message Orchestrator" }) as HTMLTextAreaElement;
const send = () => screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
const composer = () => within(document.querySelector(".composer") as HTMLElement);
const card = () => within(screen.getByRole("article", { name: "Question" }));

describe("one open single-question card: the composer answers it", () => {
  it("Send calls sessionAnswer once with the card's seq and the typed text, and never sessionSend", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [request([question()], 3)]);
    await user.type(box(), "something else");
    await user.click(send());

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]).toEqual({
      session_id: "S-1",
      seq: 3,
      answer: { type: "answers", value: { [QUESTION]: "something else" } },
      command_id: expect.stringMatching(COMMAND_ID),
    });
    expect(fake.count("sessionSend")).toBe(0);
    // A chat answer is not a request act: no confirm code is asked for.
    expect(fake.count("confirmCode")).toBe(0);
  });

  it("Enter answers it too, and the draft clears once the answer went through", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [request([question()])]);
    await user.type(box(), "typed answer{Enter}");

    await waitFor(() => expect(fake.answerArgs).toHaveLength(1));
    expect(fake.answerArgs[0]?.answer).toEqual({ type: "answers", value: { [QUESTION]: "typed answer" } });
    expect(fake.count("sessionSend")).toBe(0);
    await waitFor(() => expect(box().value).toBe(""));
  });

  it("keeps the placeholder `Message <agent>` and says in Send's title that it answers the question", async () => {
    await bootChat(seeded(), "S-1", [request([question()])]);
    expect(box().getAttribute("placeholder")).toBe("Message Orchestrator");
    expect(send().getAttribute("title")).toBe("Answer the open question");
  });

  it("shows a refusal on the composer and keeps the draft", async () => {
    const fake = seeded();
    fake.fail("sessionAnswer", "refused", "request 3 of S-1 is already answered, closed or lost", 1);
    const user = await bootChat(fake, "S-1", [request([question()], 3)]);
    await user.type(box(), "late answer");
    await user.click(send());

    expect(await composer().findByText("request 3 of S-1 is already answered, closed or lost")).toBeTruthy();
    expect(box().value).toBe("late answer");
    expect(fake.count("sessionSend")).toBe(0);
  });

  it("after `unreachable` the same text on the next press reuses the command ID", async () => {
    const fake = seeded();
    fake.fail("sessionAnswer", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake, "S-1", [request([question()])]);
    await user.type(box(), "same text");
    await user.click(send());
    expect(await composer().findByText("the server did not answer")).toBeTruthy();
    expect(box().value).toBe("same text");

    await user.click(send());
    await waitFor(() => expect(fake.answerArgs).toHaveLength(2));
    expect(fake.answerArgs[1]?.command_id).toBe(fake.answerArgs[0]?.command_id);
    expect(fake.answerArgs[1]?.answer).toEqual(fake.answerArgs[0]?.answer);
  });

  it("the card shows 11a's sentence and has no text field and no Send of its own", async () => {
    await bootChat(seeded(), "S-1", [request([question()])]);
    expect(card().getByText("Or type a different answer below. You can also answer from your phone or the CLI.")).toBeTruthy();
    expect(card().queryByRole("textbox")).toBeNull();
    expect(card().queryByRole("button", { name: "Send answer" })).toBeNull();
    // One press on an option is still one answer.
    expect(card().getAllByRole("button").filter((b) => /^\d/.test(b.textContent ?? ""))).toHaveLength(2);
  });

  it("goes back to sessionSend once the card is answered", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [request([question()], 3)]);
    await push(fake, frame(4, { kind: "question_answer", outcome: "answered", reason: null, answers: { [QUESTION]: "A" } }, { answers_seq: 3 }));
    await user.type(box(), "now a message");
    expect(send().getAttribute("title")).toBe("Send");
    await user.click(send());

    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.sendArgs[0]?.text).toBe("now a message");
    expect(fake.count("sessionAnswer")).toBe(0);
  });
});

describe("every other case keeps today's behaviour", () => {
  it("with no open card Send calls sessionSend", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", []);
    await user.type(box(), "hello");
    await user.click(send());

    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.sendArgs[0]?.text).toBe("hello");
    expect(fake.count("sessionAnswer")).toBe(0);
  });

  it("an open multi-select question keeps its toggles, its Other answer field and its Send on the card", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [request([question({ multi_select: true })])]);
    expect(card().getByRole("textbox", { name: "Other answer" })).toBeTruthy();
    expect(card().getByRole("button", { name: "Send answer" })).toBeTruthy();
    await user.type(box(), "a message");
    expect(send().getAttribute("title")).toBe("Send");
    await user.click(send());

    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.count("sessionAnswer")).toBe(0);
  });

  it("an open request with more than one question keeps its own fields and its one Send", async () => {
    const fake = seeded();
    const second = question({ text: "Which colour?", header: "Colour", options: [{ label: "red", description: "" }] });
    const user = await bootChat(fake, "S-1", [request([question(), second])]);
    expect(card().getAllByRole("textbox", { name: "Other answer" })).toHaveLength(2);
    expect(card().getByRole("button", { name: "Send answer" })).toBeTruthy();
    await user.type(box(), "a message{Enter}");

    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.count("sessionAnswer")).toBe(0);
  });

  it("two open single-question cards are ambiguous: Send is sessionSend", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [
      request([question()], 3),
      request([question({ text: "Another question?" })], 4),
    ]);
    await user.type(box(), "a message{Enter}");

    await waitFor(() => expect(fake.sendArgs).toHaveLength(1));
    expect(fake.count("sessionAnswer")).toBe(0);
  });
});
