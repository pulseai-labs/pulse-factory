// AC-8 (r1.s4.w3): the confirm dialog. One act is two clicks: the first asks for a one-use confirm
// code, the dialog shows what it binds, the second spends it. Cancel, Escape and leaving the page spend
// nothing, a refusal says why, and the code is in memory only.
//
// Known limit (Q2, C1): the code proves the device, the freshness and the head, not that a person pressed
// the button. These tests check the form of the control, and claim no more.
import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { boot, type User } from "./chat-helpers";
import { FakeTransport, claimRecord, mergeAck, requestFrames, requestSummary } from "./fake-transport";

const rail = () => within(screen.getByRole("navigation"));
const card = (id: string) => within(screen.getByRole("article", { name: `Request ${id}` }));
const dialog = () => within(screen.getByRole("dialog"));

/** The code the fake issues as its n-th: `pfc1-` and 32 hex digits. */
const codeN = (n: number) => `pfc1-${n.toString(16).padStart(32, "0")}`;

const HEAD = "a".repeat(40);

/** A fake with a stop request R-1 (text "Which database?") and a merge approval R-2, and a claim held by ann. */
function seeded(claim: FakeTransport["claim"] = claimRecord({ generation: 3, holder: "ann@host" })): FakeTransport {
  const fake = new FakeTransport();
  fake.seedRequests([requestSummary("R-1"), mergeAck("R-2")], claim);
  fake.bodies["R-1"] = { stop_text: "Which database?" };
  return fake;
}

async function open(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await user.click(rail().getByRole("button", { name: "Needs you" }));
  await screen.findByRole("article", { name: "Request R-2" });
  await screen.findByText("Which database?");
  return user;
}

async function typeAnswer(user: User, text: string) {
  await user.type(card("R-1").getByLabelText("Your answer"), text);
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

describe("the first click", () => {
  it("asks for a code once, with the answer's SHA-256 (a fixed vector), and sends nothing else", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "abc");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));

    await screen.findByRole("dialog", { name: "Confirm answer" });
    await waitFor(() => expect(fake.count("confirmCode")).toBe(1));
    expect(fake.confirmArgs).toHaveLength(1);
    const args = fake.confirmArgs[0];
    expect(args?.subject).toEqual({
      kind: "request",
      request: "R-1",
      revision: 1,
      verb: "answer",
      text_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    });
    expect(args?.command_id).toMatch(/\S+/);
    expect(fake.count("answer") + fake.count("ack") + fake.count("clear")).toBe(0);
  });

  it("hashes the UTF-8 bytes of the text", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "é ✓");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await screen.findByRole("dialog");
    expect(fake.confirmArgs[0]?.subject).toMatchObject({
      text_sha256: "086a17ec1cb4f336b148c38dfa8e349285c17c994b282b79631088d801c42005",
    });
  });

  it("asks for a merge code with the request and its revision, and no hash", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await screen.findByRole("dialog", { name: "Confirm merge" });
    expect(fake.confirmArgs).toHaveLength(1);
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-2", revision: 1, verb: "ack" });
    expect(fake.count("ack")).toBe(0);
  });

  it("asks for nothing before a click: a frame, a refresh or a reload spends and asks for no code", async () => {
    const fake = seeded();
    const user = await open(fake);
    await act(async () => fake.requestFrame(requestFrames.request(requestSummary("R-5"))));
    await user.click(screen.getAllByRole("button", { name: "Refresh" })[0] as HTMLElement);
    expect(fake.count("confirmCode")).toBe(0);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("locks the answer text while the dialog is open", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "abc");
    const area = card("R-1").getByLabelText("Your answer") as HTMLTextAreaElement;
    expect(area.readOnly).toBe(false);
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await screen.findByRole("dialog");
    expect(area.readOnly).toBe(true);
    await user.type(area, "def");
    expect(area.value).toBe("abc");
    await user.click(dialog().getByRole("button", { name: "Cancel" }));
    expect(area.readOnly).toBe(false);
  });
});

