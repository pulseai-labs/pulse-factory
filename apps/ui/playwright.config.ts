import { defineConfig } from "@playwright/test";

// d12 (scripts/demo/ui-pair.sh) runs this. Chromium 1248 comes from the Playwright cache that
// belongs to @playwright/test 1.64.1 (ADR-0013): no browser is downloaded here.
export default defineConfig({
  testDir: "e2e",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 25_000,
  reporter: [["line"]],
  // The script puts the output in its temp directory, so a run leaves nothing in the worktree.
  outputDir: process.env.PF_E2E_OUT ?? "test-results",
  use: {
    baseURL: process.env.PF_BRIDGE_URL,
    browserName: "chromium",
    channel: "chromium",
    headless: true,
    trace: "off",
    video: "off",
    screenshot: "off",
  },
});
