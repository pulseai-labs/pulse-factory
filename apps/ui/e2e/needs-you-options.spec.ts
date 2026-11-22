// d33: take the options of every decision in the app. The Needs you page in headless Chromium, through
// `pf-client-bridge`, against the real controller, API and engine on the paper package `paper-options`.
// `scripts/demo/ui-needs-you-options.sh` builds the env, arms the run with the CLI, starts everything and runs
// this file as its one journey. The page's `Notification` is replaced by a recorder (permission granted) before the
// app loads: the bridge shows no notification of the Mac, and d34's walk proves the Tauri glue.
//
// The journey, in four steps (the script prints the `step <n>:` lines, and reads the server's side after):
//   1. the stop: QUESTION, the controller's summary, Asked by, Attempt, two options from the seat with the second
//      recommended, "Another answer"; one notification whose body ends with the request's ID; one press on the
//      recommended option answers it, and the card becomes a row that reads `Pressed` and the option's text;
//   2. the park: PARKED, the cause `no_done_signal`, the attempt, Parked by the controller, About the verifier's
//      seat, one Resume from the controller with no recommendation, "Note for the next seat"; a note that holds
//      `with care` and one press on Resume (the package's verifier passes only when it reads the note);
//   3. the red gate: RED GATE, the gate's reason, two options from the workflow, the first recommended with
//      `The workflow marked this option.` and the line `Answers, then resumes at` its stage; one press takes it;
//   4. the merge approval: MERGE APPROVAL, the summary and no recommended mark; one press on Approve merge.
// Each press makes exactly two calls: `confirm_code`, then the act. Last it checks that no token, pairing code or
// confirm code is anywhere the page can see, and that no request left the bridge's origin.
import type { RequestDetail } from "@pulse-factory/api-client";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

