// AC-2 (r2.s11.w5): the acts of a block card. One press makes exactly two calls, `confirmCode` then the act,
// each with a fresh command ID. An option press binds the option's ID and sends the option and nothing of its
// own; a note rides a `resume` option as one typed `answer`; Approve merge on a block sends the `approve`
// option; the five states of 12e; `subject_moved` reads `requests` once and an option of an older revision is
// never sent again. The code is never in the DOM, storage or a URL. Known limit (Q2, C1): the code proves the
// device, the freshness and the binding, not that a person pressed the button.
import { act, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { resetHeld } from "../needs-you/held";
import { boot, type User } from "./chat-helpers";
import { answerOption, approveOption, decisionBlock, resumeOption } from "./decision-fixtures";
import { FakeTransport, claimRecord, mergeAck, requestSummary, runSummary } from "./fake-transport";

const rail = () => within(screen.getByRole("navigation"));
const card = (id: string) => within(screen.getByRole("article", { name: `Request ${id}` }));
const optionButton = (id: string, option: string) => {
  const button = screen.getByRole("article", { name: `Request ${id}` }).querySelector(`[data-option="${option}"]`);
  if (!button) throw new Error(`no option ${option} on ${id}`);
  return button as HTMLButtonElement;
};
const codeN = (n: number) => `pfc1-${n.toString(16).padStart(32, "0")}`;
const sha = async (text: string) =>
  [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)))].map((b) => b.toString(16).padStart(2, "0")).join("");

beforeEach(() => {
  resetHeld();
});

/** R-1 a question with two seat options; R-2 a merge with a block; R-3 a stall; R-4 an outcome cap; R-5 a stop that takes no text; R-6 a merge with no block. */
function seeded(): FakeTransport {
  const fake = new FakeTransport();
  fake.statusServer = { id: "0".repeat(64), name: "draco-desk" };
  fake.seedRequests(
    [
      requestSummary("R-1", { kind: "stop", unit: "u1", stage: "implement" }),
      mergeAck("R-2"),
      requestSummary("R-3", { kind: "stall", unit: "u3", stage: "build", verbs: ["answer", "clear"] }),
      requestSummary("R-4", { kind: "park", unit: "u4", stage: "review", verbs: ["answer", "clear"] }),
      requestSummary("R-5", { kind: "stop", unit: "u5", stage: "implement" }),
      mergeAck("R-6", { opened_at: "2026-11-10T11:30:00.000Z" }),
    ],
    claimRecord({ generation: 3, holder: "ann@host" }),
  );
  fake.runs = [runSummary("R-1", { spine: "s1" })];
  fake.bodies["R-1"] = { stop_text: "Which database?\nOptions:\ns1. Use SQLite\ns2. Use Postgres" };
  fake.decisions["R-1"] = decisionBlock({
    options: [answerOption("s1", "Use SQLite"), answerOption("s2", "Use Postgres")],
    recommended: { option: "s2", by: "seat", why: "The seat marked this option." },
  });
  fake.bodies["R-2"] = { head_branch: "spine/s1", pr: "https://github.com/pulseai-labs/example/pull/9", base: "main", head_sha: "a".repeat(40) };
  fake.decisions["R-2"] = decisionBlock({
    cause: "merge_waits_ack",
    asked_by: { kind: "controller" },
    options: [approveOption()],
    refs: { project: "P-1", run: "R-1", stage: "merge", pr: "pulseai-labs/example#9" },
  });
  fake.decisions["R-3"] = decisionBlock({
    cause: "stalled",
    asked_by: { kind: "controller" },
    options: [resumeOption("c1", "Resume")],
    refs: { run: "R-1", unit: "u3", stage: "build" },
  });
  fake.decisions["R-4"] = decisionBlock({
    cause: "outcome_cap",
    asked_by: { kind: "controller" },
    options: [resumeOption("c1", "Send it to fix again", "fix"), resumeOption("c2", "Go on here", "review")],
    refs: { run: "R-1", unit: "u4", stage: "review" },
  });
  fake.decisions["R-5"] = decisionBlock({
    takes_text: false,
    options: [answerOption("s1", "Use SQLite")],
    refs: { run: "R-1", unit: "u5", stage: "implement" },
  });
  fake.bodies["R-5"] = { stop_text: "Which cache?" };
  fake.bodies["R-6"] = { head_branch: "spine/s2", pr: "https://github.com/pulseai-labs/example/pull/10", base: "main", head_sha: "b".repeat(40) };
  return fake;
}

