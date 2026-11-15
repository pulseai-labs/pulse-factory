// The motion of the approved canvas, watched in headless Chromium (patch `fix/ui-canvas-motion`, 2026-11-15). The
// unit tests (`src/test/motion.test.tsx`) read the state and the rules; this file watches the transitions and the
// animations the browser runs, which jsdom cannot:
//
// - ⌘B and the need-you pill animate the frame's width over 280 ms on the standard curve, the panel keeps its
//   width inside the frame, and the main column takes the width the frame gives up, frame by frame;
// - a persisted state and a page change start no transition;
// - a closed panel takes no focus from Tab;
// - the switch animates its track and its knob over 120 ms;
// - the avatars' eyes run 9a's loops at 24 px and not at 16 px;
// - under reduced motion none of this runs.
//
//   PF_UI_DIR=apps/ui/dist npm --prefix apps run e2e --workspace ui -- e2e/motion.spec.ts
//
// It runs on d25's fixture server (`e2e/screens/server.ts`, the recorded fake `ui-screens-fixture-server`) and
// reaches nothing beyond it. No demo line runs this file: d25 (`scripts/demo/ui-screens.sh`) keeps its screenshots
// with motion off, and the journey lines d12, d19 and d23 run on the real controller.
import type { SessionList } from "@pulse-factory/api-client";
import { expect, test, type Page } from "@playwright/test";
import { clearOverlay, overlay, startFixtureServer, type FixtureServer } from "./screens/server";

test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, colorScheme: "dark", locale: "en-US", timezoneId: "UTC" });

let server: FixtureServer;
let foreign: string[] = [];

test.beforeAll(async () => {
  const dir = process.env.PF_UI_DIR;
  if (!dir) throw new Error("PF_UI_DIR is not set: build the UI and point PF_UI_DIR at apps/ui/dist");
  server = await startFixtureServer(dir);
});

test.afterAll(async () => {
  await server.close();
});

test.beforeEach(() => {
  server.setMode("paired");
  server.unexpected.length = 0;
  foreign = [];
});

test.afterEach(() => {
  clearOverlay();
  expect(foreign, "requests to an origin other than the fixture server").toEqual([]);
  expect(server.unexpected, "bridge routes or verbs with no fixture").toEqual([]);
});

/** One `transitionrun` the page saw: the frame or the element it ran on, and the property. */
interface Run {
  on: string;
  property: string;
}

/**
 * Opens the app with these localStorage entries. Before the page loads, every `transitionrun` is recorded in
 * `window.__runs`, so a transition that starts on the first paint is seen even when it ends before a check.
 */
async function open(page: Page, storage: Record<string, string> = {}): Promise<void> {
  await page.route("**/*", async (route) => {
    const url = route.request().url();
    if (new URL(url).origin === server.origin) {
      await route.continue();
      return;
    }
    foreign.push(url);
    await route.abort();
  });
  await page.addInitScript((entries) => {
    for (const [key, value] of entries) window.localStorage.setItem(key, value);
    const runs: Array<{ on: string; property: string }> = [];
    (window as unknown as { __runs: typeof runs }).__runs = runs;
    document.addEventListener(
      "transitionrun",
      (e) => {
        const el = e.target as Element;
        runs.push({ on: el.getAttribute("data-panel") ?? el.className.toString(), property: e.propertyName });
      },
      true,
    );
  }, Object.entries(storage));
  await page.goto(`${server.origin}/`);
  await expect(page.getByRole("navigation", { name: "Sections" })).toBeVisible();
}

const runs = (page: Page) => page.evaluate(() => (window as unknown as { __runs: Run[] }).__runs.slice());
const panelRuns = async (page: Page) => (await runs(page)).filter((r) => r.on === "sidebar" || r.on === "dock");

/** The running CSS transitions on a frame: property, duration and curve (the effect's, else the first keyframe's). */
function frameTransitions(page: Page, name: "sidebar" | "dock") {
  return page.evaluate((panel) => {
    const el = document.querySelector(`[data-panel="${panel}"]`);
    return document
      .getAnimations()
      .filter((a): a is CSSTransition => a instanceof CSSTransition && (a.effect as KeyframeEffect | null)?.target === el)
      .map((t) => {
        const effect = t.effect as KeyframeEffect;
        const timing = effect.getComputedTiming();
        const first = effect.getKeyframes()[0];
        return {
          property: t.transitionProperty,
          duration: timing.duration,
          easing: timing.easing === "linear" ? String(first?.easing ?? "") : String(timing.easing),
        };
      });
  }, name);
}

/**
 * Pauses the frame's width transition at `ms`, reads the widths there and finishes it: the frame, the panel inside
 * it, and the main column; the panel's left and right edges and the frame's.
 */
