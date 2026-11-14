// d25: the screens of the Mac app, each compared with a committed screenshot. One browser launch renders
// all of them, on fixed test data, under the fixed rendering contract of `e2e/screens/contract.ts`.
// `scripts/demo/ui-screens.sh` sets `PF_UI_DIR` (the built bundle) and runs this file. A changed style on
// any screen fails the line, and the failure names the screen. The script counts the `test(` calls of this
// file and checks the snapshot folder against that count, so a new screen edits no script.
//
// One `test.describe` block per area, in this order: shell, chat, needs-you, agents, settings, projects. Each
// round-3 item edits only its own block and its own fixture module (`e2e/screens/fixtures/<area>.ts`):
// w3 `chat`, w4 `needs-you`, `agents` and `settings`, w5 `projects`. A test that needs other data calls
// `overlay` (`e2e/screens/server.ts`) inside its own block. The shared helpers sit here, above the blocks.
//
// The fixture server stands in for the bridge's answers (`e2e/screens/server.ts`, test data for a
// rendering check). The journey lines d12, d19 and d23 keep the real controller, API and bridge.
import type { RequestList, RunList, SessionList } from "@pulse-factory/api-client";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { CONTRACT, openUnderContract, settle, type Recorder } from "./screens/contract";
import { PARKED } from "./screens/fixtures/needs-you";
import { clearOverlay, overlay, startFixtureServer, type FixtureServer, type Mode } from "./screens/server";

test.use({
  viewport: CONTRACT.viewport,
  deviceScaleFactor: CONTRACT.deviceScaleFactor,
  locale: CONTRACT.locale,
  timezoneId: CONTRACT.timezoneId,
  colorScheme: CONTRACT.colorScheme,
  reducedMotion: CONTRACT.reducedMotion,
  launchOptions: { args: [...CONTRACT.chromiumArgs] },
});

let server: FixtureServer;
let recorder: Recorder;

test.beforeAll(async () => {
  const dir = process.env.PF_UI_DIR;
  if (!dir) throw new Error("PF_UI_DIR is not set: run this through scripts/demo/ui-screens.sh");
  server = await startFixtureServer(dir);
});

test.afterAll(async () => {
  await server.close();
});

test.beforeEach(() => {
  server.setMode("paired");
  server.unexpected.length = 0;
});

test.afterEach(() => {
  clearOverlay();
  // A request to another origin, or a route or verb with no fixture, fails the screen that made it.
  expect(recorder.foreign, "requests to an origin other than the fixture server").toEqual([]);
  expect(server.unexpected, "bridge routes or verbs with no fixture").toEqual([]);
});

/**
 * Opens the app in a mode of the fixture server, under the contract. The dock is seeded before the page
 * loads (`pf.dock`): every screenshot is taken with it closed, except the dock's own.
 */
async function open(page: Page, mode: Mode = "paired", options: { dock?: boolean } = {}): Promise<void> {
  server.setMode(mode);
  const dock = options.dock === true ? "open" : "closed";
  await page.addInitScript((value) => window.localStorage.setItem("pf.dock", value), dock);
  recorder = await openUnderContract(page, server.origin);
}

/**
 * The screen is ready: its own marker is visible and the faces are loaded. Then the page is compared, or
 * `target` when the screenshot is of one part of it (a locator screenshot).
 */
async function compare(page: Page, name: string, ready: Locator, target?: Locator): Promise<void> {
  await expect(ready).toBeVisible();
  await settle(page);
  await expect(target ?? page).toHaveScreenshot(name, { ...CONTRACT.tolerance, animations: "disabled" });
}

const sidebar = (page: Page) => page.getByRole("region", { name: "Sidebar" });
const rail = (page: Page, name: string) => page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name });

