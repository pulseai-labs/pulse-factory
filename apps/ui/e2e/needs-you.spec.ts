// d23: answer the factory in the app. The Needs you page and the Settings rows in headless Chromium,
// through `pf-client-bridge`, against the real controller, API and engine on the paper env (`paper-d20`).
// `scripts/demo/ui-needs-you.sh` builds the env, starts everything and runs this file as its one journey,
// in both start modes: `cli` (the script arms the run) and `card` (w2's start card armed it first).
//
// The journey: answer the stop-rule question in two clicks, see the spine merge approval arrive with no
// read, confirm the merge in two clicks, watch the run wait and then finish, and set and unset a chat
// policy. Last it checks that no token, pairing code or confirm code is anywhere the page can see.
import type { RequestDetail, SettingsView } from "@pulse-factory/api-client";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

test.setTimeout(180_000);

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run this through scripts/demo/ui-needs-you.sh`);
  return value;
}

const HEX64 = /\b[0-9a-f]{64}\b/;
const MERGE_CARD_MS = 90_000;
const RUN_STATE_MS = 30_000;

test("answer the stop question, merge the spine PR, watch the run, set a chat policy", async ({ page }) => {
  const base = need("PF_BRIDGE_URL");
  const origin = new URL(base).origin;
  const code = need("PF_CODE");
  const bin = need("PF_BIN");
  need("PF_STATE_DIR");
  const deviceFile = path.join(need("PF_BRIDGE_STATE"), "device.json");
  const run = need("PF_D23_RUN");
  const closeFile = need("PF_D23_CLOSE_FILE");
  const projectId = need("PF_D23_PROJECT_ID");
  const projectName = process.env.PF_D23_PROJECT ?? "d23";
  const answer = need("PF_D23_ANSWER");

  const requests: string[] = [];
  const consoleText: string[] = [];
  const answers: string[] = [];
  const confirmCodes: string[] = [];
  let requestsCalls = 0;
  page.on("request", (r) => {
    requests.push(r.url());
    if (new URL(r.url()).pathname === "/bridge/verbs/requests") requestsCalls += 1;
  });
  page.on("console", (m) => consoleText.push(m.text()));
  page.on("pageerror", (e) => consoleText.push(String(e)));
  page.on("response", (r) => {
    const route = new URL(r.url()).pathname;
    // The stream never ends while the page is open: it carries no code and no token, and the script
    // reads the ledgers.
    if (!route.startsWith("/bridge/") || route === "/bridge/stream") return;
    r.text().then(
      (t) => {
        if (route === "/bridge/verbs/confirm_code") {
          const issued = (JSON.parse(t) as { ok: boolean; result?: { code?: string | null } }).result?.code;
          if (issued) confirmCodes.push(issued);
        } else {
          answers.push(t);
        }
      },
      () => {},
    );
  });

  const cli = <T>(...args: string[]): T =>
    JSON.parse(execFileSync(bin, args, { env: process.env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] })) as T;
  /** A field of a request as `show --json` prints it: in the body, or on the record itself. */
  const fieldOf = (id: string, name: string): string => {
    const detail = cli<RequestDetail & Record<string, unknown>>("show", id, "--json");
    const body = (detail.body ?? {}) as Record<string, unknown>;
    const value = body[name] ?? detail[name];
    if (typeof value !== "string") throw new Error(`show ${id} --json has no string ${name}`);
    return value;
  };

  const rail = (name: string) => page.getByRole("navigation").getByRole("button", { name });
  const main = page.getByRole("main");
  const openList = main.getByRole("list", { name: "Open requests" });
  const answeredList = main.getByRole("list", { name: "Answered" });
  const dialog = page.getByRole("dialog");
  const refresh = () => main.getByRole("button", { name: "Refresh" });
  /** The text of a fact (`<dt>` then `<dd>`) inside a locator. */
  const fact = (within: Locator, label: string) => within.locator(`dt:text-is("${label}") + dd`);

  async function openNeedsYou() {
    await rail("Needs you").click();
    await expect(main.getByRole("heading", { level: 2, name: "Needs you" })).toBeVisible();
  }

  // 1. Pair as "d23 browser" when the pair screen shows (the card journey paired already), open Needs you.
  await page.goto(base);
  const pairCode = page.getByLabel("Pairing code");
  await expect(pairCode.or(page.getByRole("navigation"))).toBeVisible();
  if (await pairCode.isVisible()) {
    await pairCode.fill(code);
    await page.getByLabel("Device name").fill("d23 browser");
    await page.getByRole("button", { name: "Pair" }).click();
    await expect(page.getByRole("heading", { name: /^Connected to 127\.0\.0\.1:\d+$/ })).toBeVisible();
    await page.getByRole("button", { name: "Open Pulse Factory" }).click();
  }
  await expect(page.getByRole("navigation")).toBeVisible();
  await openNeedsYou();

  // 2. The stop request's card shows (bounded): its question is the body's `stop_text`, and the badge counts it.
  const stopCard = openList.getByRole("article").first();
  await expect(stopCard).toBeVisible({ timeout: 60_000 });
  const stopId = ((await stopCard.getAttribute("aria-label")) ?? "").replace(/^Request /, "");
  expect(stopId).toMatch(/^R-\d+$/);
  await expect(fact(stopCard, "Kind")).toHaveText("stop");
  const stopText = fieldOf(stopId, "stop_text");
  expect(stopText.length).toBeGreaterThan(0);
  await expect(stopCard.getByTestId("stop-text")).toHaveText(stopText);
  await expect(rail("Needs you").locator(".rail-badge")).toHaveText("1");

  // 3. Type the answer, Send answer: the dialog shows the text and that no one holds the claim (this journey
  //    takes no CLI claim, and the card journey's device arm moves none); Send: the card leaves Open requests,
  //    shows in Answered, and the claim now names this device.
  await stopCard.getByLabel("Your answer").fill(answer);
  await stopCard.getByRole("button", { name: "Send answer" }).click();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText(answer, { exact: true })).toBeVisible();
  await expect(dialog.getByText("No one holds the claim")).toBeVisible();
  await dialog.getByRole("button", { name: "Send", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(openList.getByRole("article", { name: `Request ${stopId}` })).toHaveCount(0);
  await expect(answeredList.getByText(stopId, { exact: true })).toBeVisible();
  await expect(fact(main, "The claim")).toContainText("device D-1");
  const readsAfterAnswer = requestsCalls;

  // 4. Freshness (W1): with the page left open, the merge approval appears with no `requests` call: the
  //    stream's frame brought it.
  const mergeCard = openList.getByRole("article").first();
  await expect(fact(mergeCard, "Kind")).toHaveText("merge_ack", { timeout: MERGE_CARD_MS });
  const mergeId = ((await mergeCard.getAttribute("aria-label")) ?? "").replace(/^Request /, "");
  expect(mergeId).toMatch(/^R-\d+$/);
  expect(requestsCalls, "the merge card came by a read, not by the stream").toBe(readsAfterAnswer);

  // 5. The lid (G3): reload and open Needs you: the merge card is there first, and the stop request answered
  //    in step 3 is in Answered, filled from the `requests {all: true}` read alone. Before any click, the
  //    card's PR, Base and Head SHA are the body's.
  await page.reload();
  await expect(page.getByRole("navigation")).toBeVisible();
  await openNeedsYou();
  const merge = openList.getByRole("article", { name: `Request ${mergeId}` });
  await expect(merge).toBeVisible();
  await expect(openList.getByRole("article").first()).toHaveAttribute("aria-label", `Request ${mergeId}`);
  await expect(answeredList.getByText(stopId, { exact: true })).toBeVisible();
  const pr = fieldOf(mergeId, "pr");
  const mergeBase = fieldOf(mergeId, "base");
  const headSha = fieldOf(mergeId, "head_sha");
  const headBranch = fieldOf(mergeId, "head_branch");
  await expect(fact(merge, "PR")).toHaveText(pr);
  await expect(fact(merge, "Base")).toHaveText(mergeBase);
  await expect(fact(merge, "Head SHA")).toHaveText(headSha);

  // 6. Confirm merge: the dialog names the PR, base, head branch and head SHA of the body. Merge: the card shows
  //    the merge at that SHA and leaves Open requests.
  await merge.getByRole("button", { name: "Confirm merge" }).click();
  await expect(dialog).toBeVisible();
  await expect(fact(dialog, "PR")).toHaveText(pr);
  await expect(fact(dialog, "Base")).toHaveText(mergeBase);
  await expect(fact(dialog, "Head branch")).toHaveText(headBranch);
  await expect(fact(dialog, "Head SHA")).toHaveText(headSha);
  await dialog.getByRole("button", { name: "Merge", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(main.getByText(new RegExp(`Merged at ${headSha}`))).toBeVisible();
  await expect(openList.getByRole("article", { name: `Request ${mergeId}` })).toHaveCount(0);

  // 7. The waiting run (C12): Refresh, at most once a second and bounded, until the run shows what it waits
  //    for, and that text is one of the notes of `status`. Then the operator's close, and Refresh until the run
  //    shows it is done with the end reason of `status`.
  const runState = () => fact(main.getByRole("article", { name: `Run ${run}` }), "State");
  const settle = async (want: RegExp, within: number): Promise<string> => {
    let seen = "";
    await expect
      .poll(
        async () => {
          await refresh().click();
          seen = ((await runState().textContent({ timeout: 2_000 }).catch(() => null)) ?? "").trim();
          return want.test(seen);
        },
        { timeout: within, intervals: [1_000], message: "the run's state on Needs you" },
      )
      .toBe(true);
    return seen;
  };
  const waiting = (await settle(/^Waiting on you: /, RUN_STATE_MS)).replace(/^Waiting on you: /, "");
  const notes = cli<{ notes: string[] }>("status", run, "--json").notes;
  expect(notes, "the waiting text is one of status's notes").toContain(waiting);
  fs.mkdirSync(path.dirname(closeFile), { recursive: true });
  fs.writeFileSync(closeFile, "closed\n");
  const done = (await settle(/^Done: /, RUN_STATE_MS)).replace(/^Done: /, "");
  expect(done).toBe(cli<{ end_reason: string }>("status", run, "--json").end_reason);

  // 8. Settings: choose the project; chat.self_turn_cap shows its value from built-in; set it to 5 at this scope: the
  //    row shows 5 from the project, and the CLI reads it with the device that set it; unset it: the built-in again.
  await rail("Settings").click();
  await expect(main.getByRole("heading", { level: 2, name: "Settings" })).toBeVisible();
  const policies = main.getByRole("region", { name: "Chat policies" });
  await policies.getByLabel("Project", { exact: true }).selectOption({ label: projectName });
  const row = policies.getByRole("group", { name: "Setting chat.self_turn_cap" });
  await expect(row).toBeVisible();
  const builtIn = cli<SettingsView>("settings", "get", "chat.self_turn_cap", "--project", projectId, "--json").settings[0];
  expect(builtIn?.from.kind).toBe("built_in");
  await expect(fact(row, "From")).toHaveText("built-in");
  await expect(fact(row, "Value")).toHaveText(String(builtIn?.value));

  await row.getByLabel("New value").fill("5");
  await row.getByRole("button", { name: "Set at this scope" }).click();
  await expect(fact(row, "From")).toHaveText(`project ${projectName}`);
  await expect(fact(row, "Value")).toHaveText("5");
  const set = cli<SettingsView>("settings", "get", "chat.self_turn_cap", "--project", projectId, "--json").settings[0];
  expect(set?.value).toBe(5);
  expect(set?.from.kind).toBe("project");
  expect(set?.layers[0]?.set_by).toBe("D-1");

  await row.getByRole("button", { name: "Unset at this scope" }).click();
  await expect(fact(row, "From")).toHaveText("built-in");
  await expect(fact(row, "Value")).toHaveText(String(builtIn?.value));
  await expect(row.getByRole("button", { name: "Unset at this scope" })).toHaveCount(0);

  // 9. Secrets: every request went to the bridge's origin, and the device token, the pairing code and every
  //    confirm code that the bridge answered are in no storage, cookie, IndexedDB name, console line, request
  //    URL or the DOM. Each confirm code is in no bridge answer but its own `confirm_code` answer.
  const foreign = requests.filter((u) => !u.startsWith(`${origin}/`) && !u.startsWith("data:"));
  expect(foreign).toEqual([]);
  const token = (JSON.parse(fs.readFileSync(deviceFile, "utf8")) as { token: string }).token;
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  expect(confirmCodes.length, "the bridge answered a confirm code for the answer and for the merge").toBeGreaterThanOrEqual(2);
  for (const c of confirmCodes) expect(c).toMatch(/^pfc1-[0-9a-f]{32}$/);
  const kept = await visibleState(page);
  const dom = await page.content();
  const secrets: Array<[string, string]> = [
    ["the device token", token],
    ["the pairing code", code],
    ...confirmCodes.map((c): [string, string] => ["a confirm code", c]),
  ];
  const places: Array<[string, string]> = [
    ["storage, cookie or IndexedDB", kept],
    ["the console", consoleText.join("\n")],
    ["a request URL", requests.join("\n")],
    ["the DOM", dom],
  ];
  for (const [what, secret] of secrets) {
    for (const [where, text] of places) expect(text.includes(secret), `${what} is in ${where}`).toBe(false);
    expect(answers.join("\n").includes(secret), `${what} is in a bridge answer`).toBe(false);
  }
  expect(kept).not.toMatch(HEX64);
  expect(consoleText.join("\n")).not.toMatch(HEX64);
  expect(dom).not.toContain("pfc1-");
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
