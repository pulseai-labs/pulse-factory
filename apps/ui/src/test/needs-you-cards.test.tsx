// AC-1 (r1.s5.w4): the Needs you cards and their one-press acts (11d). One press is one `confirmCode` and
// one act (`ack`, `answer` or `clear`) with that code; the confirm panel is gone (G1). The `client-api-auth`
// control stays: the code is fresh, one-use and bound to this act, and it lives in memory for that press.
//
// Known limit (Q2, C1): the code proves the device, the freshness and the head, not that a person pressed
// the button. These tests check the form of the control, and claim no more.
import type { RequestSummary } from "@pulse-factory/api-client";
import { act, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { resetHeld } from "../needs-you/held";
import { boot, push, type User } from "./chat-helpers";
import {
  FakeTransport,
  chatSession,
  claimRecord,
  frames,
  mergeAck,
  requestFrames,
  requestSummary,
  runSummary,
} from "./fake-transport";

const rail = () => within(screen.getByRole("navigation"));
const main = () => within(screen.getByRole("main"));
const card = (id: string) => within(screen.getByRole("article", { name: `Request ${id}` }));
const openList = () => within(main().getByRole("list", { name: "Open requests" }));
const answeredList = () => within(main().getByRole("list", { name: "Answered today" }));

/** The code the fake issues as its n-th: `pfc1-` and 32 hex digits. */
const codeN = (n: number) => `pfc1-${n.toString(16).padStart(32, "0")}`;

const HEAD = "a".repeat(40);
const ago = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();

beforeEach(() => {
  resetHeld();
});

/** A stop request R-1 (text "Which database?"), a merge approval R-2 and a parked item R-3, under ann's claim. */
function seeded(claim: FakeTransport["claim"] = claimRecord({ generation: 3, holder: "ann@host" })): FakeTransport {
  const fake = new FakeTransport();
  fake.seedRequests([requestSummary("R-1"), mergeAck("R-2"), parked("R-3")], claim);
  fake.bodies["R-1"] = { stop_text: "Which database?" };
  fake.bodies["R-2"] = { head_branch: "spine/s1", pr: "https://github.com/pulseai-labs/example/pull/9", base: "main", head_sha: HEAD };
  fake.runs = [runSummary("R-1", { spine: "s1" })];
  return fake;
}

function parked(id: string, patch: Partial<RequestSummary> = {}): RequestSummary {
  return requestSummary(id, { kind: "park", unit: "w4", reason: "its tests cannot reach the database", ...patch });
}

async function open(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await user.click(rail().getByRole("button", { name: "Needs you" }));
  await screen.findByRole("article", { name: "Request R-2" });
  await screen.findByText("Which database?");
  return user;
}

/** No trace of any confirm code in the document or the storage. */
function expectNoCode() {
  expect(document.body.innerHTML).not.toContain("pfc1-");
  for (const store of [window.localStorage, window.sessionStorage]) {
    for (let i = 0; i < store.length; i += 1) {
      const key = store.key(i) as string;
      expect(key).not.toContain("pfc1");
      expect(store.getItem(key) ?? "").not.toContain("pfc1-");
    }
  }
}

async function typeAnswer(user: User, text: string) {
  await user.type(card("R-1").getByLabelText("Your answer"), text);
}

describe("the one press", () => {
  it("asks for one code and spends it at once on Approve merge: ack, with two distinct command IDs", async () => {
    const fake = seeded();
    const user = await open(fake);
    expect(fake.count("confirmCode")).toBe(0);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));

    await waitFor(() => expect(fake.count("ack")).toBe(1));
    expect(fake.calls.filter((c) => c === "confirmCode" || c === "ack")).toEqual(["confirmCode", "ack"]);
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-2", revision: 1, verb: "ack" });
    const spent = fake.actArgs[0];
    expect(spent?.verb).toBe("ack");
    expect(spent?.args).toMatchObject({ request: "R-2", rev: 1, code: codeN(1) });
    expect(spent?.args.command_id).toMatch(/\S+/);
    expect(spent?.args.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    // No dialog ever shows (G1).
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("sends the typed answer with the SHA-256 of its text (a fixed vector) in one press", async () => {
    const fake = seeded();
    const user = await open(fake);
    const send = () => card("R-1").getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send().disabled).toBe(true);
    await typeAnswer(user, "abc");
    expect(send().disabled).toBe(false);
    await user.click(send());

    await waitFor(() => expect(fake.count("answer")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toEqual({
      kind: "request",
      request: "R-1",
      revision: 1,
      verb: "answer",
      text_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    });
    expect(fake.actArgs[0]?.args).toMatchObject({ request: "R-1", rev: 1, text: "abc", code: codeN(1) });
    expect(fake.count("confirmCode")).toBe(1);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("hashes the UTF-8 bytes of the text", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "é ✓");
    await user.click(card("R-1").getByRole("button", { name: "Send" }));
    await waitFor(() => expect(fake.count("confirmCode")).toBe(1));
    expect(fake.confirmArgs[0]?.subject).toMatchObject({
      text_sha256: "086a17ec1cb4f336b148c38dfa8e349285c17c994b282b79631088d801c42005",
    });
  });

  it("resumes a parked item with `clear`, with no `to`, in one press", async () => {
    const fake = seeded();
    const user = await open(fake);
    const parkedCard = card("R-3");
    expect(parkedCard.getByText("Parked")).toBeTruthy();
    expect(parkedCard.getByText("w4 is parked: its tests cannot reach the database")).toBeTruthy();
    await user.click(parkedCard.getByRole("button", { name: "Resume w4" }));

    await waitFor(() => expect(fake.count("clear")).toBe(1));
    expect(fake.calls.filter((c) => c === "confirmCode" || c === "clear")).toEqual(["confirmCode", "clear"]);
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-3", revision: 1, verb: "clear" });
    expect(fake.actArgs[0]?.verb).toBe("clear");
    expect(fake.actArgs[0]?.args).toMatchObject({ request: "R-3", rev: 1, code: codeN(1) });
    expect("to" in (fake.actArgs[0]?.args ?? {})).toBe(false);
  });

  it("makes no call without a press: a frame or a reload (the link coming back) asks for no code", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.requestRows.push(requestSummary("R-5"));
    await act(async () => fake.requestFrame(requestFrames.request(requestSummary("R-5"))));
    await screen.findByRole("article", { name: "Request R-5" });
    await waitFor(() => expect(fake.showArgs.map((a) => a.id)).toContain("R-5"));
    // r1.s6.w3 (W1-33): the header has no Refresh to press. The page reads again when the link comes back.
    const reads = fake.count("requests");
    await act(async () => {
      fake.emit({ event: "closed", error: { kind: "unreachable", message: "the bridge closed the stream" } });
    });
    await user.click(await screen.findByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
    expect(fake.count("confirmCode")).toBe(0);
    expect(fake.count("answer") + fake.count("ack") + fake.count("clear")).toBe(0);
  });

  it("starts nothing on a second press while the first is in flight", async () => {
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
    const approve = card("R-2").getByRole("button", { name: "Approve merge" });
    await user.click(approve);
    await waitFor(() => expect((approve as HTMLButtonElement).disabled).toBe(true));
    await user.click(approve);
    await user.click(card("R-2").getByRole("button", { name: "Hold" }));
    expect(fake.count("confirmCode")).toBe(1);
    await act(async () => release());
    await waitFor(() => expect(fake.count("ack")).toBe(1));
  });

  it("moves the card to Answered today with the answer and this device's name", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "sqlite");
    await user.click(card("R-1").getByRole("button", { name: "Send" }));

    await waitFor(() => expect(screen.queryByRole("article", { name: "Request R-1" })).toBeNull());
    const row = within(await answeredList().findByRole("listitem", { name: "Answered R-1" }));
    expect(row.getByText("sqlite")).toBeTruthy();
    expect(row.getByText(/MacBook/)).toBeTruthy();
    // The act's answer is the page's own: no `show` for the row it just answered.
    expect(fake.showArgs.filter((a) => a.id === "R-1")).toHaveLength(1);
  });

  it("shows 'Merge approved' with the short SHA and this device after an ack", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    const row = within(await answeredList().findByRole("listitem", { name: "Answered R-2" }));
    expect(row.getByText("Merge approved")).toBeTruthy();
    expect(row.getByText(/MacBook/)).toBeTruthy();
    expect(screen.getByText(`Merged at ${HEAD.slice(0, 7)}`)).toBeTruthy();
  });

  it("shows 'Resumed at <stage>' after a clear", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-3").getByRole("button", { name: "Resume w4" }));
    const row = within(await answeredList().findByRole("listitem", { name: "Answered R-3" }));
    expect(row.getByText("Resumed at implement")).toBeTruthy();
  });
});

