// d19: the Orchestrator chat in headless Chromium, through `pf-client-bridge`, against the real
// controller, the real Claude Code adapter and `pf-claude-replay` (the `claude-binary` fake). The UI
// goes through the bridge, not the Tauri app (the `desktop-bridge` fake). `scripts/demo/ui-chat.sh`
// sets the environment and starts everything; this file is its one journey.
//
// alpha plays `permissions.jsonl` (a `Bash` approval and a deny) and beta plays `artifacts_on.jsonl`
// (two turns with an effort change between them). The replay never answers `get_context_usage`, and
// the controller's pump calls it after each turn result, so that session's pump waits out 30 s after
// every turn: every assertion on a second turn of one session is bounded at 60 s.
import type { ChatItemRecord, SessionList } from "@pulse-factory/api-client";
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { dollarsTitle, formatWu, weightedUnits } from "../src/components/cost";

test.setTimeout(120_000);

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run this through scripts/demo/ui-chat.sh`);
  return value;
}

const HEX64 = /\b[0-9a-f]{64}\b/;
const SECOND_TURN_MS = 60_000;

/** The label of the entry that the chat shows for each kind of stored item. */
const ENTRY: Record<string, string> = {
  operator_message: "Operator message",
  assistant_text: "Reply",
  tool_call: "Tool call",
  approval_request: "Approval request",
  question_request: "Question",
  background_task: "Background task",
  notice: "Notice",
  turn_result: "Turn result",
  redacted: "Redacted item",
  unknown: "Unsupported item",
};
// The answer items render inside their cards, as no entry of their own.
const INSIDE_A_CARD = ["approval_answer", "question_answer"];

test("chat with the Orchestrator of two projects: approve, reload, set, switch branch", async ({ page }) => {
  const base = need("PF_BRIDGE_URL");
  const origin = new URL(base).origin;
  const code = need("PF_CODE");
  const bin = need("PF_BIN");
  need("PF_STATE_DIR");
  const deviceFile = path.join(need("PF_BRIDGE_STATE"), "device.json");
  const alphaDir = fs.realpathSync(need("PF_ALPHA_DIR"));
  const askAlpha = need("PF_D19_ASK_ALPHA");
  const askBeta1 = need("PF_D19_ASK_BETA_1");
  const askBeta2 = need("PF_D19_ASK_BETA_2");

  const requests: string[] = [];
  const consoleText: string[] = [];
  const answers: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
  page.on("console", (m) => consoleText.push(m.text()));
  page.on("pageerror", (e) => consoleText.push(String(e)));
  page.on("response", (r) => {
    const route = new URL(r.url()).pathname;
    // The chat stream never ends while the page is open: `ui-chat.sh` reads it on its own.
    if (route.startsWith("/bridge/") && route !== "/bridge/stream") {
      r.text().then(
        (t) => answers.push(t),
        () => {},
      );
    }
  });

  const cli = <T>(...args: string[]): T =>
    JSON.parse(execFileSync(bin, args, { env: process.env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] })) as T;
  const sessionList = () => cli<SessionList>("session", "list", "--json");

  const fact = (name: string) => page.getByRole("group", { name, exact: true });
  const rail = (name: string) => page.getByRole("navigation").getByRole("button", { name });
  const sidebarRow = (project: string) =>
    page.getByRole("region", { name: "Sidebar" }).getByRole("button", { name: new RegExp(`session in ${project}:`) });
  const projectRow = (project: string) =>
    page.getByRole("list", { name: "Sessions by project" }).getByRole("listitem").filter({ hasText: project });
  const messageBox = () => page.getByRole("textbox", { name: "Message Orchestrator" });
  const entries = (name: string) => page.getByRole("log", { name: "Messages" }).getByRole("article", { name });
  const control = (label: string) => page.getByLabel(label, { exact: true });

  async function openFromSidebar(project: string) {
    await sidebarRow(project).click();
    await expect(fact("Project")).toHaveText(project);
  }

  // 1. Pair as "d19 browser" and open Pulse Factory.
  await page.goto(base);
  await page.getByLabel("Pairing code").fill(code);
  await page.getByLabel("Device name").fill("d19 browser");
  await page.getByRole("button", { name: "Pair" }).click();
  await expect(page.getByRole("heading", { name: /^Connected to 127\.0\.0\.1:\d+$/ })).toBeVisible();
  await page.getByRole("button", { name: "Open Pulse Factory" }).click();
  await expect(page.getByRole("navigation")).toBeVisible();
  await expect(page.getByRole("region", { name: "Sidebar" }).getByText("alpha", { exact: true })).toBeVisible();

  // 2. Agents: the Orchestrator shows effort `high`; alpha and beta each show Start session.
  await rail("Agents").click();
  const main = page.getByRole("main");
  await expect(main.getByRole("heading", { name: "Orchestrator" })).toBeVisible();
  await expect(main.getByRole("list", { name: "Defaults" }).getByText("high", { exact: true })).toBeVisible();
  for (const project of ["alpha", "beta"]) {
    await expect(projectRow(project).getByRole("button", { name: "Start session" })).toBeVisible();
  }

  // 3. Start session for alpha: the header names the project, the worktree, the branch and the cost.
  await projectRow("alpha").getByRole("button", { name: "Start session" }).click();
  await expect(fact("Project")).toHaveText("alpha");
  await expect(fact("Worktree")).toHaveAttribute("title", alphaDir);
  await expect(fact("Worktree")).toHaveText(path.basename(alphaDir));
  await expect(fact("Branch")).toHaveText("main");
  await expect(page.getByRole("group", { name: "Chat header" }).getByText("unqualified")).toBeVisible();
  await expect(fact("Cost")).toHaveText("0 wu");
  await expect(page.getByRole("tab", { name: "Orchestrator · alpha" })).toBeVisible();

  // 4. The permission mode is `manual` (the recording's order), and a question brings the approval.
  await control("Permission mode").selectOption("manual");
  await expect(control("Permission mode")).toHaveValue("manual");
  await messageBox().fill(askAlpha);
  await page.getByRole("button", { name: "Send" }).click();
  const approval = page.getByRole("article", { name: "Approval request" });
  await expect(approval).toBeVisible();
  await expect(approval.getByText("Bash", { exact: true })).toBeVisible();
  await expect(approval.getByRole("button", { name: "Allow" })).toBeVisible();
  await expect(fact("State")).toHaveText("Needs you");

  // 5. Reload, open alpha's chat from its sidebar row: the same items, and the card is still pending (C2).
  await page.reload();
  await expect(page.getByRole("navigation")).toBeVisible();
  await expect(page.getByRole("tab")).toHaveCount(0);
  await openFromSidebar("alpha");
  await expect(approval).toBeVisible();
  await expect(approval.getByRole("button", { name: "Allow" })).toBeVisible();
  await expect(approval.getByRole("button", { name: "Deny" })).toBeVisible();
  await expect(entries("Operator message")).toHaveCount(1);
  await expect(entries("Operator message")).toContainText(askAlpha);
  await expect(fact("State")).toHaveText("Needs you");

  // 6. Deny with the message `no` (the recording was made with a deny).
  await approval.getByRole("textbox", { name: "Message for a denial" }).fill("no");
  await approval.getByRole("button", { name: "Deny" }).click();
  await expect(approval.getByText("Denied", { exact: true })).toBeVisible();
  await expect(approval.getByText("no", { exact: true })).toBeVisible();
  await expect(approval.getByRole("button", { name: "Allow" })).toHaveCount(0);
  await expect(entries("Reply").first()).toBeVisible();
  await expect(entries("Turn result")).toHaveCount(1);
  // The cost is the weighted units of the session record that the CLI prints, and the title holds the dollars.
  await expect(fact("Cost")).not.toHaveText("0 wu");
  const first = sessionList().sessions.find((s) => s.id === "S-1");
  if (!first) throw new Error("session list --json has no S-1");
  expect(first.usage.cost_micros).toBeGreaterThan(0);
  await expect(fact("Cost")).toHaveText(formatWu(weightedUnits(first.usage, first.cache_ttl_seconds)));
  await expect(fact("Cost")).toHaveAttribute("title", dollarsTitle(first.usage));

  // 7. Agents, then Start session for beta: a reply, an effort change that reaches the live process,
  //    and a second reply. The pump of a session waits 30 s after every turn (the replay answers no
  //    `get_context_usage`), so the second turn's wait is bounded at 60 s.
  await rail("Agents").click();
  await projectRow("beta").getByRole("button", { name: "Start session" }).click();
  await expect(fact("Project")).toHaveText("beta");
  await messageBox().fill(askBeta1);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(entries("Reply").first()).toBeVisible();
  await expect(entries("Turn result")).toHaveCount(1);
  await control("Effort").selectOption("medium");
  await expect(control("Effort")).toHaveValue("medium", { timeout: SECOND_TURN_MS });
  await messageBox().fill(askBeta2);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(entries("Turn result")).toHaveCount(2, { timeout: SECOND_TURN_MS });
  await expect(entries("Reply")).toHaveCount(2);
  await expect(entries("Notice")).toHaveCount(0);
  const beta = cli<{ items: ChatItemRecord[] }>("session", "items", "S-2", "--json").items;
  expect(beta.filter((i) => i.body.kind === "notice" && i.body.code === "option_restart")).toEqual([]);

  // 8. The change is that chat's only: alpha's composer, the Agents page and the agent keep `high`.
  await page.getByRole("tab", { name: "Orchestrator · alpha" }).click();
  await expect(fact("Project")).toHaveText("alpha");
  await expect(control("Effort")).toHaveValue("high");
  await rail("Agents").click();
  await expect(main.getByRole("list", { name: "Defaults" }).getByText("high", { exact: true })).toBeVisible();
  await expect(main.getByRole("list", { name: "Defaults" }).getByText("medium", { exact: true })).toHaveCount(0);
  const after = sessionList();
  expect(after.sessions.find((s) => s.id === "S-1")?.effort).toBe("high");
  expect(after.sessions.find((s) => s.id === "S-2")?.effort).toBe("medium");
  expect(after.agents.find((a) => a.id === "orchestrator")?.effort).toBe("high");

  // 9. A new branch in alpha, then a reload: the header reads it live, and the items are the stored ones.
  execFileSync("git", ["-C", alphaDir, "switch", "-c", "d19-branch"], { stdio: "ignore" });
  await page.reload();
  await expect(page.getByRole("navigation")).toBeVisible();
  await openFromSidebar("alpha");
  await expect(fact("Branch")).toHaveText("d19-branch");
  const stored = cli<{ items: ChatItemRecord[] }>("session", "items", "S-1", "--json").items;
  const expected = stored.filter((i) => !INSIDE_A_CARD.includes(i.body.kind)).map((i) => ENTRY[i.body.kind] ?? i.body.kind);
  expect(expected.length).toBeGreaterThan(3);
  await expect
    .poll(
      () =>
        page
          .getByRole("log", { name: "Messages" })
          .getByRole("article")
          .evaluateAll((nodes) => nodes.map((n) => n.getAttribute("aria-label"))),
      { timeout: 15_000, message: "the chat shows the stored items, in order" },
    )
    .toEqual(expected);

  // 10. Every request went to the bridge's origin, and no token or code is anywhere the page can see.
  const foreign = requests.filter((u) => !u.startsWith(`${origin}/`) && !u.startsWith("data:"));
  expect(foreign).toEqual([]);
  const token = (JSON.parse(fs.readFileSync(deviceFile, "utf8")) as { token: string }).token;
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  const kept = await visibleState(page);
  const dom = await page.content();
  for (const [where, text] of [
    ["storage, cookie or IndexedDB", kept],
    ["the console", consoleText.join("\n")],
    ["the DOM", dom],
    ["a bridge answer", answers.join("\n")],
  ] as const) {
    expect(text.includes(token), `the token is in ${where}`).toBe(false);
    expect(text.includes(code), `the code is in ${where}`).toBe(false);
  }
  expect(kept).not.toMatch(HEX64);
  expect(consoleText.join("\n")).not.toMatch(HEX64);
  expect(answers.length).toBeGreaterThan(0);
  const local = await page.evaluate(() => Object.keys(localStorage));
  expect(local).toEqual(["pf.sidebar"]);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
});

async function visibleState(page: Page): Promise<string> {
  return page.evaluate(async () => {
    const dbs = (await indexedDB.databases()).map((d) => d.name ?? "");
    return JSON.stringify({
      local: { ...localStorage },
      session: { ...sessionStorage },
      cookie: document.cookie,
      dbs,
    });
  });
}
