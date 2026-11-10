// AC-18: the start card (r1.s4, Approach 8). A `start_card` item shows the plan's facts. Confirm start
// asks for a one-use code and opens a dialog. Arm the run spends the code. The card learns the outcome
// from the `start_result` item on the stream, and it never shows the code.
import type { ArmSummary, ChatItemBody } from "@pulse-factory/api-client";
import { waitFor, within, screen, act } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { clock } from "../components/format";
import { alpha, bootChat, main, push, seeded } from "./chat-helpers";
import { frames, item } from "./fake-transport";

const PLAN = "a".repeat(64);
const COMMAND_ID = /^[0-9a-f-]{16,}$/;

const summary: ArmSummary = {
  schema_version: 1,
  spine: "s1",
  items: 3,
  workflow: { name: "ossify-spine", version: 2 },
  roles: [
    { name: "implementer", runtime: "claude-code", model: "fake-model" },
    { name: "reviewer", runtime: "claude-code", model: "other-model" },
  ],
  caps: { model_sessions: 12, cold_builds: 2, seeds: [{ id: "w1", max: 3 }], relaunch: "once", loops: [{ track: "main", from: "review", to: "fix", max: 2 }] },
  plugins: [{ name: "ossify", version: "1.37.1" }],
  most_launches: 9,
  merges: [{ track: "main", base: "main", sentence: "merges on the operator's ack" }],
  base_pushes: [{ track: "main", stage: "plan", dir: "/home/dev/alpha", branch: "spine/s1" }],
  replaces: null,
  paper: "paper-spine-d20",
  forge_repo: "pulseai-labs/alpha",
  default_branch: "main",
};

const startCardBody = (over: Partial<Extract<ChatItemBody, { kind: "start_card" }>> = {}): ChatItemBody => ({
  kind: "start_card",
  proposal: "sp-1",
  project_id: "P-1",
  project_name: "alpha",
  plan_sha256: PLAN,
  summary,
  params: { goal: "demo" },
  ledger: "docs/ledger.md",
  plugin_roots: [],
  ...over,
});

const frame = (seq: number, body: ChatItemBody, patch = {}) => frames.item(item("S-1", seq, body, patch));
const card = (seq = 1, over = {}) => frame(seq, startCardBody(over));
const result = (seq: number, answers: number, over: Partial<Extract<ChatItemBody, { kind: "start_result" }>> = {}) =>
  frame(
    seq,
    { kind: "start_result", outcome: "answered", result: "armed", reason: null, run: "run-1", device: "D-1", ...over },
    { answers_seq: answers },
  );

const startCard = () => within(screen.getByTestId("start-card"));
const dialog = () => within(screen.getByRole("dialog", { name: "Confirm the start of s1" }));