describe("a refusal", () => {
  const WORDED: Array<[string, string]> = [
    ["code_expired", "The confirm code expired. Confirm again."],
    ["code_used", "This confirm code cannot be used (code_used). Confirm again."],
    ["code_voided", "This confirm code cannot be used (code_voided). Confirm again."],
    ["code_unknown", "This confirm code cannot be used (code_unknown). Confirm again."],
    ["code_foreign", "This confirm code cannot be used (code_foreign). Confirm again."],
    ["code_subject", "This confirm code cannot be used (code_subject). Confirm again."],
  ];

  it.each(WORDED)("%s gives its own line on the card, drops the code and a new press asks again", async (word, line) => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "the server's text, which the line does not use", 1, word);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));

    expect(await card("R-2").findByText(line)).toBeTruthy();
    expectNoCode();
    expect(screen.getByRole("article", { name: "Request R-2" })).toBeTruthy();

    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    await waitFor(() => expect(fake.count("ack")).toBe(2));
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expect(fake.actArgs[1]?.args).toMatchObject({ code: codeN(2) });
    expect(fake.actArgs[1]?.args.command_id).not.toBe(fake.actArgs[0]?.args.command_id);
  });

  it("subject_moved says the request changed and reads the requests once", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "the server's text", 1, "subject_moved");
    const reads = fake.count("requests");
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));

    expect(await card("R-2").findByText("The request changed since the code was issued. Read it again and confirm.")).toBeTruthy();
    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
  });

  it("claim_moved shows the server's text, which names the new holder, and Confirm again", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "The claim moved to bob@host.", 1, "claim_moved");
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    expect(await card("R-2").findByText("The claim moved to bob@host. Confirm again.")).toBeTruthy();
  });

  it("an engine refusal with no word shows the server's text", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("answer", "refused", "the checks are red", 1);
    await typeAnswer(user, "go");
    await user.click(card("R-1").getByRole("button", { name: "Send" }));
    expect(await card("R-1").findByText("the checks are red")).toBeTruthy();
  });

  it("a refusal on the ask shows the server's text and sends no act", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "refused", "the request is not open", 1);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    expect(await card("R-2").findByText("the request is not open")).toBeTruthy();
    expect(fake.count("ack")).toBe(0);
  });

  it("an unauthorized ask shows the 3c banner and no line on the card", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "unauthorized", "revoked");
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    expect(await screen.findByText(/This device is no longer paired with/)).toBeTruthy();
    expect(fake.count("ack")).toBe(0);
  });
});