test.describe("shell", () => {
  test("3a-pair: pair", async ({ page }) => {
    await open(page, "unpaired");
    await compare(page, "3a-pair.png", page.getByRole("heading", { name: "Connect to your server" }));
  });

  test("3b-server-menu: the server menu, open", async ({ page }) => {
    await open(page);
    await expect(page.getByRole("main").getByText("pilot-fourseat", { exact: true })).toBeVisible();
    // The menu opens from the server chip in the title bar. Its accessible name is the server's name.
    await page.getByRole("banner").getByRole("button", { name: "draco-desk" }).click();
    await compare(page, "3b-server-menu.png", page.getByRole("button", { name: "Disconnect this Mac" }));
  });

  test("3c-disconnected: disconnected", async ({ page }) => {
    // After `paired`, every call answers as an unreachable server, so the link goes down. The clock stays paused.
    await open(page, "unreachable");
    await compare(page, "3c-disconnected.png", page.getByText(/^Not connected to draco-desk$/));
  });

  test("sidebar-by-state: the sidebar, grouped by state", async ({ page }) => {
    await open(page);
    await sidebar(page).getByRole("button", { name: "By state" }).click();
    await compare(page, "sidebar-by-state.png", sidebar(page).getByText("Working", { exact: true }), sidebar(page));
  });
});

test.describe("chat", () => {
  /**
   * The chat S-2, opened from the sidebar, with its whole log held. The composer shows the `artifacts` switch of
   * 11a, which the shared runtime fixture does not declare: this block adds it through `overlay`, as every
   * screen of the block needs it. `working` makes the session work (a newer record wins in the app).
   */
  async function openChat(page: Page, options: { working?: boolean } = {}): Promise<void> {
    overlay({
      session_list: (base: unknown) => {
        const list = base as SessionList;
        return {
          ...list,
          runtimes: list.runtimes.map((r) => ({
            ...r,
            options: [...r.options, { id: "artifacts", label: "Artifacts", values: ["on", "off"], default: "off", free: false, live: false }],
          })),
          sessions: list.sessions.map((s) =>
            s.id === "S-2" && options.working ? { ...s, state: "working", updated_at: "2026-11-08T14:46:00Z" } : s,
          ),
        };
      },
    });
    await open(page);
    await sidebar(page).getByRole("button", { name: /Orchestrator session in pulse-factory/ }).click();
    await expect(page.getByRole("region", { name: "Chat" })).toBeVisible();
    await expect(page.getByTestId("start-card")).toBeAttached();
    await expect(page.getByText("Store schema first", { exact: false })).toBeVisible();
  }

  /** The stream starts at its end. A wheel turn up shows its beginning: the day divider, the first turns. */
  async function toStart(page: Page): Promise<void> {
    const log = page.getByRole("log", { name: "Messages" });
    await log.hover();
    await page.mouse.wheel(0, -10_000);
    await expect(log.getByText("Today", { exact: true })).toBeInViewport();
  }

  test("2a-chat: the shell with the sidebar open, the chat", async ({ page }) => {
    await openChat(page);
    await toStart(page);
    await compare(page, "2a-chat.png", page.getByRole("log", { name: "Messages" }));
  });

  test("2b-chat-sidebar-closed: the same, with the sidebar closed", async ({ page }) => {
    await openChat(page);
    await page.keyboard.press("Meta+b");
    await expect(sidebar(page)).toBeHidden();
    await toStart(page);
    await compare(page, "2b-chat-sidebar-closed.png", page.getByRole("log", { name: "Messages" }));
  });

  test("6b-start-card: the start card in the chat", async ({ page }) => {
    await openChat(page);
    // The line above the card scrolls to the top of the log, so the card sits whole below it and its place in the
    // thread is part of the screenshot: a change of the card's margin or of the gap above it moves the card.
    // (The log keeps its end in place, so a change above a card that fills the top of the log would not show.)
    await page.getByTestId("start-card").evaluate((card) => {
      (card.previousElementSibling ?? card).scrollIntoView({ block: "start" });
    });
    await compare(page, "6b-start-card.png", page.getByTestId("start-workflow"));
  });

  test("11b-end-session: End session asked while the agent works", async ({ page }) => {
    await openChat(page, { working: true });
    await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();
    await page.getByRole("group", { name: "Chat header" }).getByRole("button", { name: "End session" }).click();
    await compare(page, "11b-end-session.png", page.getByRole("group", { name: "Confirm end" }));
  });
});