describe("a start card", () => {
  it("shows Start workflow with every fact group", async () => {
    await bootChat(seeded(), "S-1", [card()]);
    const el = screen.getByRole("article", { name: "Start workflow" });
    expect(el.getAttribute("data-proposal")).toBe("sp-1");
    const c = within(el);
    for (const name of [
      "Spine",
      "Project",
      "Items",
      "Workflow",
      "Roles",
      "Caps",
      "Plugins",
      "Most launches",
      "Merges",
      "Base pushes",
      "Forge",
      "Paper env",
      "Ledger",
      "Parameters",
    ]) {
      expect(c.getByRole("group", { name }), name).toBeTruthy();
    }
    expect(c.getByRole("group", { name: "Roles" }).textContent).toContain("implementer: claude-code, fake-model");
    expect(c.getByRole("group", { name: "Roles" }).textContent).toContain("reviewer: claude-code, other-model");
    expect(c.getByRole("group", { name: "Parameters" }).textContent).toContain("goal=demo");
    expect(c.getByRole("button", { name: "Confirm start" })).toBeTruthy();
  });

  it("leaves out a group whose value is empty", async () => {
    const bare: ArmSummary = { ...summary, items: null, most_launches: null, paper: null, plugins: [], merges: [], base_pushes: [] };
    await bootChat(seeded(), "S-1", [card(1, { summary: bare, params: {}, ledger: "" })]);
    const c = startCard();
    for (const name of ["Items", "Most launches", "Paper env", "Plugins", "Merges", "Base pushes", "Parameters", "Ledger"]) {
      expect(c.queryByRole("group", { name }), name).toBeNull();
    }
    expect(c.getByRole("group", { name: "Spine" })).toBeTruthy();
  });

  it("Confirm start asks for a code once, with the arm subject and a command ID, and opens the dialog", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));

    await waitFor(() => expect(screen.getByTestId("start-dialog")).toBeTruthy());
    expect(fake.confirmArgs).toEqual([
      { subject: { kind: "arm", proposal: "sp-1", plan_sha256: PLAN }, command_id: expect.stringMatching(COMMAND_ID) },
    ]);
    const d = dialog();
    expect(d.getByText(new RegExp(`expires at ${clock(Date.parse("2026-11-09T10:10:00Z"))}`))).toBeTruthy();
    expect(d.getByText(/device D-1/)).toBeTruthy();
    expect(d.getByRole("button", { name: "Arm the run" })).toBeTruthy();
    expect(d.getByRole("button", { name: "Cancel" })).toBeTruthy();
    // The code is in no text of the page.
    expect(document.body.textContent).not.toContain(fake.nextCode);
    expect(document.body.innerHTML).not.toContain(fake.nextCode);
  });

  it("Arm the run sends the code with a new command ID, shows Arming… and then the result", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await user.click(await screen.findByRole("button", { name: "Arm the run" }));

    await waitFor(() => expect(fake.armArgs).toHaveLength(1));
    expect(fake.armArgs[0]).toEqual({
      proposal: "sp-1",
      plan_sha256: PLAN,
      code: fake.nextCode,
      command_id: expect.stringMatching(COMMAND_ID),
    });
    expect(fake.armArgs[0]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    await waitFor(() => expect(screen.getByTestId("start-outcome").textContent).toBe("Arming…"));
    expect(screen.queryByTestId("start-dialog")).toBeNull();
    expect(document.body.innerHTML).not.toContain(fake.nextCode);

    await push(fake, result(2, 1));
    expect(screen.getByTestId("start-outcome").textContent).toBe("Armed: run run-1 from D-1");
    expect(startCard().queryByRole("button", { name: "Confirm start" })).toBeNull();
    // The result shows inside its card, not as an entry of its own.
    expect(within(main().getByRole("log", { name: "Messages" })).queryByRole("article", { name: "Answer" })).toBeNull();
  });

  it("Cancel calls nothing more and drops the code", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await screen.findByTestId("start-dialog");
    const before = [...fake.calls];
    await user.click(dialog().getByRole("button", { name: "Cancel" }));

    expect(screen.queryByTestId("start-dialog")).toBeNull();
    expect(fake.calls).toEqual(before);
    expect(fake.armArgs).toHaveLength(0);
    // The next Confirm start asks again, with a new command ID.
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await waitFor(() => expect(fake.confirmArgs).toHaveLength(2));
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
  });

  it("a refused arm shows its message as an alert, drops the code and the next Confirm start asks again", async () => {
    const fake = seeded();
    fake.fail("arm", "refused", "the code was already used", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await user.click(await screen.findByRole("button", { name: "Arm the run" }));

    expect((await startCard().findByRole("alert")).textContent).toBe("the code was already used");
    expect(screen.queryByTestId("start-dialog")).toBeNull();
    expect(screen.queryByTestId("start-outcome")).toBeNull();

    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await waitFor(() => expect(fake.confirmArgs).toHaveLength(2));
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    await user.click(await screen.findByRole("button", { name: "Arm the run" }));
    await waitFor(() => expect(fake.armArgs).toHaveLength(2));
    expect(fake.armArgs[1]?.command_id).not.toBe(fake.armArgs[0]?.command_id);
  });

  it("a refused Confirm start shows its message as an alert and opens no dialog", async () => {
    const fake = seeded();
    fake.fail("confirmCode", "refused", "the proposal is not open", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));

    expect((await startCard().findByRole("alert")).textContent).toBe("the proposal is not open");
    expect(screen.queryByTestId("start-dialog")).toBeNull();
    expect(fake.count("arm")).toBe(0);
  });

  it("a retry of Confirm start after unreachable reuses the command ID", async () => {
    const fake = seeded();
    fake.fail("confirmCode", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    expect((await startCard().findByRole("alert")).textContent).toBe("the server did not answer");

    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await screen.findByTestId("start-dialog");
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).toBe(fake.confirmArgs[0]?.command_id);
  });

  it("a retry of Arm the run after unreachable keeps the dialog and reuses the command ID", async () => {
    const fake = seeded();
    fake.fail("arm", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await user.click(await screen.findByRole("button", { name: "Arm the run" }));

    expect((await dialog().findByRole("alert")).textContent).toBe("the server did not answer");
    await user.click(dialog().getByRole("button", { name: "Arm the run" }));
    await waitFor(() => expect(fake.armArgs).toHaveLength(2));
    expect(fake.armArgs[1]).toEqual(fake.armArgs[0]);
    expect(fake.confirmArgs).toHaveLength(1);
    await waitFor(() => expect(screen.getByTestId("start-outcome").textContent).toBe("Arming…"));
  });

  it("a replayed confirm answer has no code to show: it says so and asks for a new one next time", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    // The first answer is lost on the way: the server holds the code, the app got `unreachable`.
    fake.fail("confirmCode", "unreachable", "the server did not answer", 1);
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await startCard().findByRole("alert");
    // The server did admit it, so the replay of the same command ID answers with no code.
    await act(async () => {
      await fake.confirmCode(fake.confirmArgs[0]!).catch(() => undefined);
    });
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    expect((await startCard().findByRole("alert")).textContent).toMatch(/cannot show it again/);
    expect(screen.queryByTestId("start-dialog")).toBeNull();
    await user.click(startCard().getByRole("button", { name: "Confirm start" }));
    await screen.findByTestId("start-dialog");
    expect(fake.confirmArgs.at(-1)?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
  });
});