describe("an act that never arrived", () => {
  it("unreachable on the spend keeps the code and the command ID: Retry spends the same code", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "unreachable", "no answer", 1);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));

    const retry = await card("R-2").findByRole("button", { name: "Retry" });
    expect(card("R-2").getByText("no answer")).toBeTruthy();
    expect(fake.actArgs).toHaveLength(1);
    expectNoCode();
    await user.click(retry);

    await waitFor(() => expect(fake.actArgs).toHaveLength(2));
    expect(fake.actArgs[1]?.args.code).toBe(fake.actArgs[0]?.args.code);
    expect(fake.actArgs[1]?.args.command_id).toBe(fake.actArgs[0]?.args.command_id);
    expect(fake.count("confirmCode")).toBe(1);
  });

  it("unreachable on the ask: Retry asks again with a new command ID", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "unreachable", "no answer", 1);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));

    const retry = await card("R-2").findByRole("button", { name: "Retry" });
    expect(fake.confirmArgs).toHaveLength(1);
    expect(fake.count("ack")).toBe(0);
    await user.click(retry);
    await waitFor(() => expect(fake.count("ack")).toBe(1));
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
  });

  it("a replayed answer reads the requests once and claims no outcome while the request is open", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "sqlite");
    const reads = fake.count("requests");
    fake.replayOnce("answer");
    await user.click(card("R-1").getByRole("button", { name: "Send" }));

    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
    expect(screen.getByRole("article", { name: "Request R-1" })).toBeTruthy();
    expect(screen.queryByText(/Your answer sqlite/)).toBeNull();
    expect(main().queryByRole("listitem", { name: "Answered R-1" })).toBeNull();
  });

  it("a replayed ack reads the requests once and claims no merge", async () => {
    const fake = seeded();
    const user = await open(fake);
    const reads = fake.count("requests");
    fake.replayOnce("ack");
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
    expect(screen.queryByText(/Merge approved/)).toBeNull();
    expect(screen.queryByText(/Merged at/)).toBeNull();
  });
});