async function open(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await user.click(rail().getByRole("button", { name: "Needs you" }));
  await screen.findByRole("article", { name: "Request R-6" });
  await waitFor(() => expect(document.querySelectorAll(".ny-opt").length).toBeGreaterThan(4));
  await screen.findAllByRole("button", { name: /Approve merge/ });
  return user;
}

function expectNoCode() {
  expect(document.body.innerHTML).not.toContain("pfc1-");
  for (const store of [window.localStorage, window.sessionStorage]) {
    for (let i = 0; i < store.length; i += 1) {
      const key = store.key(i) as string;
      expect(key).not.toContain("pfc1");
      expect(store.getItem(key) ?? "").not.toContain("pfc1-");
    }
  }
  expect(window.location.href).not.toContain("pfc1");
}

describe("an option press", () => {
  it("makes exactly two calls, binds the option and no text hash or stage, and sends an empty text with the option", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(optionButton("R-1", "s2"));
    await waitFor(() => expect(fake.count("answer")).toBe(1));
    expect(fake.calls.filter((c) => c === "confirmCode" || c === "answer" || c === "clear" || c === "ack")).toEqual(["confirmCode", "answer"]);
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-1", revision: 1, verb: "answer", option: "s2" });
    expect("text_sha256" in (fake.confirmArgs[0]?.subject ?? {})).toBe(false);
    expect("to" in (fake.confirmArgs[0]?.subject ?? {})).toBe(false);
    const spent = fake.actArgs[0];
    expect(spent?.verb).toBe("answer");
    expect(spent?.args).toMatchObject({ request: "R-1", rev: 1, text: "", option: "s2", code: codeN(1) });
    expect("to" in (spent?.args ?? {})).toBe(false);
    expect(spent?.args.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expectNoCode();
  });

  it("sends a `resume` option as `clear` with the option and no stage", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(optionButton("R-4", "c1"));
    await waitFor(() => expect(fake.count("clear")).toBe(1));
    expect(fake.calls.filter((c) => c === "confirmCode" || c === "clear")).toEqual(["confirmCode", "clear"]);
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-4", revision: 1, verb: "clear", option: "c1" });
    expect(fake.actArgs[0]?.args).toMatchObject({ request: "R-4", rev: 1, option: "c1", code: codeN(1) });
    expect("to" in (fake.actArgs[0]?.args ?? {})).toBe(false);
    expect("text" in (fake.actArgs[0]?.args ?? {})).toBe(false);
  });

  it("sends Approve merge on a block with `approve` as `ack` with the option, and on a request with no block as `ack` with none", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: /Approve merge/ }));
    await waitFor(() => expect(fake.count("ack")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-2", revision: 1, verb: "ack", option: "approve" });
    expect(fake.actArgs[0]?.args).toMatchObject({ request: "R-2", rev: 1, option: "approve", code: codeN(1) });
    await user.click(card("R-6").getByRole("button", { name: /Approve merge/ }));
    await waitFor(() => expect(fake.count("ack")).toBe(2));
    expect(fake.confirmArgs[1]?.subject).toEqual({ kind: "request", request: "R-6", revision: 1, verb: "ack" });
    expect("option" in (fake.actArgs[1]?.args ?? {})).toBe(false);
  });
});

