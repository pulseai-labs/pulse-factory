// d23: answer the factory in the app. The Needs you page and the Settings rows in headless Chromium,
// through `pf-client-bridge`, against the real controller, API and engine on the paper env (`paper-d20`).
// `scripts/demo/ui-needs-you.sh` builds the env, starts everything and runs this file as its one journey,
// in both start modes: `cli` (the script arms the run) and `card` (w2's start card armed it first).
//
// The journey: answer the stop-rule question in one press, see the spine merge approval arrive with no
// read, approve the merge in one press, watch the run wait and then finish, and set and remove a chat
// policy. Each press asks for a fresh confirm code and spends it at once (r1.s5.w4): the page shows no
// dialog, and a second use of a spent code is refused. Last it checks that no token, pairing code or
// confirm code is anywhere the page can see.
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
  /** The device verbs the page called, in order: `confirm_code`, `answer` and `ack`. */
  const acts: string[] = [];
  let answerBody = "";
  let requestsCalls = 0;
  page.on("request", (r) => {
    requests.push(r.url());
    const route = new URL(r.url()).pathname;
    if (route === "/bridge/verbs/requests") requestsCalls += 1;
    const verb = /^\/bridge\/verbs\/(confirm_code|answer|ack)$/.exec(route)?.[1];
    if (verb !== undefined) {
      acts.push(verb);
      if (verb === "answer" && answerBody === "") answerBody = r.postData() ?? "";
    }
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
  const answeredList = main.getByRole("list", { name: "Answered today" });
  /** The "Answered today" row of a request. */
  const answeredRow = (id: string): Locator => answeredList.locator(`li[aria-label="Answered ${id}"]`);
  const dialog = page.getByRole("dialog");

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
    await expect(page.getByRole("heading", { name: /^Connected to .+/ })).toBeVisible();
    await page.getByRole("button", { name: "Open Pulse Factory" }).click();
  }
  await expect(page.getByRole("navigation")).toBeVisible();
  await openNeedsYou();

  // 2. The stop request's card shows (bounded): its question is the body's `stop_text`, and the badge counts it.
  const stopCard = openList.getByRole("article").first();
  await expect(stopCard).toBeVisible({ timeout: 60_000 });
  const stopId = ((await stopCard.getAttribute("aria-label")) ?? "").replace(/^Request /, "");
  expect(stopId).toMatch(/^R-\d+$/);
  await expect(stopCard).toHaveAttribute("data-kind", "stop");
  const stopText = fieldOf(stopId, "stop_text");
  expect(stopText.length).toBeGreaterThan(0);
  await expect(stopCard.getByTestId("stop-text")).toHaveText(stopText);
  await expect(rail("Needs you").locator(".ui-count")).toHaveText("1");

  // 3. Type the answer, press Send: one press, no dialog. It made exactly one `confirm_code` and one `answer`
  //    call; the card leaves Open requests and shows in Answered today with the answer and this device's name.
  //    A second use of the same code is refused as `code_used`.
  await stopCard.getByLabel("Your answer").fill(answer);
  expect(acts, "no call before the press").toEqual([]);
  await stopCard.getByRole("button", { name: "Send", exact: true }).click();
  await expect(openList.getByRole("article", { name: `Request ${stopId}` })).toHaveCount(0);
  await expect(dialog).toHaveCount(0);
  expect(acts, "one press: one confirm_code, then one answer").toEqual(["confirm_code", "answer"]);
  await expect(answeredRow(stopId)).toContainText(answer);
  await expect(answeredRow(stopId)).toContainText("d23 browser");
  expect(confirmCodes.length).toBe(1);
  const spent = JSON.parse(answerBody) as { request: string; rev: number; text: string; code: string; command_id: string };
  expect(spent.code, "the answer spent the code the page was given").toBe(confirmCodes[0]);
  const again = await page.request.post(`${origin}/bridge/verbs/answer`, {
    data: { ...spent, command_id: `d23-reuse-${Date.now()}` },
  });
  const refused = (await again.json()) as { ok: boolean; error?: { refusal?: string } };
  expect(refused.ok).toBe(false);
  expect(refused.error?.refusal, "a spent confirm code is refused").toBe("code_used");
  const readsAfterAnswer = requestsCalls;

  // 4. Freshness (W1): with the page left open, the merge approval appears with no `requests` call: the
  //    stream's frame brought it.
  const mergeCard = openList.getByRole("article").first();
  await expect(mergeCard).toHaveAttribute("data-kind", "merge_ack", { timeout: MERGE_CARD_MS });
  const mergeId = ((await mergeCard.getAttribute("aria-label")) ?? "").replace(/^Request /, "");
  expect(mergeId).toMatch(/^R-\d+$/);
  expect(requestsCalls, "the merge card came by a read, not by the stream").toBe(readsAfterAnswer);

  // 5. The lid (G3): reload and open Needs you: the merge card is there first, and the stop request answered
  //    in step 3 is in Answered today, filled from the `requests {all: true}` read and its `show`. Before any
  //    click, the card's PR, from-branch with its short head SHA, and into-branch are the body's.
  await page.reload();
  await expect(page.getByRole("navigation")).toBeVisible();
  await openNeedsYou();
  const merge = openList.getByRole("article", { name: `Request ${mergeId}` });
  await expect(merge).toBeVisible();
  await expect(openList.getByRole("article").first()).toHaveAttribute("aria-label", `Request ${mergeId}`);
  await expect(answeredRow(stopId)).toBeVisible();
  const pr = fieldOf(mergeId, "pr");
  const mergeBase = fieldOf(mergeId, "base");
  const headSha = fieldOf(mergeId, "head_sha");
  const headBranch = fieldOf(mergeId, "head_branch");
  const prNumber = /\/pull\/(\d+)$/.exec(pr)?.[1];
  await expect(merge.getByTestId("merge-pr")).toContainText(prNumber ?? pr);
  await expect(merge.getByTestId("merge-into")).toHaveText(mergeBase);
  await expect(merge.getByTestId("merge-from")).toHaveText(headBranch);
  await expect(merge.getByTestId("merge-sha")).toHaveText(headSha.slice(0, 7));
  await expect(merge.getByTestId("merge-sha")).toHaveAttribute("title", headSha);

  // 6. Approve merge: one press, no dialog. It made one `confirm_code` and one `ack` call; the `ack` answer names
  //    the PR, the base and the head SHA of the body; the card leaves Open requests and shows in Answered today
  //    as "Merge approved" by this device.
  const callsBeforeMerge = acts.length;
  const answersBeforeMerge = answers.length;
  await merge.getByRole("button", { name: "Approve merge" }).click();
  await expect(openList.getByRole("article", { name: `Request ${mergeId}` })).toHaveCount(0);
  await expect(dialog).toHaveCount(0);
  expect(acts.slice(callsBeforeMerge), "one press: one confirm_code, then one ack").toEqual(["confirm_code", "ack"]);
  await expect(answeredRow(mergeId)).toContainText("Merge approved");
  await expect(answeredRow(mergeId)).toContainText("d23 browser");
  await expect.poll(() => answers.length, { message: "the ack answer" }).toBeGreaterThan(answersBeforeMerge);
  const ackAnswer = answers
    .slice(answersBeforeMerge)
    .map((t) => JSON.parse(t) as { ok: boolean; result?: { merged?: boolean; head_sha?: string; pr?: string; base?: string } })
    .find((a) => a.result?.merged !== undefined);
  expect(ackAnswer?.ok).toBe(true);
  expect(ackAnswer?.result?.merged).toBe(true);
  expect(ackAnswer?.result?.head_sha).toBe(headSha);
  expect(ackAnswer?.result?.pr).toBe(pr);
  expect(ackAnswer?.result?.base).toBe(mergeBase);

  // 7. The waiting run (C12): the run's state is in the sidebar's "Workflow runs" row (G1 moved it from Needs you).
  //    Each poll opens Needs you and then Projects, so the sidebar mounts and reads `runList` again, at most once a
  //    second and bounded, until the run shows what it waits for, and that text is one of the notes of `status`.
  //    Then the operator's close, and the same poll until the run shows it ended with the end reason of `status`.
  const runRow = () => page.getByRole("region", { name: "Sidebar" }).locator(`li[aria-label="Run ${run}"] .sb-run-sub`);
  const settle = async (want: RegExp, within: number): Promise<string> => {
    let seen = "";
    await expect
      .poll(
        async () => {
          await rail("Needs you").click();
          await expect(main.getByRole("heading", { level: 2, name: "Needs you" })).toBeVisible();
          await rail("Projects").click();
          seen = ((await runRow().textContent({ timeout: 2_000 }).catch(() => null)) ?? "").trim();
          return want.test(seen);
        },
        { timeout: within, intervals: [1_000], message: "the run's state in the sidebar" },
      )
      .toBe(true);
    return seen;
  };
  const waiting = (await settle(/^Waiting: /, RUN_STATE_MS)).replace(/^Waiting: /, "");
  const notes = cli<{ notes: string[] }>("status", run, "--json").notes;
  expect(notes, "the waiting text is one of status's notes").toContain(waiting);
  fs.mkdirSync(path.dirname(closeFile), { recursive: true });
  fs.writeFileSync(closeFile, "closed\n");
  const done = (await settle(/^Ended: /, RUN_STATE_MS)).replace(/^Ended: /, "");
  expect(done).toBe(cli<{ end_reason: string }>("status", run, "--json").end_reason);

  // 8. Settings: choose "A project" and then the project's pill; chat.self_turn_cap shows its value from the
  //    built-in link of the chain; type 5 and press Enter at this scope: the row shows 5 from the project's link, and
  //    the CLI reads it with the device that set it; Remove this override: the built-in link again.
  await rail("Settings").click();
  await expect(main.getByRole("heading", { level: 2, name: "Settings" })).toBeVisible();
  const policies = main.getByRole("region", { name: "Chat policies" });
  await policies.getByRole("button", { name: "A project", exact: true }).click();
  await policies.getByRole("group", { name: "Project", exact: true }).getByRole("button", { name: projectName, exact: true }).click();
  const row = policies.getByRole("group", { name: "Setting chat.self_turn_cap" });
  await expect(row).toBeVisible();
  const live = row.getByRole("list", { name: "Comes from" }).locator('li[aria-current="true"] .st-node-label');
  const valueField = row.getByLabel("Value", { exact: true });
  const builtIn = cli<SettingsView>("settings", "get", "chat.self_turn_cap", "--project", projectId, "--json").settings[0];
  expect(builtIn?.from.kind).toBe("built_in");
  await expect(live).toHaveText("Built-in");
  await expect(valueField).toHaveValue(String(builtIn?.value));

  await valueField.fill("5");
  await valueField.press("Enter");
  await expect(live).toHaveText(projectName);
  await expect(valueField).toHaveValue("5");
  const set = cli<SettingsView>("settings", "get", "chat.self_turn_cap", "--project", projectId, "--json").settings[0];
  expect(set?.value).toBe(5);
  expect(set?.from.kind).toBe("project");
  expect(set?.layers[0]?.set_by).toBe("D-1");

  await row.getByRole("button", { name: "Remove this override" }).click();
  await expect(live).toHaveText("Built-in");
  await expect(valueField).toHaveValue(String(builtIn?.value));
  await expect(row.getByRole("button", { name: "Remove this override" })).toHaveCount(0);

  // 9. Secrets: every request went to the bridge's origin, and the device token, the pairing code and every
  //    confirm code that the bridge answered are in no storage, cookie, IndexedDB name, console line, request
  //    URL or the DOM. Each confirm code is in no bridge answer but its own `confirm_code` answer.
  const foreign = requests.filter((u) => !u.startsWith(`${origin}/`) && !u.startsWith("data:"));
  expect(foreign).toEqual([]);
  const token = (JSON.parse(fs.readFileSync(deviceFile, "utf8")) as { token: string }).token;
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  expect(confirmCodes.length, "the bridge answered one confirm code for each press: the answer and the merge").toBe(2);
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
  // `pf.server-name` is the last name an answer gave, a label (r1.s6.w1, P1); `pf.dock` is written only when the dock changes.
  expect([...local].sort()).toEqual(["pf.server-name", "pf.sidebar"]);
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