describe("a frame that closes the request while its spend is in flight", () => {
  it("applies the act's outcome when it answers, though the card left", async () => {
    const fake = seeded();
    const user = await open(fake);
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const original = fake.ack.bind(fake);
    let sent = false;
    fake.ack = async (args) => {
      sent = true;
      await gate;
      return original(args);
    };
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    await waitFor(() => expect(sent).toBe(true));
    await act(async () =>
      fake.requestFrame(requestFrames.request({ ...mergeAck("R-2"), status: "answered", answered_at: new Date().toISOString(), verbs: [] })),
    );
    expect(screen.queryByRole("article", { name: "Request R-2" })).toBeNull();

    await act(async () => release());
    const row = within(await answeredList().findByRole("listitem", { name: "Answered R-2" }));
    expect(row.getByText("Merge approved")).toBeTruthy();
  });

  it("applies the act's answer to the shared state even when the operator left the page", async () => {
    const fake = seeded();
    const user = await open(fake);
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const original = fake.ack.bind(fake);
    let sent = false;
    fake.ack = async (args) => {
      sent = true;
      await gate;
      return original(args);
    };
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    await waitFor(() => expect(sent).toBe(true));
    await user.click(rail().getByRole("button", { name: "Projects" }));

    const badge = () => rail().getByRole("button", { name: "Needs you" }).querySelector(".ui-count");
    expect(badge()?.textContent).toBe("3");
    await act(async () => release());
    await waitFor(() => expect(badge()?.textContent).toBe("2"));
  });
});

describe("the code stays in memory", () => {
  it("is in no DOM text or storage at any step: asked, refused, retried and spent", async () => {
    const fake = seeded();
    const user = await open(fake);
    expectNoCode();
    fake.fail("ack", "unreachable", "no answer", 1);
    await user.click(card("R-2").getByRole("button", { name: "Approve merge" }));
    await card("R-2").findByRole("button", { name: "Retry" });
    expectNoCode();
    await user.click(card("R-2").getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(fake.actArgs).toHaveLength(2));
    expectNoCode();
    expect(fake.actArgs[0]?.args.code).toMatch(/^pfc1-[0-9a-f]{32}$/);
  });
});

