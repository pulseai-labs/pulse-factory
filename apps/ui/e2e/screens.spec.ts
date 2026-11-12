// d25: the ten screens of the Mac app, each compared with a committed screenshot. One browser launch
// renders all of them, on fixed test data, under the fixed rendering contract of `e2e/screens/contract.ts`.
// `scripts/demo/ui-screens.sh` sets `PF_UI_DIR` (the built bundle) and runs this file. A changed style on
// any screen fails the line, and the failure names the screen.
//
// The fixture server stands in for the bridge's answers (`e2e/screens/server.ts`, test data for a
// rendering check). The journey lines d12, d19 and d23 keep the real controller, API and bridge.
import { expect, test, type Locator, type Page } from "@playwright/test";
import { CONTRACT, openUnderContract, settle, type Recorder } from "./screens/contract";
import { startFixtureServer, type FixtureServer, type Mode } from "./screens/server";

test.use({
  viewport: CONTRACT.viewport,
  deviceScaleFactor: CONTRACT.deviceScaleFactor,
  locale: CONTRACT.locale,
  timezoneId: CONTRACT.timezoneId,
  colorScheme: CONTRACT.colorScheme,
  reducedMotion: CONTRACT.reducedMotion,
});

const ADDRESS = /100\.90\.203\.21:7443/;

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
  // A request to another origin, or a route or verb with no fixture, fails the screen that made it.
  expect(recorder.foreign, "requests to an origin other than the fixture server").toEqual([]);
  expect(server.unexpected, "bridge routes or verbs with no fixture").toEqual([]);
});

/** Opens the app in a mode of the fixture server, under the contract. */
async function open(page: Page, mode: Mode = "paired"): Promise<void> {
  server.setMode(mode);
  recorder = await openUnderContract(page, server.origin);
}

/** The screen is ready: its own marker is visible and the faces are loaded. Then it is compared. */
async function compare(page: Page, name: string, ready: Locator): Promise<void> {
  await expect(ready).toBeVisible();
  await settle(page);
  await expect(page).toHaveScreenshot(name, { ...CONTRACT.tolerance, animations: "disabled" });
}

const sidebar = (page: Page) => page.getByRole("region", { name: "Sidebar" });
const rail = (page: Page, name: string) => page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name });

/** The chat S-2, opened from the sidebar, with its whole log held. */
async function openChat(page: Page): Promise<void> {
  await open(page);
  await sidebar(page).getByRole("button", { name: /Orchestrator session in pulse-factory/ }).click();
  await expect(page.getByRole("region", { name: "Chat" })).toBeVisible();
  await expect(page.getByTestId("start-card")).toBeAttached();
  await expect(page.getByText("Chat first", { exact: true })).toBeVisible();
}

test("2a-chat: the shell with the sidebar open, the chat", async ({ page }) => {
  await openChat(page);
  await compare(page, "2a-chat.png", page.getByRole("log", { name: "Messages" }));
});

test("2b-chat-sidebar-closed: the same, with the sidebar closed", async ({ page }) => {
  await openChat(page);
  await page.keyboard.press("Meta+b");
  await expect(sidebar(page)).toBeHidden();
  await compare(page, "2b-chat-sidebar-closed.png", page.getByRole("log", { name: "Messages" }));
});

test("2c-projects: Projects", async ({ page }) => {
  // The app opens on Projects, and a click on its rail item would toggle the sidebar.
  await open(page);
  await compare(page, "2c-projects.png", page.getByRole("main").getByText("pilot-fourseat", { exact: true }));
});

test("6b-start-card: the start card in the chat", async ({ page }) => {
  await openChat(page);
  await page.getByTestId("start-card").scrollIntoViewIfNeeded();
  await compare(page, "6b-start-card.png", page.getByRole("button", { name: "Confirm start" }));
});

test("3a-pair: pair", async ({ page }) => {
  await open(page, "unpaired");
  await compare(page, "3a-pair.png", page.getByRole("heading", { name: "Connect to your server" }));
});

test("3b-server-menu: the server menu, open", async ({ page }) => {
  await open(page);
  await expect(page.getByRole("main").getByText("pilot-fourseat", { exact: true })).toBeVisible();
  await sidebar(page).getByRole("button", { name: ADDRESS }).click();
  await compare(page, "3b-server-menu.png", page.getByRole("button", { name: "Disconnect this Mac" }));
});

test("3c-disconnected: disconnected", async ({ page }) => {
  // After `paired`, every call answers as an unreachable server, so the link goes down. The clock stays paused.
  await open(page, "unreachable");
  await compare(page, "3c-disconnected.png", page.getByText(/^Not connected to 100\.90\.203\.21:7443$/));
});

test("6a-agents: Agents", async ({ page }) => {
  await open(page);
  await rail(page, "Agents").click();
  await expect(page.getByRole("heading", { name: "Agents", level: 2 })).toBeVisible();
  await expect(page.getByRole("list", { name: "Sessions by project" }).getByText("Needs you")).toBeVisible();
  await compare(page, "6a-agents.png", page.getByRole("list", { name: "Sessions by project" }).getByText("Working"));
});

test("3f-needs-you: Needs you", async ({ page }) => {
  await open(page);
  await rail(page, "Needs you").click();
  await expect(page.getByRole("heading", { name: "Needs you", level: 2 })).toBeVisible();
  await expect(page.getByLabel("Run r0-s19-1")).toBeVisible();
  await expect(page.getByRole("button", { name: "Confirm merge" })).toBeVisible();
  await compare(page, "3f-needs-you.png", page.getByRole("button", { name: "Send answer" }));
});

test("settings: Settings", async ({ page }) => {
  await open(page);
  await rail(page, "Settings").click();
  await expect(page.getByText("chat.self_turn_cap")).toBeVisible();
  await compare(page, "settings.png", page.getByText("Server version"));
});
