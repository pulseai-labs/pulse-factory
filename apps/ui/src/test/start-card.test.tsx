// AC-2 of `r1.s5.w3`: the start card (11c). A `start_card` item shows the plan's facts, limits first. One
// press on Start workflow asks for a one-use code and arms the run with it (G1). The card learns the
// outcome from the `start_result` item on the stream, and it never shows the code.
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
  // The two forms the server writes (`arm.rs:315-322`), as the `ossify-spine` workflow's `item` and `spine` tracks give them.
  merges: [
    { track: "item", base: "spine/s1", sentence: "item PRs merge into spine/s1 on green checks with no ack" },
    { track: "spine", base: "main", sentence: "the spine PR merges into main only on the claim's ack" },
  ],
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
const press = () => startCard().getByTestId("start-workflow") as HTMLButtonElement;
const group = (name: string) => startCard().getByRole("group", { name });

describe("a start card (11c)", () => {
  it("shows the limits first, the merge line, the work items and who runs it, and one Start workflow", async () => {
    await bootChat(seeded(), "S-1", [card(1)]);
    const el = screen.getByRole("article", { name: "Start workflow" });
    expect(el.getAttribute("data-proposal")).toBe("sp-1");
    // The head: the eyebrow with the time of the proposal, the spine ID and the project as a chip.
    expect(startCard().getByText("Start a workflow")).toBeTruthy();
    expect(startCard().getByText(`proposed ${clock(Date.parse("2026-11-09T10:00:00Z"))}`)).toBeTruthy();
    expect(startCard().getByText("s1")).toBeTruthy();
    expect(startCard().getByText("alpha")).toBeTruthy();

    // Limits: tiles for the caps the plan has, then the other caps and the stop sentence.
    const limits = within(group("Limits"));
    expect(limits.getByText("12")).toBeTruthy();
    expect(limits.getByText("Model sessions")).toBeTruthy();
    expect(limits.getByText("Cold builds")).toBeTruthy();
    expect(within(group("Most sessions")).getByText("9")).toBeTruthy();
    expect(limits.getByText("The run stops and asks you when it reaches a limit.")).toBeTruthy();
    // The per-loop line is gone (d24-1, d24-4): seeds, relaunch and loops are in the group's title.
    expect(group("Limits").getAttribute("title")).toBe("seed w1: at most 3 · relaunch: once · loop main review to fix: at most 2");
    expect(screen.getByTestId("start-card").querySelector(".limit-line")).toBeNull();

    // The merge line reads 11c's sentence, with each base in a code chip.
    expect(group("Merge line").textContent).toContain("Item PRs merge into spine/s1 without asking.");
    expect(group("Merge line").textContent).toContain("The spine PR into main waits for you.");
    expect(group("Work items").textContent).toContain("3 work items");

    // Who runs it: one row per role, with the model's display name and the runtime as a chip.
    const who = group("Who runs it");
    expect(within(who).getAllByRole("listitem")).toHaveLength(2);
    expect(who.textContent).toContain("implementer");
    expect(who.textContent).toContain("fake-model");
    expect(within(who).getAllByText("claude-code")[0]?.className).toContain("ui-chip-mono");
    // The paper mark is one chip in the head, with its reason.
    expect(group("Paper env").getAttribute("title")).toBe("No model, no spend");

    expect(startCard().getByText("Nothing launches until you start it.")).toBeTruthy();
    expect(press().textContent).toBe("Start workflow");
    expect(startCard().getAllByRole("button", { name: "Start workflow" })).toHaveLength(1);
  });

  it("shows a role's model as its display name", async () => {
    const real: ArmSummary = { ...summary, roles: [{ name: "planner", runtime: "claude-code", model: "claude-sonnet-5-5" }] };
    await bootChat(seeded(), "S-1", [card(1, { summary: real })]);
    expect(group("Who runs it").textContent).toContain("Sonnet 5.5");
  });

  it("shows the chip row under the title: the spine ID, the spine branch, an arrow and the base (KL-4)", async () => {
    await bootChat(seeded(), "S-1", [card()]);
    const row = group("Spine and branches");
    const chips = Array.from(row.querySelectorAll(".ui-chip")).map((c) => c.textContent);
    // The spine branch is the base of the first merge whose base is not the default branch; the base is the default branch.
    expect(chips).toEqual(["s1", "spine/s1", "main"]);
    expect(row.querySelector("[data-icon=chevron-right]")).not.toBeNull();
  });

  it("shows the spine ID chip only when no merge base differs from the default branch", async () => {
    const same: ArmSummary = { ...summary, merges: [{ track: "spine", base: "main", sentence: "the spine PR merges into main only on the claim's ack" }] };
    await bootChat(seeded(), "S-1", [card(1, { summary: same })]);
    const row = group("Spine and branches");
    expect(Array.from(row.querySelectorAll(".ui-chip")).map((c) => c.textContent)).toEqual(["s1"]);
    expect(row.querySelector("[data-icon=chevron-right]")).toBeNull();
  });

  it("maps the two forms the server writes to 11c's sentence, and shows any other form as the server wrote it (P5)", async () => {
    const mixed: ArmSummary = {
      ...summary,
      merges: [
        { track: "item", base: "spine/s1", sentence: "item PRs merge into spine/s1 on green checks with no ack" },
        { track: "spine", base: "main", sentence: "the spine PR merges into main only on the claim's ack" },
        { track: "lane", base: "x", sentence: "merges on the operator's ack" },
      ],
    };
    await bootChat(seeded(), "S-1", [card(1, { summary: mixed })]);
    const line = group("Merge line");
    expect(line.textContent).toContain("Item PRs merge into spine/s1 without asking.");
    expect(line.textContent).toContain("The spine PR into main waits for you.");
    expect(line.textContent).toContain("merges on the operator's ack");
    expect(line.textContent).not.toContain("green checks");
    expect(Array.from(line.querySelectorAll("code")).map((c) => c.textContent)).toEqual(["spine/s1", "main"]);
  });

  it("puts the item count at the right end of the Work items label row, with no line under it", async () => {
    await bootChat(seeded(), "S-1", [card()]);
    const work = group("Work items");
    const head = work.querySelector(".section-head") as HTMLElement;
    expect(head.textContent).toContain("Work items");
    expect(within(head).getByText("3 work items")).toBeTruthy();
    expect(work.querySelector(".section-body")).toBeNull();
  });

  it("the thirteen old groups are gone and no name of the old two steps is on the card", async () => {
    await bootChat(seeded(), "S-1", [card()]);
    for (const name of ["Spine", "Project", "Items", "Workflow", "Roles", "Caps", "Plugins", "Most launches", "Merges", "Base pushes", "Forge", "Ledger", "Parameters"]) {
      expect(startCard().queryByRole("group", { name }), name).toBeNull();
    }
    expect(startCard().queryByRole("button", { name: /Confirm start|Arm the run/ })).toBeNull();
    expect(screen.queryByTestId("start-dialog")).toBeNull();
    expect(document.body.textContent).not.toContain("ossify-spine");
    expect(document.body.textContent).not.toContain("docs/ledger.md");
    expect(document.body.textContent).not.toContain("goal=demo");
  });

  it("leaves out a group whose value is empty", async () => {
    const bare: ArmSummary = { ...summary, items: null, most_launches: null, paper: null, plugins: [], merges: [], base_pushes: [] };
    await bootChat(seeded(), "S-1", [card(1, { summary: bare, params: {}, ledger: "" })]);
    for (const name of ["Work items", "Most sessions", "Paper env", "Merge line"]) {
      expect(startCard().queryByRole("group", { name }), name).toBeNull();
    }
    expect(group("Limits")).toBeTruthy();
    expect(group("Who runs it")).toBeTruthy();
  });

  it("CLI, Change something and Open run view are not built: disabled, each with the Later release tag", async () => {
    await bootChat(seeded(), "S-1", [card()]);
    for (const label of ["CLI", "Change something"]) {
      const el = startCard().getByText(label).closest("[data-not-built]") as HTMLElement;
      expect(el.getAttribute("aria-disabled")).toBe("true");
      expect(within(el).getByText("Later release")).toBeTruthy();
      expect(el.tagName).not.toBe("BUTTON");
      // 11b and 11c draw these stubs with the label and the tag only: no icon.
      expect(el.querySelector("[data-icon]"), label).toBeNull();
    }
  });

  it("one press asks for a code once and arms once, in that order, with the issued code", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());

    await waitFor(() => expect(fake.armArgs).toHaveLength(1));
    expect(fake.confirmArgs).toEqual([
      { subject: { kind: "arm", proposal: "sp-1", plan_sha256: PLAN }, command_id: expect.stringMatching(COMMAND_ID) },
    ]);
    expect(fake.armArgs[0]).toEqual({ proposal: "sp-1", plan_sha256: PLAN, code: fake.nextCode, command_id: expect.stringMatching(COMMAND_ID) });
    expect(fake.armArgs[0]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    const order = fake.calls.filter((c) => c === "confirmCode" || c === "arm");
    expect(order).toEqual(["confirmCode", "arm"]);
    // No dialog, no expiry text, no second button.
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByTestId("start-dialog")).toBeNull();
    expect(document.body.textContent).not.toMatch(/expires/i);
    // The code is in no text or attribute of the page.
    expect(document.body.textContent).not.toContain(fake.nextCode);
    expect(document.body.innerHTML).not.toContain(fake.nextCode);
    await waitFor(() => expect(screen.getByTestId("start-outcome").textContent).toBe("Arming…"));
  });

  it("disables the button while the press is in flight", async () => {
    const fake = seeded();
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const original = fake.confirmCode.bind(fake);
    fake.confirmCode = async (args) => {
      await gate;
      return original(args);
    };
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());
    await waitFor(() => expect(press().disabled).toBe(true));
    await user.click(press());
    release();
    await waitFor(() => expect(fake.armArgs).toHaveLength(1));
    expect(fake.confirmArgs).toHaveLength(1);
  });

  it("after the start the note, CLI and Change something leave, and the Started block reads as 11c", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());
    await waitFor(() => expect(fake.armArgs).toHaveLength(1));
    await push(fake, frame(2, { kind: "start_result", outcome: "answered", result: "armed", reason: null, run: "run-1", device: "D-1" }, { answers_seq: 1, created_at: "2026-11-09T10:12:00Z" }));
    const outcome = screen.getByTestId("start-outcome");
    expect(outcome.getAttribute("role")).toBe("status");
    expect(outcome.getAttribute("data-run")).toBe("run-1");
    expect(outcome.getAttribute("data-device")).toBe("D-1");
    // The check icon, "Started", "<device> · <time>" and "Run <run> is running".
    expect(outcome.querySelector("[data-icon=check]")).not.toBeNull();
    expect(within(outcome).getByText("Started")).toBeTruthy();
    expect(within(outcome).getByText(`MacBook · ${clock(Date.parse("2026-11-09T10:12:00Z"))}`)).toBeTruthy();
    expect(outcome.textContent).toContain("Run run-1 is running");
    // W1-31: the note and the two stubs leave with the press.
    expect(startCard().queryByText("Nothing launches until you start it.")).toBeNull();
    expect(startCard().queryByText("CLI")).toBeNull();
    expect(startCard().queryByText("Change something")).toBeNull();
    expect(screen.queryByTestId("start-workflow")).toBeNull();
    // Open run view is 11l's quiet link with its tag, inside the block, and it is not built.
    const view = within(outcome).getByText("Open run view").closest("[data-not-built]") as HTMLElement;
    expect(view.getAttribute("aria-disabled")).toBe("true");
    expect(within(view).getByText("Later release")).toBeTruthy();
    expect(view.querySelector("[data-icon]")).toBeNull();
    // The result shows inside its card, not as an entry of its own.
    expect(within(main().getByRole("log", { name: "Messages" })).queryByRole("article", { name: "Answer" })).toBeNull();
  });

  it("before the start the footer is the note, CLI, Change something and Start workflow", async () => {
    await bootChat(seeded(), "S-1", [card()]);
    const foot = screen.getByTestId("start-card").querySelector(".start-foot") as HTMLElement;
    expect(within(foot).getByText("Nothing launches until you start it.")).toBeTruthy();
    for (const label of ["CLI", "Change something"]) expect(within(foot).getByText(label).closest("[data-not-built]")).not.toBeNull();
    expect(within(foot).getByTestId("start-workflow")).toBeTruthy();
    expect(screen.queryByTestId("start-outcome")).toBeNull();
  });

  it("names the device by its ID when the result is another device's", async () => {
    await bootChat(seeded(), "S-1", [card(), result(2, 1, { device: "D-7" })]);
    expect(screen.getByTestId("start-outcome").textContent).toContain("D-7 · ");
    expect(screen.getByTestId("start-outcome").getAttribute("data-device")).toBe("D-7");
  });

  it("a refused arm shows its message, drops the code, and the next press asks for a new one", async () => {
    const fake = seeded();
    fake.fail("arm", "refused", "the code was already used", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());

    expect((await startCard().findByRole("alert")).textContent).toBe("the code was already used");
    expect(screen.queryByTestId("start-outcome")).toBeNull();
    await user.click(press());
    await waitFor(() => expect(fake.confirmArgs).toHaveLength(2));
    expect(fake.confirmArgs[1]?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    await waitFor(() => expect(fake.armArgs).toHaveLength(2));
    expect(fake.armArgs[1]?.command_id).not.toBe(fake.armArgs[0]?.command_id);
  });

  it("a refused code request shows its message and arms nothing", async () => {
    const fake = seeded();
    fake.fail("confirmCode", "refused", "the proposal is not open", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());
    expect((await startCard().findByRole("alert")).textContent).toBe("the proposal is not open");
    expect(fake.count("arm")).toBe(0);
  });

  it("a retry after unreachable on the code request reuses its command ID", async () => {
    const fake = seeded();
    fake.fail("confirmCode", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());
    expect((await startCard().findByRole("alert")).textContent).toBe("the server did not answer");

    await user.click(press());
    await waitFor(() => expect(fake.armArgs).toHaveLength(1));
    expect(fake.confirmArgs).toHaveLength(2);
    expect(fake.confirmArgs[1]?.command_id).toBe(fake.confirmArgs[0]?.command_id);
  });

  it("after unreachable on arm the code and the command ID stay: the next press retries arm only", async () => {
    const fake = seeded();
    fake.fail("arm", "unreachable", "the server did not answer", 1);
    const user = await bootChat(fake, "S-1", [card()]);
    await user.click(press());

    expect((await startCard().findByRole("alert")).textContent).toBe("the server did not answer");
    await user.click(press());
    await waitFor(() => expect(fake.armArgs).toHaveLength(2));
    expect(fake.armArgs[1]).toEqual(fake.armArgs[0]);
    expect(fake.confirmArgs).toHaveLength(1);
    await waitFor(() => expect(screen.getByTestId("start-outcome").textContent).toBe("Arming…"));
  });

  it("a replayed code answer has no code to show: it says so and asks for a new one next time", async () => {
    const fake = seeded();
    const user = await bootChat(fake, "S-1", [card()]);
    // The first answer is lost on the way: the server holds the code, the app got `unreachable`.
    fake.fail("confirmCode", "unreachable", "the server did not answer", 1);
    await user.click(press());
    await startCard().findByRole("alert");
    // The server did admit it, so the replay of the same command ID answers with no code.
    await act(async () => {
      await fake.confirmCode(fake.confirmArgs[0]!).catch(() => undefined);
    });
    await user.click(press());
    expect((await startCard().findByRole("alert")).textContent).toMatch(/cannot show it again/);
    expect(fake.count("arm")).toBe(0);
    await user.click(press());
    await waitFor(() => expect(fake.armArgs).toHaveLength(1));
    expect(fake.confirmArgs.at(-1)?.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
  });
});