describe("Hold (R5: a client-only close)", () => {
  it("sets the merge card aside with no call, leaves a quiet row, and Show puts it back", async () => {
    const fake = seeded();
    const user = await open(fake);
    const before = fake.calls.length;
    await user.click(card("R-2").getByRole("button", { name: "Hold" }));

    expect(screen.queryByRole("article", { name: "Request R-2" })).toBeNull();
    const row = within(screen.getByTestId("held-row"));
    expect(row.getByText(/Held on this Mac/)).toBeTruthy();
    expect(row.getByText(/Merge spine\/s1 into main\?/)).toBeTruthy();
    // The request stays open on the server and in the count.
    expect(main().getByTestId("open-count").textContent).toBe("3");
    expect(rail().getByRole("button", { name: "Needs you" }).querySelector(".ui-count")?.textContent).toBe("3");

    await user.click(row.getByRole("button", { name: "Show" }));
    expect(screen.getByRole("article", { name: "Request R-2" })).toBeTruthy();
    expect(screen.queryByTestId("held-row")).toBeNull();
    expect(fake.calls.length).toBe(before);
  });

  it("shows the card again by itself when the request's revision changes, and not for the same revision", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Hold" }));
    await act(async () => fake.requestFrame(requestFrames.request(mergeAck("R-2"))));
    expect(screen.queryByRole("article", { name: "Request R-2" })).toBeNull();

    fake.requestRows = fake.requestRows.map((r) => (r.id === "R-2" ? { ...r, revision: 2, head_sha: "b".repeat(40) } : r));
    fake.bodies["R-2"] = { head_branch: "spine/s1", head_sha: "b".repeat(40) };
    await act(async () => fake.requestFrame(requestFrames.request({ ...mergeAck("R-2"), revision: 2, head_sha: "b".repeat(40) })));
    expect(await screen.findByRole("article", { name: "Request R-2" })).toBeTruthy();
  });

  it("is on a merge card only", async () => {
    const fake = seeded();
    await open(fake);
    expect(card("R-1").queryByRole("button", { name: "Hold" })).toBeNull();
    expect(card("R-3").queryByRole("button", { name: "Hold" })).toBeNull();
    expect(card("R-2").getByRole("button", { name: "Hold" })).toBeTruthy();
  });

  it("keeps the held set in memory only: a new page load shows the card", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Hold" }));
    expect(Object.keys(window.localStorage)).not.toContain("pf.held");
    for (let i = 0; i < window.localStorage.length; i += 1) {
      expect(window.localStorage.getItem(window.localStorage.key(i) as string)).not.toContain("R-2");
    }
    expect(window.sessionStorage.length).toBe(0);
  });
});