test.setTimeout(55_000);

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run this through scripts/demo/ui-needs-you-options.sh`);
  return value;
}

const HEX64 = /\b[0-9a-f]{64}\b/;
const CARD_MS = 25_000;

declare global {
  interface Window {
    __notifications: Array<{ title: string; body: string }>;
  }
}

test("take the stop's option, resume the park with a note, take the red gate's option, approve the merge", async ({ page }) => {
  const base = need("PF_BRIDGE_URL");
  const origin = new URL(base).origin;
  const code = need("PF_CODE");
  const bin = need("PF_BIN");
  need("PF_STATE_DIR");
  const deviceFile = path.join(need("PF_BRIDGE_STATE"), "device.json");
  const arm = need("PF_D33_ARM");
  const note = need("PF_D33_NOTE");
  expect(note, "the verifier of the package passes only on a note that holds these words").toContain("with care");

  // The recorder: a `Notification` that keeps what it was given and is granted. It is in place before the app loads.
  await page.addInitScript(() => {
    window.__notifications = [];
    class Recorder {
      static permission = "granted";
      static requestPermission = () => Promise.resolve("granted");
      onclick: ((event: unknown) => void) | null = null;
      constructor(title: string, options?: { body?: string }) {
        window.__notifications.push({ title, body: options?.body ?? "" });
      }
      close(): void {}
    }
    Object.defineProperty(window, "Notification", { value: Recorder, configurable: true, writable: true });
  });

  const requests: string[] = [];
  const consoleText: string[] = [];
  const answers: string[] = [];
  const confirmCodes: string[] = [];
  /** The device verbs the page called, in order: `confirm_code`, `answer`, `ack` and `notice_ack`. */
  const acts: string[] = [];
  page.on("request", (r) => {
    requests.push(r.url());
    const verb = /^\/bridge\/verbs\/(confirm_code|answer|ack|notice_ack)$/.exec(new URL(r.url()).pathname)?.[1];
    if (verb !== undefined) acts.push(verb);
  });
  page.on("console", (m) => consoleText.push(m.text()));
  page.on("pageerror", (e) => consoleText.push(String(e)));
  page.on("response", (r) => {
    const route = new URL(r.url()).pathname;
    // The stream never ends while the page is open: it carries no code and no token, and the script reads the ledgers.
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
  const shown = (id: string) => cli<RequestDetail>("show", id, "--json");

  const rail = (name: string) => page.getByRole("navigation").getByRole("button", { name });
  const main = page.getByRole("main");
  const openList = main.getByRole("list", { name: "Open requests" });
  const answeredList = main.getByRole("list", { name: "Answered today" });
  const answeredRow = (id: string): Locator => answeredList.locator(`li[aria-label="Answered ${id}"]`);
  const dialog = page.getByRole("dialog");

  /** A fact row of a card: its label, then its value (the DOM text holds no space between them). */
  const fact = (label: string, value: string): RegExp => new RegExp(`${label}\\s*${value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`);

  /** The open card of a kind, and its request ID. */
  async function cardOf(kind: string): Promise<{ card: Locator; id: string }> {
    const card = openList.locator(`article[data-kind="${kind}"]`).first();
    await expect(card).toBeVisible({ timeout: CARD_MS });
    const id = ((await card.getAttribute("aria-label")) ?? "").replace(/^Request /, "");
    expect(id).toMatch(/^R-\d+$/);
    return { card, id };
  }

  /** One press: the calls it made, `confirm_code` and then the act. */
  async function press(control: Locator, act: "answer" | "ack"): Promise<void> {
    const before = acts.length;
    await control.click();
    await expect(dialog).toHaveCount(0);
    await expect.poll(() => acts.slice(before).filter((a) => a !== "notice_ack"), { message: `one press: confirm_code, then ${act}` }).toEqual(["confirm_code", act]);
  }

  // Pair as "d33 browser", open Needs you.
  await page.goto(base);
  const pairCode = page.getByLabel("Pairing code");
  await expect(pairCode.or(page.getByRole("navigation"))).toBeVisible();
  if (await pairCode.isVisible()) {
    await pairCode.fill(code);
    await page.getByLabel("Device name").fill("d33 browser");
    await page.getByRole("button", { name: "Pair" }).click();
    await expect(page.getByRole("heading", { name: /^Connected to .+/ })).toBeVisible();
    await page.getByRole("button", { name: "Open Pulse Factory" }).click();
  }
  await expect(page.getByRole("navigation")).toBeVisible();
  await rail("Needs you").click();
  await expect(main.getByRole("heading", { level: 2, name: "Needs you" })).toBeVisible();
  // The stream is open: a device is told of a request that opens after its stream joined. Now the run starts.
  await expect.poll(() => requests.some((u) => u.endsWith("/bridge/stream") || u.includes("/bridge/stream?")), { message: "the page's stream" }).toBe(true);
  await expect(openList).toHaveCount(0);
  execFileSync(arm, [], { env: process.env, stdio: ["ignore", "ignore", "pipe"] });

  // 1. The stop.
  const stop = await cardOf("stop");
  const stopBlock = shown(stop.id).decision;
  expect(stopBlock?.cause).toBe("stop_file");
  expect(stopBlock?.options.map((o) => o.id)).toEqual(["s1", "s2"]);
  await expect(stop.card.getByTestId("kind-label")).toHaveText("QUESTION");
  await expect(stop.card.getByTestId("summary")).toHaveText(stopBlock?.summary ?? "");
  await expect(stop.card).toContainText(fact("Asked by", `Implementer · ${stopBlock?.refs.unit} · ${stop.id}`));
  await expect(stop.card).toContainText(fact("Attempt", `${stopBlock?.attempt}`));
  const s1 = stop.card.locator('[data-option="s1"]');
  const s2 = stop.card.locator('[data-option="s2"]');
  await expect(s1).toContainText("from Implementer");
  await expect(s2).toContainText("from Implementer");
  await expect(s1).not.toContainText("Recommended by");
  await expect(s2).toContainText("Recommended by Implementer");
  await expect(s2).toContainText("The seat marked this option.");
  await expect(stop.card.getByLabel("Another answer")).toBeVisible();
  // One notification, whose body ends with the request's ID.
  await expect.poll(() => page.evaluate(() => window.__notifications.length), { message: "the notification of the stop" }).toBe(1);
  const first = await page.evaluate(() => window.__notifications[0]);
  expect(first?.body.endsWith(` ${stop.id}`), `the body of the first notification ends with ${stop.id}: ${first?.body}`).toBe(true);
  expect(first?.title).toContain("asks a question");
  expect(acts.filter((a) => a !== "notice_ack"), "no call before the press").toEqual([]);
  await press(s2, "answer");
  await expect(openList.locator(`article[aria-label="Request ${stop.id}"]`)).toHaveCount(0);
  await expect(answeredRow(stop.id)).toContainText("Pressed");
  await expect(answeredRow(stop.id)).toContainText(stopBlock?.options[1]?.label ?? "");
  console.log(`step 1: the stop ${stop.id}: QUESTION, two options from the seat, s2 recommended, one notification; s2 pressed`);

  // 2. The park: the verifier's seat ended its turn with no done signal.
  const park = await cardOf("park");
  const parkBlock = shown(park.id).decision;
  expect(parkBlock?.cause).toBe("no_done_signal");
  expect(parkBlock?.options.map((o) => o.id)).toEqual(["resume"]);
  await expect(park.card.getByTestId("kind-label")).toHaveText("PARKED");
  await expect(park.card).toContainText(fact("Cause", "no_done_signal"));
  await expect(park.card).toContainText(fact("Attempt", `${parkBlock?.attempt}`));
  await expect(park.card).toContainText(fact("Parked by", "the controller"));
  await expect(park.card).toContainText(fact("About", `Verifier · ${parkBlock?.about?.unit}`));
  const resume = park.card.locator("[data-option]");
  await expect(resume).toHaveCount(1);
  await expect(resume).toHaveAttribute("data-option", "resume");
  await expect(resume).toContainText("from the controller");
  await expect(resume).not.toContainText("Recommended by");
  await park.card.getByLabel("Note for the next seat").fill(note);
  await press(resume, "answer");
  await expect(openList.locator(`article[aria-label="Request ${park.id}"]`)).toHaveCount(0);
  const parkRow = answeredRow(park.id);
  await expect(parkRow).toContainText("Resumed");
  await expect(parkRow).toContainText(parkBlock?.refs.unit ?? "");
  await expect(parkRow).toContainText(parkBlock?.refs.stage ?? "");
  await expect(parkRow).toContainText(`note: ${note}`);
  console.log(`step 2: the park ${park.id}: cause no_done_signal, one Resume from the controller, a note typed and pressed`);

  // 3. The red gate: the close seat's correction packet, with the workflow's two options.
  const gate = await cardOf("red_gate");
  const gateBlock = shown(gate.id).decision;
  expect(gateBlock?.options.map((o) => o.id)).toEqual(["w1", "w2"]);
  await expect(gate.card.getByTestId("kind-label")).toHaveText("RED GATE");
  await expect(gate.card).toContainText(/The gate.s reason/);
  await expect(gate.card).toContainText(shown(gate.id).reason);
  const w1 = gate.card.locator('[data-option="w1"]');
  const w2 = gate.card.locator('[data-option="w2"]');
  await expect(w1).toContainText("from the workflow");
  await expect(w2).toContainText("from the workflow");
  await expect(w1).toContainText("Recommended by the workflow");
  await expect(w1).toContainText("The workflow marked this option.");
  await expect(w2).not.toContainText("Recommended by");
  await expect(w1).toContainText("Answers, then resumes at");
  await expect(w1).toContainText(gateBlock?.options[0]?.effect.verb === "answer" ? (gateBlock.options[0].effect.to ?? "") : "");
  await press(w1, "answer");
  await expect(openList.locator(`article[aria-label="Request ${gate.id}"]`)).toHaveCount(0);
  await expect(answeredRow(gate.id)).toContainText("Pressed");
  await expect(answeredRow(gate.id)).toContainText(gateBlock?.options[0]?.label ?? "");
  console.log(`step 3: the red gate ${gate.id}: RED GATE, two options from the workflow, w1 recommended and pressed`);

  // 4. The merge approval: no recommended mark, one press.
  const merge = await cardOf("merge_ack");
  const mergeBlock = shown(merge.id).decision;
  expect(mergeBlock?.options.map((o) => o.id)).toEqual(["approve"]);
  await expect(merge.card.getByTestId("kind-label")).toHaveText("MERGE APPROVAL");
  await expect(merge.card.getByTestId("summary")).toHaveText(mergeBlock?.summary ?? "");
  await expect(merge.card).not.toContainText("Recommended by");
  await press(merge.card.getByRole("button", { name: "Approve merge" }), "ack");
  await expect(openList.locator(`article[aria-label="Request ${merge.id}"]`)).toHaveCount(0);
  await expect(answeredRow(merge.id)).toContainText("Merge approved");
  console.log(`step 4: the merge approval ${merge.id}: MERGE APPROVAL, no recommended mark; Approve merge pressed and the PR merged`);

  // One notification came for each request that opened, in order, and none for an answer, a resume or a merge.
  const notices = await page.evaluate(() => window.__notifications);
  expect(notices.map((n) => n.body.split(" ").pop())).toEqual([stop.id, park.id, gate.id, merge.id]);

  // Secrets: every request went to the bridge's origin, and the device token, the pairing code and every confirm
  // code that the bridge answered are in no storage, cookie, IndexedDB name, console line, request URL or the DOM.
  const foreign = requests.filter((u) => !u.startsWith(`${origin}/`) && !u.startsWith("data:"));
  expect(foreign, "requests to an origin other than the bridge's").toEqual([]);
  const token = (JSON.parse(fs.readFileSync(deviceFile, "utf8")) as { token: string }).token;
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  expect(confirmCodes.length, "the bridge answered one confirm code for each press: four").toBe(4);
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
    ["a notification", JSON.stringify(notices)],
  ];
  for (const [what, secret] of secrets) {
    for (const [where, text] of places) expect(text.includes(secret), `${what} is in ${where}`).toBe(false);
    expect(answers.join("\n").includes(secret), `${what} is in a bridge answer`).toBe(false);
  }
  expect(kept).not.toMatch(HEX64);
  expect(consoleText.join("\n")).not.toMatch(HEX64);
  expect(dom).not.toContain("pfc1-");
  console.log("step 6 (the page): no token, pairing code or confirm code in the page's storage, console, URLs or DOM; no request left the bridge");
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