describe("the note and the typed answer", () => {
  it("sends a note with a `resume` option as one typed `answer`: the note, the option's stage as `to`, a code that binds both, no option", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.type(card("R-4").getByLabelText("Note for the next seat"), "restarted the database");
    await user.click(optionButton("R-4", "c1"));
    await waitFor(() => expect(fake.count("answer")).toBe(1));
    expect(fake.calls.filter((c) => c === "confirmCode" || c === "answer" || c === "clear")).toEqual(["confirmCode", "answer"]);
    expect(fake.confirmArgs[0]?.subject).toEqual({
      kind: "request",
      request: "R-4",
      revision: 1,
      verb: "answer",
      text_sha256: await sha("restarted the database"),
      to: "fix",
    });
    expect(fake.actArgs[0]?.args).toMatchObject({ request: "R-4", rev: 1, text: "restarted the database", to: "fix", code: codeN(1) });
    expect("option" in (fake.actArgs[0]?.args ?? {})).toBe(false);
  });

  it("sends a note with a `resume` option that names no stage with no `to`", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.type(card("R-3").getByLabelText("Note for the next seat"), "with care");
    await user.click(optionButton("R-3", "c1"));
    await waitFor(() => expect(fake.count("answer")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-3", revision: 1, verb: "answer", text_sha256: await sha("with care") });
    expect("to" in (fake.actArgs[0]?.args ?? {})).toBe(false);
  });

  it("sends the option's own act when the note is empty", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(optionButton("R-3", "c1"));
    await waitFor(() => expect(fake.count("clear")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-3", revision: 1, verb: "clear", option: "c1" });
  });

  it("sends Send as today's typed answer: a code that binds the text's SHA-256", async () => {
    const fake = seeded();
    const user = await open(fake);
    const send = () => card("R-1").getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send().disabled).toBe(true);
    await user.type(card("R-1").getByLabelText("Another answer"), "abc");
    await user.click(send());
    await waitFor(() => expect(fake.count("answer")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toEqual({
      kind: "request",
      request: "R-1",
      revision: 1,
      verb: "answer",
      text_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    });
    expect(fake.actArgs[0]?.args).toMatchObject({ text: "abc", code: codeN(1) });
    expect("option" in (fake.actArgs[0]?.args ?? {})).toBe(false);
  });

  it("sends a stop that takes no text one Resume: `clear` with no option and no stage", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(optionButton("R-5", "resume"));
    await waitFor(() => expect(fake.count("clear")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-5", revision: 1, verb: "clear" });
    expect(Object.keys(fake.actArgs[0]?.args ?? {}).sort()).toEqual(["code", "command_id", "request", "rev"]);
  });
});

describe("the five states", () => {
  it("shows the working text on the pressed control and waits every other control of the card; a second press starts nothing", async () => {
    const fake = seeded();
    const user = await open(fake);
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const original = fake.answer.bind(fake);
    fake.answer = async (args) => {
      await gate;
      return original(args);
    };
    await user.click(optionButton("R-1", "s2"));
    await waitFor(() => expect(optionButton("R-1", "s2").textContent).toContain("Sending to draco-desk…"));
    expect(optionButton("R-1", "s1").disabled).toBe(true);
    expect(optionButton("R-1", "s2").disabled).toBe(true);
    expect((card("R-1").getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(optionButton("R-1", "s1"));
    await user.click(optionButton("R-1", "s2"));
    expect(fake.count("confirmCode")).toBe(1);
    release();
    await waitFor(() => expect(fake.count("answer")).toBe(1));
  });

  it("shows `Approving…` on Approve merge while the press is in flight", async () => {
    const fake = seeded();
    const user = await open(fake);
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const original = fake.ack.bind(fake);
    fake.ack = async (args) => {
      await gate;
      return original(args);
    };
    await user.click(card("R-2").getByRole("button", { name: /Approve merge/ }));
    await waitFor(() => expect(card("R-2").getByRole("button", { name: /Approving/ })).toBeTruthy());
    expect((card("R-2").getByRole("button", { name: "Hold" }) as HTMLButtonElement).disabled).toBe(true);
    release();
    await waitFor(() => expect(fake.count("ack")).toBe(1));
  });

  it("turns the card into its answered row after a landed act", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(optionButton("R-1", "s2"));
    const answered = within(await screen.findByRole("list", { name: "Answered today" }));
    expect(await answered.findByText("Pressed Use Postgres")).toBeTruthy();
    expect(screen.queryByRole("article", { name: "Request R-1" })).toBeNull();
    expect(answered.getByText("MacBook · " + new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" }))).toBeTruthy();
  });

  it("names a resume and a note in the answered row", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.type(card("R-4").getByLabelText("Note for the next seat"), "restarted it");
    await user.click(optionButton("R-4", "c1"));
    const answered = within(await screen.findByRole("list", { name: "Answered today" }));
    expect(await answered.findByText("Resumed u4 at fix · note: restarted it")).toBeTruthy();
    await user.click(optionButton("R-3", "c1"));
    expect(await answered.findByText("Resumed u3 at build")).toBeTruthy();
  });

  it("shows `<server> refused it.` and the refusal line, and Try again asks a new code", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("answer", "refused", "the code expired", 1, "code_expired");
    await user.click(optionButton("R-1", "s2"));
    const line = await waitFor(() => {
      const el = card("R-1").getByRole("alert");
      expect(el.textContent).toContain("draco-desk refused it.");
      return el;
    });
    expect(line.textContent).toContain("The confirm code expired. Confirm again.");
    await user.click(within(line).getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(fake.count("confirmCode")).toBe(2));
    await waitFor(() => expect(fake.actArgs).toHaveLength(2));
    expect(fake.actArgs[1]?.args).toMatchObject({ option: "s2", code: codeN(2) });
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expect(fake.confirmArgs[1]?.subject).toEqual(fake.confirmArgs[0]?.subject);
  });

  it("shows the not-connected line after `unreachable` on the ask, and Retry now asks again with a new command ID", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "unreachable", "no route to host", 1);
    await user.click(optionButton("R-1", "s2"));
    const line = await waitFor(() => {
      const el = card("R-1").getByText("Not connected to draco-desk. Nothing was sent. You can press again once it’s back.");
      return el.closest("[role=alert]") as HTMLElement;
    });
    expect(fake.count("answer")).toBe(0);
    await user.click(within(line).getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.count("answer")).toBe(1));
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
  });

  it("shows the answer-may-not-have-arrived line after `unreachable` on the spend, and Retry now spends the same code with the same command ID", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("answer", "unreachable", "no route to host", 1);
    await user.click(optionButton("R-1", "s2"));
    const line = await waitFor(() => {
      const el = card("R-1").getByText("Not connected to draco-desk. The answer may not have arrived. Retry sends the same one again.");
      return el.closest("[role=alert]") as HTMLElement;
    });
    await user.click(within(line).getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.actArgs).toHaveLength(2));
    expect(fake.confirmArgs).toHaveLength(1);
    expect(fake.actArgs[1]?.args.code).toBe(fake.actArgs[0]?.args.code);
    expect(fake.actArgs[1]?.args.command_id).toBe(fake.actArgs[0]?.args.command_id);
  });
});