describe("the dialog", () => {
  it("shows the PR, the base, the head branch, the head SHA, whom the act takes the claim from, and the expiry", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await screen.findByRole("dialog", { name: "Confirm merge" });

    const d = dialog();
    expect(await d.findByText("pulseai-labs/example#9")).toBeTruthy();
    expect(d.getByText("main")).toBeTruthy();
    expect(d.getByText("spine/s1")).toBeTruthy();
    expect(d.getByText(HEAD)).toBeTruthy();
    expect(d.getByText("Takes the claim from ann@host")).toBeTruthy();
    expect(d.getByText("2026-11-10T10:02:00.000Z")).toBeTruthy();
    expect(screen.getByRole("dialog").getAttribute("aria-modal")).toBe("true");
    expectNoCode();
  });

  it("shows the request and the exact answer text for an answer", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "use sqlite, keep it small");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await screen.findByRole("dialog", { name: "Confirm answer" });
    const d = dialog();
    expect(await d.findByText("use sqlite, keep it small")).toBeTruthy();
    expect(d.getByText("R-1")).toBeTruthy();
    expect(d.getByRole("button", { name: "Send" })).toBeTruthy();
  });

  it("says whom the act takes the claim from when another holder has it", async () => {
    const fake = seeded(claimRecord({ holder: "ann@host", device: null }));
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    expect(await dialog().findByText("Takes the claim from ann@host")).toBeTruthy();
  });

  it("says that this device holds the claim", async () => {
    const fake = seeded(claimRecord({ generation: 4, holder: "device D-1", device: "D-1" }));
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    expect(await dialog().findByText("This device holds the claim")).toBeTruthy();
  });

  it("says that no one holds the claim", async () => {
    const fake = seeded(null);
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    expect(await dialog().findByText("No one holds the claim")).toBeTruthy();
  });
});

describe("the second click", () => {
  it("sends ack with the code the dialog holds and a new command ID, then closes and reads the runs once", async () => {
    const fake = seeded();
    const user = await open(fake);
    const runsBefore = fake.count("runList");
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Merge" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(fake.actArgs).toHaveLength(1);
    const { verb, args } = fake.actArgs[0] as (typeof fake.actArgs)[number];
    expect(verb).toBe("ack");
    expect(args).toMatchObject({ request: "R-2", rev: 1, code: codeN(1) });
    expect(args.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expect(fake.count("runList")).toBe(runsBefore + 1);

    // The request leaves Open requests and its outcome shows.
    expect(screen.queryByRole("article", { name: "Request R-2" })).toBeNull();
    expect(await screen.findByText(`Merged at ${HEAD} (landed)`)).toBeTruthy();
    expectNoCode();
  });

  it("sends answer with the typed text, the code and a new command ID, and shows the claim it took", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "sqlite");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await dialog().findByText("sqlite");
    await user.click(dialog().getByRole("button", { name: "Send" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const { verb, args } = fake.actArgs[0] as (typeof fake.actArgs)[number];
    expect(verb).toBe("answer");
    expect(args).toMatchObject({ request: "R-1", rev: 1, text: "sqlite", code: codeN(1) });
    expect(args.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expect(await screen.findByText("Answered under claim 4")).toBeTruthy();
    // The claim line follows the act: this device holds it now.
    expect(screen.getByText("device D-1, generation 4")).toBeTruthy();
    // An answer reads no run list: only an ack does.
    expect(fake.count("runList")).toBe(1);
    expectNoCode();
  });

  it("sends nothing on Cancel, on Escape and on leaving the page, and a later first click asks for a new code", async () => {
    const fake = seeded();
    const user = await open(fake);

    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();

    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();

    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(rail().getByRole("button", { name: "Projects" }));
    expect(screen.queryByRole("dialog")).toBeNull();

    expect(fake.count("ack") + fake.count("answer") + fake.count("clear")).toBe(0);
    expect(fake.count("confirmCode")).toBe(3);
    const ids = fake.confirmArgs.map((a) => a.command_id);
    expect(new Set(ids).size).toBe(3);

    // Back on the page, a new click gives a new code, and only the second click spends it.
    await user.click(rail().getByRole("button", { name: "Needs you" }));
    await user.click(await screen.findByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    await waitFor(() => expect(fake.count("ack")).toBe(1));
    expect(fake.actArgs[0]?.args).toMatchObject({ code: codeN(4) });
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

  it.each(WORDED)("%s gives its own line, drops the code and lets a new first click ask again", async (word, line) => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "the server's text, which the line does not use", 1, word);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Merge" }));

    expect(await dialog().findByText(line)).toBeTruthy();
    // The dialog ended: nothing is left to spend.
    expect(within(screen.getByRole("dialog")).queryByRole("button", { name: "Merge" })).toBeNull();
    expectNoCode();
    // The card stays.
    expect(screen.getByRole("article", { name: "Request R-2" })).toBeTruthy();

    await user.click(dialog().getByRole("button", { name: "Close" }));
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    await waitFor(() => expect(fake.count("ack")).toBe(2));
    expect(fake.actArgs[1]?.args).toMatchObject({ code: codeN(2) });
    expect(fake.actArgs[1]?.args.command_id).not.toBe(fake.actArgs[0]?.args.command_id);
  });

  it("subject_moved says the request changed and reads the requests once", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "the server's text", 1, "subject_moved");
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    const reads = fake.count("requests");
    await user.click(dialog().getByRole("button", { name: "Merge" }));

    expect(await dialog().findByText("The request changed since the code was issued. Read it again and confirm.")).toBeTruthy();
    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
  });

  it("claim_moved shows the server's text, which names the new holder, and Confirm again", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("answer", "refused", "the claim moved to bob@host (generation 4)", 1, "claim_moved");
    await typeAnswer(user, "sqlite");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await dialog().findByText("sqlite");
    await user.click(dialog().getByRole("button", { name: "Send" }));

    expect(await dialog().findByText("the claim moved to bob@host (generation 4) Confirm again.")).toBeTruthy();
  });

  it("device_revoked shows the server's text", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "this device was revoked", 1, "device_revoked");
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    expect(await dialog().findByText("this device was revoked")).toBeTruthy();
  });

  it("an engine refusal with no word shows the server's text", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "refused", "the spine's checks are red", 1);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    expect(await dialog().findByText("the spine's checks are red")).toBeTruthy();
    expect(screen.getByRole("article", { name: "Request R-2" })).toBeTruthy();
  });

  it("a refusal on the first click shows the server's text and ends the dialog", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "refused", "the request is closed", 1);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    expect(await dialog().findByText("the request is closed")).toBeTruthy();
    expect(fake.count("ack")).toBe(0);
  });
});