describe("the end of a start card", () => {
  it("a withdrawn result shows Withdrawn: <reason> and no Confirm start", async () => {
    await bootChat(seeded(), "S-1", [card(), result(2, 1, { result: "withdrawn", reason: "halted", run: null, device: null })]);
    expect(screen.getByTestId("start-outcome").textContent).toBe("Withdrawn: halted");
    expect(startCard().queryByRole("button", { name: "Confirm start" })).toBeNull();
  });

  it("a failed result shows Failed: <reason> and no Confirm start", async () => {
    await bootChat(seeded(), "S-1", [card(), result(2, 1, { result: "failed", reason: "the run could not be created", run: null, device: "D-1" })]);
    expect(screen.getByTestId("start-outcome").textContent).toBe("Failed: the run could not be created");
    expect(startCard().queryByRole("button", { name: "Confirm start" })).toBeNull();
  });

  it("a result with no card held shows as an Answer entry", async () => {
    await bootChat(seeded(), "S-1", [result(5, 3)]);
    const log = within(main().getByRole("log", { name: "Messages" }));
    expect(log.getByRole("article", { name: "Answer" }).textContent).toMatch(/start card this app has not seen \(armed\)/);
    expect(screen.queryByTestId("start-card")).toBeNull();
  });

  it("an ended session shows no Confirm start", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [card()]);
    expect(startCard().getByRole("button", { name: "Confirm start" })).toBeTruthy();
    await push(fake, frames.session(alpha({ ended_at: "2026-11-09T11:00:00Z", state: "idle" })));
    expect(startCard().queryByRole("button", { name: "Confirm start" })).toBeNull();
    expect(fake.count("confirmCode")).toBe(0);
  });
});
