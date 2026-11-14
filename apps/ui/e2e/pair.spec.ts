// d12: the whole journey in headless Chromium against the built binary, the built bundle and the
// test bridge. `scripts/demo/ui-pair.sh` sets the environment and starts everything.
import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run this through scripts/demo/ui-pair.sh`);
  return value;
}

const HEX64 = /\b[0-9a-f]{64}\b/;

async function shot(page: Page, name: string) {
  const dir = process.env.PF_SHOTS_DIR;
  if (dir) await page.screenshot({ path: path.join(dir, name) });
}

test("pair, add a project, revoke the device, and see 3c", async ({ page }) => {
  const base = need("PF_BRIDGE_URL");
  const origin = new URL(base).origin;
  const code = need("PF_CODE");
  const bin = need("PF_BIN");
  need("PF_STATE_DIR");
  // The controller's home folder (`ui-pair.sh` makes it): the picker of Add a project lists it.
  const home = need("PF_D12_HOME");
  const deviceFile = path.join(need("PF_BRIDGE_STATE"), "device.json");

  const requests: string[] = [];
  const consoleText: string[] = [];
  const answers: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
  page.on("console", (m) => consoleText.push(m.text()));
  page.on("pageerror", (e) => consoleText.push(String(e)));
  page.on("response", (r) => {
    if (new URL(r.url()).pathname.startsWith("/bridge/")) {
      r.text().then(
        (t) => answers.push(t),
        () => {},
      );
    }
  });
  const bridgeCalls = () => requests.filter((u) => new URL(u).pathname.startsWith("/bridge/")).length;

  // 1. The page opens on 3a. The test pastes the code and the name, and pairs.
  await page.goto(base);
  await expect(page.getByRole("heading", { name: "Connect to your server" })).toBeVisible();
  await shot(page, "3a-pair.png");
  await page.getByLabel("Pairing code").fill(code);
  await page.getByLabel("Device name").fill("d12 browser");
  await page.getByRole("button", { name: "Pair" }).click();

  // 2. The success state names the server and shows the address. "Open Pulse Factory" shows Projects with an empty list.
  await expect(page.getByRole("heading", { name: /^Connected to .+/ })).toBeVisible();
  const serverName = ((await page.locator("dt:has-text('Server') + dd").textContent()) ?? "").trim();
  await expect(page.locator("dt:has-text('Address') + dd")).toHaveText(/^127\.0\.0\.1:\d+$/);
  await expect(page.getByText("d12 browser")).toBeVisible();
  await page.getByRole("button", { name: "Open Pulse Factory" }).click();
  await expect(page.getByRole("navigation")).toBeVisible();
  await expect(page.getByRole("banner").getByText("Projects")).toBeVisible();
  await expect(page.getByText("No projects yet. Use Add a project to add one.")).toBeVisible();

  // 3. The test makes a plain folder in the controller's home folder and adds it with `git init`. The folder
  //    becomes a repository and a project; its name shows in the sidebar and on the page. Then the test
  //    removes the project and adds the folder again, now a repository, without the box.
  const folder = path.join(home, "d12-project");
  fs.mkdirSync(folder);
  const name = path.basename(folder);
  const main = page.getByRole("main");
  const sidebar = page.getByRole("region", { name: "Sidebar" });
  /** Adds the folder through the dialog. `initGit` ticks the box. It stops on the success view. */
  async function addFolder(initGit: boolean) {
    await page.getByRole("button", { name: "Add a project" }).click();
    const dialog = page.getByRole("dialog", { name: "Add a project" });
    await dialog.getByRole("button", { name: new RegExp(`^${name}( |$)`) }).click();
    await expect(dialog.getByLabel("Project name")).toHaveValue(name);
    if (initGit) {
      await dialog.getByRole("checkbox", { name: /Run git init in this folder when I add it/ }).check();
    } else {
      // A repository already: no tip and no box.
      await expect(dialog.getByRole("checkbox")).toHaveCount(0);
    }
    await dialog.getByRole("button", { name: "Add project" }).click();
    await expect(dialog.getByText(`${name} is a project`)).toBeVisible();
    return dialog;
  }
  const insideWorkTree = () =>
    execFileSync("git", ["-C", folder, "rev-parse", "--is-inside-work-tree"], { encoding: "utf8" }).trim();
  try {
    let dialog = await addFolder(true);
    await expect(dialog.getByText(/^Ran git init in the folder\. It is now a git repository on branch \S+\.$/)).toBeVisible();
    await dialog.getByRole("button", { name: "Open project" }).click();
    await expect(page.getByRole("dialog", { name: "Add a project" })).toBeHidden();
    await expect(main.getByText(name, { exact: true })).toBeVisible();
    await expect(sidebar.getByText(name, { exact: true })).toBeVisible();
    expect(insideWorkTree()).toBe("true");

    // Remove it: the confirm with nothing live, then Remove project. The name leaves the page and the sidebar,
    // and the server lists no live project.
    await main.getByRole("button", { name: `Remove ${name}` }).click();
    const confirm = page.getByRole("alertdialog", { name: `Remove ${name}` });
    await expect(confirm.getByRole("heading", { name: `Remove ${name} from Pulse Factory?` })).toBeVisible();
    await confirm.getByRole("button", { name: "Remove project" }).click();
    await expect(main.getByText(new RegExp(`^${name} was removed\\.`))).toBeVisible();
    await expect(main.getByRole("heading", { name, exact: true })).toHaveCount(0);
    await expect(sidebar.getByText(name, { exact: true })).toHaveCount(0);
    const listed = JSON.parse(
      execFileSync(bin, ["project", "list", "--json"], { env: process.env, encoding: "utf8" }),
    ) as { projects: Array<{ removed_at: string | null }> };
    expect(listed.projects.filter((p) => p.removed_at === null)).toEqual([]);

    // Add it again, as a repository: its row says so, and the box is gone. Steps 4 and on find a project.
    await page.getByRole("button", { name: "Add a project" }).click();
    await expect(
      page.getByRole("dialog", { name: "Add a project" }).getByRole("button", { name: new RegExp(`^${name} git · `) }),
    ).toBeVisible();
    await page.keyboard.press("Escape");
    dialog = await addFolder(false);
    await expect(dialog.getByText(/Ran git init/)).toHaveCount(0);
    await dialog.getByRole("button", { name: "Open project" }).click();
    await expect(main.getByText(name, { exact: true })).toBeVisible();
    await expect(sidebar.getByText(name, { exact: true })).toBeVisible();
    await shot(page, "2c-projects.png");

    // The server menu (3b).
    await page.getByRole("banner").getByRole("button", { name: serverName }).click();
    await expect(page.getByRole("dialog", { name: "Server menu" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Disconnect this Mac" })).toBeVisible();
    await shot(page, "3b-server-menu.png");
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog", { name: "Server menu" })).toBeHidden();

    // 4. The test revokes the device on the server and opens the Projects page again. The 3c banner
    //    says the device is no longer paired, and no timer runs.
    execFileSync(bin, ["device", "revoke", "D-1"], { env: process.env, stdio: ["ignore", "pipe", "pipe"] });
    // The rail's buttons: the title bar has a Projects tab of its own since r1.s6.w1.
    const rail = page.getByRole("navigation", { name: "Sections" });
    await rail.getByRole("button", { name: "Settings" }).click();
    await rail.getByRole("button", { name: "Projects" }).click();
    await expect(page.getByText(/This device is no longer paired with 127\.0\.0\.1:\d+\./)).toBeVisible();
    await expect(page.getByText(/Retrying/)).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Pair again" })).toBeVisible();
    await expect(main.getByText(name, { exact: true })).toBeVisible(); // last data stays
    await shot(page, "3c-disconnected.png");
    const before = bridgeCalls();
    await page.waitForTimeout(4500); // longer than the 4 s retry of an unreachable server
    expect(bridgeCalls()).toBe(before);
  } finally {
    fs.rmSync(folder, { recursive: true, force: true });
  }

  // 5. Every request the page made went to the bridge's origin.
  const foreign = requests.filter((u) => !u.startsWith(`${origin}/`) && !u.startsWith("data:"));
  expect(foreign).toEqual([]);

  // 6. No token and no code in storage, a cookie, an IndexedDB name, the console or a bridge answer.
  const token = (JSON.parse(fs.readFileSync(deviceFile, "utf8")) as { token: string }).token;
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  const kept = await page.evaluate(async () => {
    const dbs = (await indexedDB.databases()).map((d) => d.name ?? "");
    return JSON.stringify({
      local: { ...localStorage },
      session: { ...sessionStorage },
      cookie: document.cookie,
      dbs,
    });
  });
  for (const [where, text] of [
    ["storage, cookie or IndexedDB", kept],
    ["the console", consoleText.join("\n")],
    ["a bridge answer", answers.join("\n")],
  ] as const) {
    expect(text.includes(token), `the token is in ${where}`).toBe(false);
    expect(text.includes(code), `the code is in ${where}`).toBe(false);
  }
  expect(kept).not.toMatch(HEX64);
  expect(consoleText.join("\n")).not.toMatch(HEX64);
  expect(answers.length).toBeGreaterThan(0);
  expect(fs.statSync(deviceFile).mode & 0o777).toBe(0o600);
});