describe("an answer that never arrived", () => {
  it("unreachable on the second click keeps the code and the command ID: Retry resends both", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("ack", "unreachable", "no answer", 1);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Merge" }));

    const retry = await dialog().findByRole("button", { name: "Retry" });
    expect(fake.actArgs).toHaveLength(1);
    expectNoCode();
    await user.click(retry);

    await waitFor(() => expect(fake.actArgs).toHaveLength(2));
    expect(fake.actArgs[1]?.args.code).toBe(fake.actArgs[0]?.args.code);
    expect(fake.actArgs[1]?.args.command_id).toBe(fake.actArgs[0]?.args.command_id);
    // Only one code was ever asked for.
    expect(fake.count("confirmCode")).toBe(1);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("unreachable on the first click: Retry asks with a new command ID", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "unreachable", "no answer", 1);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    const retry = await dialog().findByRole("button", { name: "Retry" });
    expect(fake.confirmArgs).toHaveLength(1);

    await user.click(retry);
    await dialog().findByText("spine/s1");
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expect(fake.count("ack")).toBe(0);
  });

  it("a replayed answer closes the dialog, reads the requests once and claims no outcome while the request is open", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "sqlite");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await dialog().findByText("sqlite");
    const reads = fake.count("requests");
    fake.replayOnce("answer");
    await user.click(dialog().getByRole("button", { name: "Send" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await waitFor(() => expect(fake.count("requests")).toBe(reads + 1));
    // The read says it is still open: the card stays and no outcome shows.
    expect(screen.getByRole("article", { name: "Request R-1" })).toBeTruthy();
    expect(screen.queryByText(/Answered under claim/)).toBeNull();
    expect(screen.queryByText(/Done before/)).toBeNull();
    // A replay acts on nothing: the claim did not move.
    expect(screen.getByText("ann@host, generation 3")).toBeTruthy();
  });

  it("a replayed answer shows `Done before` when the read says the request closed", async () => {
    const fake = seeded();
    const user = await open(fake);
    await typeAnswer(user, "sqlite");
    await user.click(card("R-1").getByRole("button", { name: "Send answer" }));
    await dialog().findByText("sqlite");
    // The first send landed, and its answer was lost.
    const closedAt = new Date().toISOString();
    fake.requestRows = fake.requestRows.map((r) =>
      r.id === "R-1" ? { ...r, status: "answered", answered_at: closedAt, verbs: [] } : r,
    );
    fake.replayOnce("answer");
    await user.click(dialog().getByRole("button", { name: "Send" }));

    expect(await screen.findByText(`Done before: answered at ${closedAt}`)).toBeTruthy();
    expect(screen.queryByText(/Answered under claim/)).toBeNull();
    expect(fake.count("answer")).toBe(1);
  });

  it("a replayed ack reads the requests once and the runs once, and claims no merge", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    const reads = fake.count("requests");
    const runs = fake.count("runList");
    const closedAt = new Date().toISOString();
    fake.requestRows = fake.requestRows.map((r) =>
      r.id === "R-2" ? { ...r, status: "answered", answered_at: closedAt, verbs: [] } : r,
    );
    fake.replayOnce("ack");
    await user.click(dialog().getByRole("button", { name: "Merge" }));

    expect(await screen.findByText(`Done before: answered at ${closedAt}`)).toBeTruthy();
    expect(fake.count("requests")).toBe(reads + 1);
    expect(fake.count("runList")).toBe(runs + 1);
    expect(screen.queryByText(/Merged at/)).toBeNull();
  });
});