describe("the cards", () => {
  it("shows the merge card: eyebrow, spine, PR pill, question, From and Into, the short SHA with the full one as its title, Holding up", async () => {
    const fake = seeded();
    fake.seedRequests([mergeAck("R-2", { blocks: ["S-2 store schema", "S-3 agents"] })]);
    await open2(fake);
    const merge = card("R-2");
    expect(merge.getByText("Spine merge")).toBeTruthy();
    expect(merge.getByText("· s1")).toBeTruthy();
    expect(merge.getByTestId("merge-pr").textContent).toBe("PR #9");
    expect(merge.getByText("Merge spine/s1 into main?")).toBeTruthy();
    expect(merge.getByTestId("merge-from").textContent).toBe("spine/s1");
    expect(merge.getByTestId("merge-into").textContent).toBe("main");
    const sha = merge.getByTestId("merge-sha");
    expect(sha.textContent).toBe(HEAD.slice(0, 7));
    expect(sha.getAttribute("title")).toBe(HEAD);
    expect(merge.getByText("Holding up")).toBeTruthy();
    expect(merge.getByText("S-2 store schema, S-3 agents")).toBeTruthy();
  });

  it("falls back to the summary's PR text when it does not end in /pull/<n>", async () => {
    const fake = seeded();
    fake.bodies["R-2"] = { head_branch: "spine/s1" };
    await open(fake);
    expect(card("R-2").getByTestId("merge-pr").textContent).toBe("pulseai-labs/example#9");
  });

  it("shows no Holding up row when blocks is empty", async () => {
    const fake = seeded();
    await open(fake);
    expect(card("R-2").queryByText("Holding up")).toBeNull();
  });

  it("shows a run question once, with its facts, the Later-release run view and a Send that waits for text", async () => {
    const fake = seeded();
    fake.seedRequests([requestSummary("R-1", { blocks: ["W-2 pricing"], reason: "stop rule" }), mergeAck("R-2")]);
    await open2(fake);
    const stop = card("R-1");
    expect(stop.getAllByText("Which database?")).toHaveLength(1);
    expect(stop.getByText("Blocks")).toBeTruthy();
    expect(stop.getByText("W-2 pricing")).toBeTruthy();
    expect(stop.getByText("Stage")).toBeTruthy();
    expect(stop.getByText("Reason")).toBeTruthy();
    expect(stop.getByLabelText("Your answer")).toBeTruthy();
    const later = stop.getByText("Open run view").closest("[data-not-built]");
    expect(later?.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByRole("article", { name: "Request R-1" }).getAttribute("data-kind")).toBe("stop");
  });

  it("shows each gap of a gaps request with its section, question and severity", async () => {
    const fake = seeded();
    fake.seedRequests([mergeAck("R-2"), requestSummary("R-4", { kind: "gaps" })]);
    fake.bodies["R-4"] = {
      gaps: [
        { section: "§3", question: "Is the cache in scope?", severity: "blocking" },
        { section: "§5", question: "Which timeout?", severity: "nice-to-have" },
      ],
    };
    await boot(fake).then(async (user) => {
      await user.click(rail().getByRole("button", { name: "Needs you" }));
    });
    const gaps = await screen.findByRole("list", { name: "Gaps of R-4" });
    expect(within(gaps).getAllByRole("listitem")).toHaveLength(2);
    expect(within(gaps).getByText("Is the cache in scope?")).toBeTruthy();
    expect(within(gaps).getByText("nice-to-have")).toBeTruthy();
  });

  it("shows the parked card's facts, its note field as not built (it takes no text) and Open run view as not built", async () => {
    const fake = seeded();
    await open(fake);
    const p = card("R-3");
    expect(p.getByText("Cause")).toBeTruthy();
    expect(p.getByText("its tests cannot reach the database", { selector: "dd, span" })).toBeTruthy();
    expect(p.getByText("Fix the cause on the server, then resume w4.")).toBeTruthy();
    const field = p.getByText("Note for the Implementer").closest("[data-not-built]");
    expect(field?.getAttribute("data-not-built")).toBe("field");
    expect(p.queryByRole("textbox")).toBeNull();
    expect(p.getByText("Open run view").closest("[data-not-built]")).not.toBeNull();
    // The request carries no attempt count and no asker at this head.
    expect(p.queryByText(/Asked by|Parked by|attempts/)).toBeNull();
  });

  it("shows a chat question card for each live session that needs you, and Open session opens that chat", async () => {
    const fake = seeded();
    fake.sessions = [
      chatSession("S-1", { project_id: "P-1", project_name: "alpha", state: "needs_you", branch: "feat-x", last_turn_at: ago(1) }),
      chatSession("S-2", { project_id: "P-2", project_name: "beta", state: "working" }),
    ];
    fake.projects = [];
    const user = await open(fake);
    await push(fake, frames.hello(fake.sessions));
    const chat = within(await screen.findByRole("article", { name: "Chat S-1" }));
    expect(chat.getByText("Orchestrator")).toBeTruthy();
    expect(chat.getByText("· alpha")).toBeTruthy();
    expect(chat.getByText("Waiting for your answer in the chat")).toBeTruthy();
    expect(chat.getByText("feat-x")).toBeTruthy();
    expect(screen.queryByRole("article", { name: "Chat S-2" })).toBeNull();
    // The chat question follows the requests and is not in the Open count.
    expect(main().getByTestId("open-count").textContent).toBe("3");
    const cards = main().getAllByRole("article");
    expect(cards.at(-1)?.getAttribute("aria-label")).toBe("Chat S-1");

    await user.click(chat.getByRole("button", { name: "Open session" }));
    expect(await screen.findByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
  });

  it("lists the merge card first, then the other requests by when they opened", async () => {
    const fake = seeded();
    fake.seedRequests([
      requestSummary("R-1", { opened_at: "2026-11-10T10:30:00.000Z" }),
      parked("R-3", { opened_at: "2026-11-10T10:10:00.000Z" }),
      mergeAck("R-2", { opened_at: "2026-11-10T11:00:00.000Z" }),
    ]);
    await open2(fake);
    expect(openList().getAllByRole("article").map((c) => c.getAttribute("aria-label"))).toEqual([
      "Request R-2",
      "Request R-3",
      "Request R-1",
    ]);
  });

  it("shows no claim line and no Runs list", async () => {
    const fake = seeded();
    fake.runs = [runSummary("R-1", { waiting: "x" })];
    await open(fake);
    expect(main().queryByText("The claim")).toBeNull();
    expect(main().queryByText(/generation/)).toBeNull();
    expect(main().queryByRole("list", { name: "Runs" })).toBeNull();
    expect(main().queryByRole("article", { name: "Run R-1" })).toBeNull();
  });
});

describe("Answered today", () => {
  it("names this device, a CLI claim holder, or the claim's generation, and calls show once for each row", async () => {
    const fake = new FakeTransport();
    const closed = (id: string, patch: Partial<RequestSummary> = {}) =>
      requestSummary(id, { status: "answered", revision: 2, answered_at: ago(2), verbs: [], ...patch });
    fake.seedRequests(
      [closed("R-7"), closed("R-8"), closed("R-9"), mergeAck("R-10", { status: "answered", revision: 2, answered_at: ago(3), verbs: [] })],
      claimRecord({ generation: 4, holder: "session C-2", device: null }),
    );
    // `show` answers by request: the answer text and the generation that answered it.
    const original = fake.show.bind(fake);
    const generations: Record<string, number> = { "R-7": 4, "R-8": 2, "R-9": 4, "R-10": 4 };
    fake.show = async (args) => {
      const detail = await original(args);
      return {
        ...detail,
        answer: args.id === "R-10" ? null : { verb: "answer", text: `answer to ${args.id}` },
        answer_generation: generations[args.id] ?? null,
      };
    };
    fake.bodies["R-7"] = { stop_text: "Seven?" };
    const user = await boot(fake);
    await user.click(rail().getByRole("button", { name: "Needs you" }));

    const row7 = within(await answeredList().findByRole("listitem", { name: "Answered R-7" }));
    expect(row7.getByText("Seven?")).toBeTruthy();
    expect(row7.getByText("answer to R-7")).toBeTruthy();
    expect(row7.getByText(/CLI · claim session C-2/)).toBeTruthy();
    expect(within(answeredList().getByRole("listitem", { name: "Answered R-8" })).getByText(/claim 2/)).toBeTruthy();
    expect(within(answeredList().getByRole("listitem", { name: "Answered R-10" })).getByText("Merge approved")).toBeTruthy();
    expect(main().getByTestId("answered-count").textContent).toBe("4");
    expect(fake.showArgs.map((a) => a.id).sort()).toEqual(["R-10", "R-7", "R-8", "R-9"]);
  });

  it("names the device when the claim's holder is a device", async () => {
    const fake = new FakeTransport();
    fake.seedRequests(
      [requestSummary("R-7", { status: "answered", revision: 2, answered_at: ago(2), verbs: [] })],
      claimRecord({ generation: 4, holder: "device D-1", device: "D-1" }),
    );
    const original = fake.show.bind(fake);
    fake.show = async (args) => ({ ...(await original(args)), answer: { verb: "answer", text: "yes" }, answer_generation: 4 });
    const user = await boot(fake);
    await user.click(rail().getByRole("button", { name: "Needs you" }));
    const row = within(await answeredList().findByRole("listitem", { name: "Answered R-7" }));
    expect(row.getByText(/MacBook/)).toBeTruthy();
  });
});

describe("the Open count", () => {
  it("is the open requests only: the page, the rail and the title bar never disagree", async () => {
    const fake = seeded();
    fake.sessions = [chatSession("S-1", { project_id: "P-1", project_name: "alpha", state: "needs_you" })];
    await open(fake);
    await push(fake, frames.hello(fake.sessions));
    await screen.findByRole("article", { name: "Chat S-1" });
    expect(main().getByTestId("open-count").textContent).toBe("3");
    expect(rail().getByRole("button", { name: "Needs you" }).querySelector(".ui-count")?.textContent).toBe("3");
  });
});

/** Like `open`, for a fake whose merge card is R-2 and whose stop text may be missing: wait for the page's cards. */
async function open2(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await user.click(rail().getByRole("button", { name: "Needs you" }));
  await screen.findByRole("article", { name: "Request R-2" });
  await waitFor(() => expect(fake.count("show")).toBeGreaterThan(0));
  await waitFor(() => expect(main().queryByText("Reading the requests.")).toBeNull());
  await screen.findByTestId("merge-sha").catch(() => undefined);
  return user;
}