describe("the end of a start card", () => {
  it("a withdrawn result shows Withdrawn: <reason> and no Start workflow", async () => {
    await bootChat(seeded(), "S-1", [card(), result(2, 1, { result: "withdrawn", reason: "halted", run: null, device: null })]);
    expect(screen.getByTestId("start-outcome").textContent).toBe("Withdrawn: halted");
    expect(screen.queryByTestId("start-workflow")).toBeNull();
  });

  it("a failed result shows Failed: <reason> and no Start workflow", async () => {
    await bootChat(seeded(), "S-1", [card(), result(2, 1, { result: "failed", reason: "the run could not be created", run: null, device: "D-1" })]);
    expect(screen.getByTestId("start-outcome").textContent).toBe("Failed: the run could not be created");
    expect(screen.queryByTestId("start-workflow")).toBeNull();
  });

  it("a result with no card held shows as an Answer entry", async () => {
    await bootChat(seeded(), "S-1", [result(5, 3)]);
    const log = within(main().getByRole("log", { name: "Messages" }));
    expect(log.getByRole("article", { name: "Answer" }).textContent).toMatch(/start card this app has not seen \(armed\)/);
    expect(screen.queryByTestId("start-card")).toBeNull();
  });

  it("an ended session shows no Start workflow", async () => {
    const fake = seeded();
    await bootChat(fake, "S-1", [card()]);
    expect(press()).toBeTruthy();
    await push(fake, frames.session(alpha({ ended_at: "2026-11-09T11:00:00Z", state: "idle" })));
    expect(screen.queryByTestId("start-workflow")).toBeNull();
    expect(fake.count("confirmCode")).toBe(0);
  });
});