describe("a frame that closes the request while its spend is in flight", () => {
  // The real controller pushes the `request` frame as soon as the act is admitted, which can be before the
  // answer of the HTTP call (an ack merges through the forge first). The dialog must outlive the card.
  it("keeps the dialog, then shows the outcome and reads the runs once, when the ack answers", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    const runsBefore = fake.count("runList");

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
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    await waitFor(() => expect(sent).toBe(true));

    await act(async () =>
      fake.requestFrame(requestFrames.request({ ...mergeAck("R-2"), status: "answered", answered_at: new Date().toISOString(), verbs: [] })),
    );
    // The card left Open requests; the dialog is still there, waiting for its answer.
    expect(screen.queryByRole("article", { name: "Request R-2" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Confirm merge" })).toBeTruthy();

    await act(async () => release());
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await screen.findByText(`Merged at ${HEAD} (landed)`)).toBeTruthy();
    expect(fake.count("runList")).toBe(runsBefore + 1);
  });

  it("applies the act's outcome even when the operator left the page before the answer", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");

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
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    await waitFor(() => expect(sent).toBe(true));
    await user.click(rail().getByRole("button", { name: "Projects" }));

    // The act's answer applies to the shared state with no read: the rail's count drops from 2 to 1.
    const badge = () => rail().getByRole("button", { name: "Needs you" }).querySelector(".rail-badge");
    expect(badge()?.textContent).toBe("2");
    await act(async () => release());
    await waitFor(() => expect(badge()?.textContent).toBe("1"));
    expect(fake.count("requests")).toBe(1);
  });
});

describe("the code stays in memory", () => {
  it("is in no DOM text or storage at any step: ready, refused, retried and spent", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    await dialog().findByText("spine/s1");
    expectNoCode();
    fake.fail("ack", "unreachable", "no answer", 1);
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    await dialog().findByRole("button", { name: "Retry" });
    expectNoCode();
    await user.click(dialog().getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expectNoCode();
    expect(fake.actArgs[0]?.args.code).toMatch(/^pfc1-[0-9a-f]{32}$/);
  });

  it("closes the dialog on an unauthorized first click and shows the 3c banner", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("confirmCode", "unauthorized", "revoked");
    await user.click(card("R-2").getByRole("button", { name: "Confirm merge" }));
    expect(await screen.findByText(/This device is no longer paired with/)).toBeTruthy();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(fake.count("ack")).toBe(0);
  });
});