describe("a request that moved", () => {
  it("reads `requests` once after `subject_moved`, shows the new revision's options and sends the new revision on the next press", async () => {
    const fake = seeded();
    const user = await open(fake);
    const reads = fake.count("requests");
    fake.fail("clear", "refused", "the request changed", 1, "subject_moved");
    await user.click(optionButton("R-4", "c1"));
    await waitFor(() => expect(card("R-4").getByRole("alert").textContent).toContain("The request changed since the code was issued."));
    // The server moved the request to revision 2 and replaced its options.
    const at = fake.requestRows.findIndex((r) => r.id === "R-4");
    fake.requestRows[at] = { ...(fake.requestRows[at] as (typeof fake.requestRows)[number]), revision: 2 };
    fake.decisions["R-4"] = decisionBlock({
      cause: "outcome_cap",
      asked_by: { kind: "controller" },
      options: [resumeOption("c9", "Send it to fix once more", "fix")],
      refs: { run: "R-1", unit: "u4", stage: "review" },
    });
    // The read already happened on the refusal; it brought the old revision. One more read is the page's own.
    expect(fake.count("requests")).toBe(reads + 1);
    await act(async () => {
      const list = await fake.requests({ all: true });
      fake.requestFrame({ type: "requests", requests: list.requests, claim: list.claim });
    });
    await waitFor(() => expect(optionButton("R-4", "c9")).toBeTruthy());
    expect(card("R-4").queryByRole("button", { name: /Try again/ })).toBeNull();
    expect(screen.getByRole("article", { name: "Request R-4" }).querySelector('[data-option="c1"]')).toBeNull();
    await user.click(optionButton("R-4", "c9"));
    await waitFor(() => expect(fake.actArgs.filter((a) => a.verb === "clear")).toHaveLength(2));
    expect(fake.confirmArgs.at(-1)?.subject).toEqual({ kind: "request", request: "R-4", revision: 2, verb: "clear", option: "c9" });
    expect(fake.actArgs.at(-1)?.args).toMatchObject({ rev: 2, option: "c9" });
  });

  it("does not show Try again after `subject_moved`, and never sends the old option again", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("clear", "refused", "the request changed", 1, "subject_moved");
    await user.click(optionButton("R-4", "c1"));
    await waitFor(() => expect(card("R-4").getByRole("alert")).toBeTruthy());
    expect(card("R-4").queryByRole("button", { name: "Try again" })).toBeNull();
    expect(fake.actArgs.filter((a) => a.verb === "clear")).toHaveLength(1);
  });
});

describe("a replay and the code", () => {
  it("claims no outcome after a replay: it reads `requests` and shows no answered words of its own", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.replayOnce("answer");
    const reads = fake.count("requests");
    await user.click(optionButton("R-1", "s2"));
    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
    expect(screen.queryByText("Pressed Use Postgres")).toBeNull();
    expect(card("R-1").queryByText(/Pressed/)).toBeNull();
  });

  it("never has the code in the DOM, storage or a call's URL, in any state", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("answer", "refused", "the code expired", 1, "code_expired");
    await user.click(optionButton("R-1", "s2"));
    await waitFor(() => expect(card("R-1").getByRole("alert")).toBeTruthy());
    expectNoCode();
    await user.click(card("R-1").getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(fake.count("answer")).toBe(2));
    expectNoCode();
  });

  it("makes no call without a press", async () => {
    const fake = seeded();
    await open(fake);
    expect(fake.count("confirmCode")).toBe(0);
    expect(fake.count("answer") + fake.count("clear") + fake.count("ack")).toBe(0);
  });
});