function widthsAt(page: Page, name: "sidebar" | "dock", ms: number) {
  return page.evaluate(
    ([panel, at]) => {
      const frame = document.querySelector(`[data-panel="${panel}"]`) as HTMLElement;
      const inner = frame.firstElementChild as HTMLElement;
      const main = document.querySelector("main.main") as HTMLElement;
      const t = document.getAnimations().find((a) => a instanceof CSSTransition && (a.effect as KeyframeEffect).target === frame);
      if (!t) return null;
      t.pause();
      t.currentTime = at as number;
      const f = frame.getBoundingClientRect();
      const p = inner.getBoundingClientRect();
      const out = { frame: f.width, panel: p.width, main: main.getBoundingClientRect().width, frameLeft: f.left, frameRight: f.right, panelLeft: p.left, panelRight: p.right };
      t.finish();
      return out;
    },
    [name, ms] as const,
  );
}

const width = (page: Page, selector: string) => page.locator(selector).evaluate((el) => el.getBoundingClientRect().width);
const STANDARD = "cubic-bezier(0.2, 0, 0, 1)";

test.describe("with motion", () => {
  test.use({ reducedMotion: "no-preference" });

  test("draws a persisted state with no transition on the first paint", async ({ page }) => {
    await open(page, { "pf.sidebar": "closed", "pf.dock": "open" });
    await expect(page.locator('[data-panel="dock"] aside.dock')).toBeVisible();
    // Give any late style change the time of a whole transition to show itself.
    await page.waitForTimeout(400);
    expect(await panelRuns(page)).toEqual([]);
    expect(await width(page, '[data-panel="sidebar"]')).toBe(0);
    expect(await width(page, '[data-panel="dock"]')).toBe(328);
  });

  test("⌘B animates the sidebar's frame over 280 ms on the standard curve, and the main column follows it", async ({ page }) => {
    await open(page, { "pf.dock": "closed" });
    await expect(page.getByRole("region", { name: "Sidebar" })).toBeVisible();
    const before = await width(page, "main.main");
    expect(await width(page, '[data-panel="sidebar"]')).toBe(288);

    await page.keyboard.press("Meta+b");
    expect(await frameTransitions(page, "sidebar")).toEqual([{ property: "width", duration: 280, easing: STANDARD }]);
    const mid = await widthsAt(page, "sidebar", 140);
    expect(mid).not.toBeNull();
    const m = mid as NonNullable<typeof mid>;
    // Half way through, the frame is part open, the sidebar keeps its 288 px, and the main column holds the rest.
    expect(m.frame).toBeGreaterThan(0);
    expect(m.frame).toBeLessThan(288);
    expect(m.panel).toBe(288);
    expect(m.main + m.frame).toBeCloseTo(before + 288, 0);
    await expect(page.getByRole("region", { name: "Sidebar" })).toBeHidden();
    expect(await width(page, '[data-panel="sidebar"]')).toBe(0);
    expect(await width(page, "main.main")).toBeCloseTo(before + 288, 0);

    await page.keyboard.press("Meta+b");
    expect(await frameTransitions(page, "sidebar")).toEqual([{ property: "width", duration: 280, easing: STANDARD }]);
  });

  test("the pill animates the dock's frame over 280 ms, and the dock stays against the right edge", async ({ page }) => {
    await open(page, { "pf.dock": "closed" });
    const before = await width(page, "main.main");
    await page.getByRole("banner").getByTitle("Show or hide Needs you").click();
    expect(await frameTransitions(page, "dock")).toEqual([{ property: "width", duration: 280, easing: STANDARD }]);
    const mid = await widthsAt(page, "dock", 140);
    expect(mid).not.toBeNull();
    const m = mid as NonNullable<typeof mid>;
    expect(m.frame).toBeGreaterThan(0);
    expect(m.frame).toBeLessThan(328);
    expect(m.panel).toBe(328);
    // `justify-content:flex-end`: the dock's right edge is the frame's, and its left part is the part clipped.
    expect(m.panelRight).toBeCloseTo(m.frameRight, 0);
    expect(m.panelLeft).toBeLessThan(m.frameLeft);
    expect(m.main + m.frame).toBeCloseTo(before, 0);
    await expect(page.getByRole("complementary", { name: "Needs you" })).toBeVisible();
    expect(await width(page, "main.main")).toBeCloseTo(before - 328, 0);
  });

  test("a page change starts no transition: Settings drops the frames, Back mounts them in their state", async ({ page }) => {
    await open(page, { "pf.dock": "open" });
    await expect(page.getByRole("complementary", { name: "Needs you" })).toBeVisible();
    await page.waitForTimeout(400);
    await page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name: "Settings" }).click();
    await expect(page.getByText("Server version")).toBeVisible();
    await expect(page.locator("[data-panel]")).toHaveCount(0);
    await page.getByRole("banner").getByRole("button", { name: "Back" }).click();
    await expect(page.getByRole("complementary", { name: "Needs you" })).toBeVisible();
    await page.waitForTimeout(400);
    expect(await panelRuns(page)).toEqual([]);
    expect(await width(page, '[data-panel="sidebar"]')).toBe(288);
    expect(await width(page, '[data-panel="dock"]')).toBe(328);
  });

  test("a closed panel takes no focus: Tab walks past the sidebar and the dock", async ({ page }) => {
    await open(page, { "pf.sidebar": "closed", "pf.dock": "closed" });
    await expect(page.locator('[data-panel="sidebar"] section.sidebar')).toBeAttached();
    const stops: string[] = [];
    for (let i = 0; i < 60; i++) {
      await page.keyboard.press("Tab");
      const where = await page.evaluate(() => {
        const el = document.activeElement;
        if (!el || el === document.body) return "body";
        return el.closest("[data-panel]") ? `inside ${el.closest("[data-panel]")?.getAttribute("data-panel")}` : "outside";
      });
      stops.push(where);
    }
    expect(stops.filter((s) => s.startsWith("inside"))).toEqual([]);
    expect(stops.filter((s) => s === "outside").length).toBeGreaterThan(5);
  });

  test("the switch animates its track and its knob over 120 ms, as 11a's Artifacts switch does", async ({ page }) => {
    overlay({
      session_list: (base: unknown) => {
        const list = base as SessionList;
        return {
          ...list,
          runtimes: list.runtimes.map((r) => ({
            ...r,
            options: [...r.options, { id: "artifacts", label: "Artifacts", values: ["on", "off"], default: "off", free: false, live: false }],
          })),
        };
      },
    });
    await open(page);
    await page.getByRole("region", { name: "Sidebar" }).getByRole("button", { name: /Orchestrator session in pulse-factory/ }).click();
    const sw = page.getByRole("switch", { name: "Artifacts" });
    await expect(sw).toBeVisible();
    // Flip the box's state in the page only: the change makes no call, and the style change starts the transitions.
    const seen = await sw.evaluate((el: HTMLInputElement) => {
      el.checked = !el.checked;
      return document
        .getAnimations()
        .filter((a): a is CSSTransition => a instanceof CSSTransition && (a.effect as KeyframeEffect).target === el)
        .map((t) => ({ property: t.transitionProperty, pseudo: (t.effect as KeyframeEffect).pseudoElement ?? "", duration: t.effect?.getComputedTiming().duration }))
        .sort((a, b) => a.property.localeCompare(b.property));
    });
    expect(seen).toEqual([
      { property: "background-color", pseudo: "", duration: 120 },
      { property: "left", pseudo: "::after", duration: 120 },
    ]);
  });

  test("the avatars' eyes run 9a's loops at 24 px, and none at 16 px", async ({ page }) => {
    await open(page);
    const row = page.getByRole("region", { name: "Sidebar" }).getByRole("button", { name: /Orchestrator session in pulse-factory/ });
    await row.click();
    const names = (selector: string) =>
      page.evaluate(
        (sel) =>
          [...document.querySelectorAll(sel)].flatMap((el) =>
            el.getAnimations({ subtree: true }).map((a) => (a as CSSAnimation).animationName),
          ),
        selector,
      );
    // The sidebar's rows draw their agents at 24 px, idle: the eye blinks and the iris looks around.
    const rows = await names('[data-panel="sidebar"] .ui-avatar');
    expect(rows.length).toBeGreaterThan(0);
    expect(new Set(rows)).toEqual(new Set(["lvBlink", "lvLook"]));
    // The tab draws its agent at 16 px: no motion.
    expect(await names(".tb-tabs .ui-avatar")).toEqual([]);
    expect(await page.locator(".tb-tabs .ui-avatar-sm").count()).toBeGreaterThan(0);
  });
});

test.describe("under reduced motion", () => {
  test.use({ reducedMotion: "reduce" });

  test("⌘B and the pill start no transition, and no avatar moves", async ({ page }) => {
    await open(page, { "pf.dock": "closed" });
    await expect(page.getByRole("region", { name: "Sidebar" })).toBeVisible();
    await page.keyboard.press("Meta+b");
    expect(await frameTransitions(page, "sidebar")).toEqual([]);
    expect(await width(page, '[data-panel="sidebar"]')).toBe(0);
    await page.getByRole("banner").getByTitle("Show or hide Needs you").click();
    expect(await frameTransitions(page, "dock")).toEqual([]);
    expect(await width(page, '[data-panel="dock"]')).toBe(328);
    expect(await panelRuns(page)).toEqual([]);
    const moving = await page.evaluate(() => document.getAnimations().filter((a) => a instanceof CSSAnimation).length);
    expect(moving).toBe(0);
  });
});
