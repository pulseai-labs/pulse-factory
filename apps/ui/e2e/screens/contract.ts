// The rendering contract of d25 (`scripts/demo/ui-screens.sh`): everything that must be fixed for a
// screenshot of a screen to be the same on every run on one machine. The spec uses the one exported
// object, `CONTRACT`, and the functions below. Nothing here loosens a check to make a run pass: the
// tolerance is one constant, and a screen that cannot meet it is a defect to fix in the screen or here.
//
// What is fixed: the viewport (the artboards' 1440×900), the pixel ratio, the locale, the time zone, the
// colour scheme, reduced motion, the clock (installed before the page loads and paused at one time), the
// animations and the caret, the bundled faces (loaded and checked by name), the raster mode (partial raster
// off, see `chromiumArgs`) and the network (every request to an origin other than the fixture server is
// refused and recorded). Headless Chromium is not the Mac's WebKit: d25 holds the bundle's rendering in
// Chromium only.
import type { Page } from "@playwright/test";

export const CONTRACT = {
  viewport: { width: 1440, height: 900 },
  deviceScaleFactor: 1,
  locale: "en-US",
  timezoneId: "UTC",
  colorScheme: "dark",
  reducedMotion: "reduce",
  /** The clock is installed a minute before this and paused here, before the page loads. */
  clockStart: "2026-11-08T14:46:00Z",
  clockAt: "2026-11-08T14:47:00Z",
  /** The faces the bundle ships (`@fontsource-variable/geist` and `geist-mono`). */
  faces: ["Geist Variable", "Geist Mono Variable"],
  /**
   * Chromium's partial raster repaints only the invalidated part of a tile over the old pixels. A page that
   * painted its icons in two steps (the rail, before the request count arrived) kept slightly different
   * anti-aliasing on their edges than a page that painted them once: two states, picked at each page load
   * (16 pixels of a 1440×900 screen, a few pixels per icon, in about half of the runs). With the flag, every
   * paint is a whole-tile paint and the render depends on the page's content only. It is a rendering
   * contract, not a tolerance: it changes what is drawn, not what is accepted.
   */
  chromiumArgs: ["--disable-partial-raster"],
  /** The one tolerance of every screenshot. It is never raised to make a run pass. */
  tolerance: { maxDiffPixels: 0, threshold: 0.05 },
} as const;

/** What one opened page recorded. */
export interface Recorder {
  /** Every request to an origin other than the fixture server (each was aborted). */
  foreign: string[];
}

/**
 * Applies the contract to a page and opens a URL: the clock first, then the network rule, then the page.
 * The style that stops animations and hides the caret is added by `settle`, after the load.
 */
export async function openUnderContract(page: Page, origin: string, path = "/"): Promise<Recorder> {
  const recorder: Recorder = { foreign: [] };
  await page.clock.install({ time: new Date(CONTRACT.clockStart) });
  await page.clock.pauseAt(new Date(CONTRACT.clockAt));
  await page.route("**/*", async (route) => {
    const url = route.request().url();
    if (new URL(url).origin === origin) {
      await route.continue();
      return;
    }
    recorder.foreign.push(url);
    await route.abort();
  });
  await page.goto(new URL(path, origin).href);
  await page.addStyleTag({
    content: "*, *::before, *::after { animation: none !important; transition: none !important; caret-color: transparent !important; }",
  });
  return recorder;
}

/**
 * Waits for the bundled faces: it asks for each, waits for `document.fonts.ready`, and fails naming the
 * face when `document.fonts.check` is false or the face is not among the loaded ones.
 */
export async function settle(page: Page): Promise<void> {
  const report = await page.evaluate(async (faces) => {
    const out: Array<{ face: string; check: boolean; loaded: boolean }> = [];
    for (const face of faces) {
      await document.fonts.load(`13px "${face}"`);
    }
    await document.fonts.ready;
    for (const face of faces) {
      const loaded = [...document.fonts].some((f) => f.family.replace(/"/g, "") === face && f.status === "loaded");
      out.push({ face, check: document.fonts.check(`13px "${face}"`), loaded });
    }
    return out;
  }, [...CONTRACT.faces]);
  for (const r of report) {
    if (!r.check || !r.loaded) {
      throw new Error(`the face "${r.face}" is not loaded (document.fonts.check: ${r.check}, a loaded face of that name: ${r.loaded})`);
    }
  }
}
