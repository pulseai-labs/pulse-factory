// The start card's journey (r1.s4.w2, Approach 13a) in headless Chromium, through `pf-client-bridge`,
// against the real controller, the real Claude Code adapter and `pf-claude-replay` (the chat's wrapper of
// `scripts/demo/start-env.sh`). `scripts/demo/ui-start.sh` sets the environment and starts everything;
// w3's d23 runs this file on the merged tree. It holds exactly one `test(`.
//
// It reads only: PF_BRIDGE_URL, PF_CODE, PF_BIN, PF_STATE_DIR, PF_E2E_OUT and PF_D23_PROJECT (the
// project's name, as the sidebar shows it). It writes nothing outside PF_E2E_OUT.
import type { ChatItemRecord, SessionList } from "@pulse-factory/api-client";
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";

test.setTimeout(120_000);

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run this through scripts/demo/ui-start.sh`);
  return value;
}

const FACTS = [
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
];

test("start a spine from the Orchestrator chat: card, cancel, arm, reload", async ({ page }) => {
  const base = need("PF_BRIDGE_URL");
  const origin = new URL(base).origin;
  const code = need("PF_CODE");
  const bin = need("PF_BIN");
  need("PF_STATE_DIR");
  need("PF_E2E_OUT");
  const project = need("PF_D23_PROJECT");

  const requests: string[] = [];
  const consoleText: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
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
    await expect(page.getByRole("heading", { name: /^Connected to 127\.0\.0\.1:\d+$/ })).toBeVisible();
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

  // 4. The card, with every fact group the plan has (a group with no value is left out: the paper
  //    spine has no plugin and no base push). `Most launches` is the card item's own number.
  await expect(card()).toBeVisible({ timeout: 60_000 });
  await expect(card()).toHaveAttribute("data-proposal", /^sp-[0-9a-f]{16}$/);
  const session = mySession();
  const held = cli<{ items: ChatItemRecord[] }>("session", "items", session.id, "--json").items;
  const item = held.find((i) => i.body.kind === "start_card");
  if (!item || item.body.kind !== "start_card") throw new Error(`${session.id} holds no start_card item`);
  const plan = item.body.summary;
  const present: Record<string, boolean> = {
    Spine: plan.spine !== "",
    Project: item.body.project_name !== "",
    Items: plan.items !== null,
    Workflow: true,
    Roles: plan.roles.length > 0,
    Caps: true,
    Plugins: plan.plugins.length > 0,
    "Most launches": plan.most_launches !== null,
    Merges: plan.merges.length > 0,
    "Base pushes": plan.base_pushes.length > 0,
    Forge: plan.forge_repo !== "",
    "Paper env": plan.paper !== null && plan.paper !== "",
    Ledger: item.body.ledger !== "",
    Parameters: Object.keys(item.body.params).length > 0,
  };
  for (const name of FACTS) {
    await expect(card().getByRole("group", { name, exact: true }), name).toHaveCount(present[name] ? 1 : 0);
  }
  for (const name of ["Spine", "Roles", "Caps", "Merges", "Most launches", "Paper env", "Ledger", "Parameters"]) {
    expect(present[name], `the paper spine's card has a ${name} fact`).toBe(true);
  }
  expect(plan.most_launches).not.toBeNull();
  await expect(card().getByRole("group", { name: "Most launches", exact: true }).locator("p")).toHaveText(
    String(plan.most_launches),
  );

  // 5. Confirm start, then Cancel: nothing is armed.
  const before = statusRun();
  await card().getByRole("button", { name: "Confirm start" }).click();
  const dialog = page.getByTestId("start-dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toHaveCount(0);
  expect(statusRun()).toEqual(before);

  // 6. Confirm start, then Arm the run.
  await card().getByRole("button", { name: "Confirm start" }).click();
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Arm the run" }).click();

  // 7. The outcome names the run and the device, and `status` names the same run in the project.
  await expect(outcome()).toHaveText(/^Armed: run \S+ from D-\d+$/, { timeout: 30_000 });
  const armed = /^Armed: run (\S+) from (D-\d+)$/.exec((await outcome().textContent()) ?? "");
  if (!armed) throw new Error("the outcome did not name a run and a device");
  const now = statusRun();
  expect(now?.run).toBe(armed[1]);
  expect(now?.project_id).toBe(session.project_id);
  await expect(card().getByRole("button", { name: "Confirm start" })).toHaveCount(0);

  // 8. A reload shows the card armed, with no Confirm start.
  await page.reload();
  await expect(nav).toBeVisible();
  await sidebarRow().first().click();
  await expect(card()).toBeVisible();
  await expect(outcome()).toHaveText(`Armed: run ${armed[1]} from ${armed[2]}`);
  await expect(card().getByRole("button", { name: "Confirm start" })).toHaveCount(0);

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