test.describe("needs-you", () => {
  test("3f-needs-you: Needs you", async ({ page }) => {
    // The parked item is this test's alone: one more open request would change the rail's count on every other
    // screen. It comes through the overlay (its `show` answer is in `fixtures/needs-you.ts`).
    overlay({
      requests: (base: unknown) => {
        const list = base as RequestList;
        return { ...list, open: list.open + 1, requests: [...list.requests, PARKED] };
      },
    });
    // The page is a column of five cards and the answered rows: a taller window shows all of them in one
    // screenshot. Only the height changes; the width and the pixel ratio stay as the contract fixes them.
    await page.setViewportSize({ width: CONTRACT.viewport.width, height: 1720 });
    await open(page);
    await rail(page, "Needs you").click();
    await expect(page.getByRole("heading", { name: "Needs you", level: 2 })).toBeVisible();
    const main = page.getByRole("main");
    // The merge card, the run question, the parked item and the chat question, then the answered rows.
    await expect(main.getByTestId("merge-sha")).toBeVisible();
    await expect(main.getByTestId("stop-text")).toBeVisible();
    await expect(main.getByRole("button", { name: "Resume w3" })).toBeVisible();
    await expect(main.getByRole("button", { name: "Open session" })).toBeVisible();
    await expect(main.getByRole("list", { name: "Answered today" }).getByText("CLI · claim session C-2").first()).toBeVisible();
    await compare(page, "3f-needs-you.png", main.getByRole("button", { name: "Approve merge" }));
  });

  test("dock-needs-you: the dock, open on Projects", async ({ page }) => {
    await open(page, "paired", { dock: true });
    const dock = page.getByRole("complementary", { name: "Needs you" });
    await expect(dock.getByRole("button", { name: "Open" }).first()).toBeVisible();
    await expect(dock.getByTestId("merge-sha")).toBeVisible();
    await compare(page, "dock-needs-you.png", dock.getByText("all projects"), dock);
  });
});

test.describe("agents", () => {
  test("6a-agents: Agents", async ({ page }) => {
    await open(page);
    await rail(page, "Agents").click();
    await expect(page.getByRole("heading", { name: "Orchestrator", level: 2 })).toBeVisible();
    const sessions = page.getByRole("list", { name: "Sessions by project" });
    await expect(sessions.getByText("Waiting for your answer")).toBeVisible();
    await compare(page, "6a-agents.png", sessions.getByText("Working"));
  });
});

test.describe("settings", () => {
  test("settings: Settings", async ({ page }) => {
    // The page holds the server block and three policy blocks: a taller window shows all of them.
    await page.setViewportSize({ width: CONTRACT.viewport.width, height: 1480 });
    await open(page);
    await rail(page, "Settings").click();
    await expect(page.getByText("chat.self_turn_cap")).toBeVisible();
    await expect(page.getByRole("region", { name: "Chat policies" })).toBeVisible();
    await compare(page, "settings.png", page.getByText("Server version"));
  });
});

test.describe("projects", () => {
  test("2c-projects: Projects", async ({ page }) => {
    // The app opens on Projects, and a click on its rail item would toggle the sidebar.
    await open(page);
    await expect(page.getByRole("main").getByRole("button", { name: "Add a project" })).toBeVisible();
    await expect(page.getByRole("main").getByRole("button", { name: "Remove pilot-fourseat" })).toBeVisible();
    await compare(page, "2c-projects.png", page.getByRole("main").getByText("pilot-fourseat", { exact: true }));
  });

  test("11e-remove-confirm: the confirm in pulse-factory's block, a workflow running", async ({ page }) => {
    // The active run r0-s19-1 belongs to pulse-factory: the confirm names it and locks Remove project.
    overlay({
      run_list: (base: unknown) => {
        const list = base as RunList;
        return { ...list, runs: list.runs.map((r) => (r.id === "r0-s19-1" ? { ...r, project_id: "P-1" } : r)) };
      },
    });
    await open(page);
    await page.getByRole("main").getByRole("button", { name: "Remove pulse-factory" }).click();
    const confirm = page.getByRole("alertdialog", { name: "Remove pulse-factory" });
    await expect(confirm.getByRole("heading", { name: "pulse-factory has a workflow running" })).toBeVisible();
    await compare(page, "11e-remove-confirm.png", confirm.getByRole("button", { name: "Remove project" }));
  });

  test("11f-add-project: the Add a project dialog, scratch picked", async ({ page }) => {
    await open(page);
    await page.getByRole("main").getByRole("button", { name: "Add a project" }).click();
    const dialog = page.getByRole("dialog", { name: "Add a project" });
    await dialog.getByRole("button", { name: /^scratch/ }).click();
    await expect(dialog.getByText("Tip: make it a git repository")).toBeVisible();
    await compare(page, "11f-add-project.png", dialog.getByRole("button", { name: "Add project" }));
  });
});
