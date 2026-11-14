// The start card's journey (r1.s4.w2, Approach 13a; r1.s5.w3, Approach 8: the one-press card) in headless Chromium, through `pf-client-bridge`,
// against the real controller, the real Claude Code adapter and `pf-claude-replay` (the chat's wrapper of
// `scripts/demo/start-env.sh`). `scripts/demo/ui-start.sh` sets the environment and starts everything;
// w3's d23 runs this file on the merged tree. It holds exactly one `test(`.
//
// It reads only: PF_BRIDGE_URL, PF_CODE, PF_BIN, PF_STATE_DIR, PF_E2E_OUT and PF_D23_PROJECT (the
// project's name, as the sidebar shows it). It writes nothing outside PF_E2E_OUT.
import type { ChatItemRecord, SessionList } from "@pulse-factory/api-client";
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { modelName } from "../src/ui/display-names";

test.setTimeout(120_000);

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run this through scripts/demo/ui-start.sh`);
  return value;
}

/** The thirteen groups of the old card (G2): none is on the card any more. */
const OLD_GROUPS = [
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
  "Ledger",
  "Parameters",
];

test("start a spine from the Orchestrator chat: card, one press, outcome, reload", async ({ page }) => {
  const base = need("PF_BRIDGE_URL");
  const origin = new URL(base).origin;
  const code = need("PF_CODE");
  const bin = need("PF_BIN");
  need("PF_STATE_DIR");
  need("PF_E2E_OUT");
  const stateDir = need("PF_STATE_DIR");
  const project = need("PF_D23_PROJECT");

  const requests: string[] = [];
  const consoleText: string[] = [];
  /** Every POST to a bridge verb, in order: the verb and its body (kept in memory only). */
  const posted: Array<{ verb: string; body: string | null }> = [];
  page.on("request", (r) => {
    requests.push(r.url());
    const verb = /\/bridge\/verbs\/([a-z_]+)$/.exec(new URL(r.url()).pathname)?.[1];
    if (r.method() === "POST" && verb) posted.push({ verb, body: r.postData() });
  });
  page.on("console", (m) => consoleText.push(m.text()));
  page.on("pageerror", (e) => consoleText.push(String(e)));

  const cli = <T>(...args: string[]): T =>
    JSON.parse(execFileSync(bin, args, { env: process.env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] })) as T;
  /** The run that `status` names, or null when none exists (`status` exits non-zero then). */
  const statusRun = (): { run: string; project_id: string } | null => {
    try {
      return cli<{ run: string; project_id: string }>("status", "--json");
    } catch {
      return null;
    }
  };
  const sessionList = () => cli<SessionList>("session", "list", "--json");
  const mySession = () => {
    const found = sessionList().sessions.find((s) => s.project_name === project && s.agent_id === "orchestrator" && s.ended_at === null);
    if (!found) throw new Error(`no live Orchestrator session of ${project}`);
    return found;
  };

  const sidebarRow = () =>
    page.getByRole("region", { name: "Sidebar" }).getByRole("button", { name: new RegExp(`session in ${project}:`) });
  const card = () => page.getByTestId("start-card");
  const messageBox = () => page.getByRole("textbox", { name: "Message Orchestrator" });
  const outcome = () => page.getByTestId("start-outcome");

  // 1. Pair as "d23 browser", if the pair screen shows.
  await page.goto(base);
  const pairing = page.getByLabel("Pairing code");
  const nav = page.getByRole("navigation");
  await expect(pairing.or(nav)).toBeVisible();
  if (await pairing.isVisible()) {
    await pairing.fill(code);
    await page.getByLabel("Device name").fill("d23 browser");
    await page.getByRole("button", { name: "Pair" }).click();
    await expect(page.getByRole("heading", { name: /^Connected to .+/ })).toBeVisible();
    await page.getByRole("button", { name: "Open Pulse Factory" }).click();
  }
  await expect(nav).toBeVisible();
  await expect(page.getByRole("region", { name: "Sidebar" }).getByText(project, { exact: true })).toBeVisible();

  // 2. Open the project's Orchestrator session: the live one, or a new one from the Agents page.
  if ((await sidebarRow().count()) > 0) {
    await sidebarRow().first().click();
  } else {
    await page.getByRole("navigation").getByRole("button", { name: "Agents" }).click();
    await page
      .getByRole("list", { name: "Sessions by project" })
      .getByRole("listitem")
      .filter({ hasText: project })
      .getByRole("button", { name: "Start session" })
      .click();
  }
  await expect(page.getByRole("group", { name: "Project", exact: true }).first()).toHaveText(project);

  // 3. One message. The chat's wrapper proposes the start once that message is held.
  await messageBox().fill("start the spine s1 of this project");
  await page.getByRole("button", { name: "Send" }).click();

  // 4. The card, with the groups the plan has: Limits, Most sessions, the merge line, the work items and who
  //    runs it (a group with no value is left out), and Paper env (the paper spine has one). The thirteen
  //    groups of the old card are gone. `Most sessions` is the card item's own number.
  await expect(card()).toBeVisible({ timeout: 60_000 });
  await expect(card()).toHaveAttribute("data-proposal", /^sp-[0-9a-f]{16}$/);
  const session = mySession();
  const held = cli<{ items: ChatItemRecord[] }>("session", "items", session.id, "--json").items;
  const item = held.find((i) => i.body.kind === "start_card");
  if (!item || item.body.kind !== "start_card") throw new Error(`${session.id} holds no start_card item`);
  const plan = item.body.summary;
  const present: Record<string, boolean> = {
    Limits: true,
    "Most sessions": plan.most_launches !== null,
    "Merge line": plan.merges.length > 0,
    "Work items": plan.items !== null,
    "Who runs it": plan.roles.length > 0,
    "Paper env": plan.paper !== null && plan.paper !== "",
  };
  for (const [name, here] of Object.entries(present)) {
    await expect(card().getByRole("group", { name, exact: true }), name).toHaveCount(here ? 1 : 0);
  }
  for (const name of ["Limits", "Most sessions", "Merge line", "Who runs it", "Paper env"]) {
    expect(present[name], `the paper spine's card has a ${name} group`).toBe(true);
  }
  expect(plan.most_launches).not.toBeNull();
  await expect(card().getByRole("group", { name: "Most sessions", exact: true })).toContainText(String(plan.most_launches));
  const who = card().getByRole("group", { name: "Who runs it", exact: true });
  await expect(who.getByRole("listitem")).toHaveCount(plan.roles.length);
  // The rows come in the plan's order: row i names role i and the display name of its model.
  for (const [i, role] of plan.roles.entries()) {
    await expect(who.getByRole("listitem").nth(i).locator(".role-name")).toHaveText(role.name);
    await expect(who.getByRole("listitem").nth(i)).toContainText(modelName(role.model));
  }
  for (const merge of plan.merges) {
    await expect(card().getByRole("group", { name: "Merge line", exact: true })).toContainText(merge.sentence);
  }
  for (const name of OLD_GROUPS) {
    await expect(card().getByRole("group", { name, exact: true }), name).toHaveCount(0);
  }

  // 5. Before any press: the card says nothing launches until the operator starts it, shows no dialog and
  //    none of the old two buttons, CLI and Change something are disabled with the Later release tag, and
  //    nothing is armed.
  const before = statusRun();
  await expect(card().getByText("Nothing launches until you start it.")).toBeVisible();
  await expect(page.getByTestId("start-dialog")).toHaveCount(0);
  await expect(card().getByRole("button", { name: /confirm start|arm the run/i })).toHaveCount(0);
  for (const label of ["CLI", "Change something"]) {
    const later = card().locator("[data-not-built]").filter({ hasText: label });
    await expect(later, label).toHaveAttribute("aria-disabled", "true");
    await expect(later, label).toContainText("Later release");
  }
  expect(statusRun()).toEqual(before);

  // 6. One press on Start workflow: the page asks for the confirm code once and then arms once.
  const mark = posted.length;
  await card().getByTestId("start-workflow").click();
  await expect(outcome()).toBeVisible({ timeout: 30_000 });
  const asked = posted.slice(mark).filter((r) => r.verb === "confirm_code" || r.verb === "arm");
  expect(asked.map((r) => r.verb)).toEqual(["confirm_code", "arm"]);
  const armBody = asked[1]?.body ?? null;
  if (armBody === null) throw new Error("the page sent no body with its arm call");

  // 7. The outcome says Started, names the run and the device, and `status` names the same run in the
  //    project. Start workflow is gone. The ledger holds one confirm_code line and one ok arm line of that
  //    device. The spent code: the captured arm call, posted again with a new command ID, is refused.
  await expect(outcome()).toContainText("Started", { timeout: 30_000 });
  const run = await outcome().getAttribute("data-run");
  const device = await outcome().getAttribute("data-device");
  if (!run) throw new Error("the outcome names no run");
  expect(device).toMatch(/^D-\d+$/);
  const now = statusRun();
  expect(now?.run).toBe(run);
  expect(now?.project_id).toBe(session.project_id);
  await expect(card().getByTestId("start-workflow")).toHaveCount(0);
  const apiLines = fs.readFileSync(path.join(stateDir, "api.ledger"), "utf8").split("\n");
  expect(apiLines.filter((l) => l.includes(`verb=confirm_code device=${device} `))).toHaveLength(1);
  expect(apiLines.filter((l) => l.includes(`verb=arm device=${device} `) && l.includes("outcome=ok"))).toHaveLength(1);
  const replay = await page.request.post(`${origin}/bridge/verbs/arm`, {
    data: { ...(JSON.parse(armBody) as Record<string, unknown>), command_id: randomUUID() },
  });
  expect(((await replay.json()) as { ok: boolean }).ok).toBe(false);
  expect(statusRun()?.run).toBe(run);

  // 8. After a reload the card shows the same Started outcome, and no Start workflow.
  await page.reload();
  await expect(nav).toBeVisible();
  await sidebarRow().first().click();
  await expect(card()).toBeVisible();
  await expect(outcome()).toContainText("Started");
  await expect(outcome()).toHaveAttribute("data-run", run);
  await expect(outcome()).toHaveAttribute("data-device", device ?? "");
  await expect(card().getByTestId("start-workflow")).toHaveCount(0);

  // 9. Every request went to the bridge's origin, and no confirm code or pairing code is anywhere the
  //    page keeps or logs: not in the console, a URL, the DOM or the page's storage.
  const foreign = requests.filter((u) => !u.startsWith(`${origin}/`) && !u.startsWith("data:"));
  expect(foreign).toEqual([]);
  const kept = await visibleState(page);
  const dom = await page.content();
  const confirmCode = /pfc1-[0-9a-f]{32}/;
  for (const [where, text] of [
    ["storage, cookie or IndexedDB", kept],
    ["the console", consoleText.join("\n")],
    ["a request URL", requests.join("\n")],
    ["the DOM", dom],
  ] as const) {
    expect(text.includes(code), `the pairing code is in ${where}`).toBe(false);
    expect(confirmCode.test(text), `a confirm code is in ${where}`).toBe(false);
  }
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
