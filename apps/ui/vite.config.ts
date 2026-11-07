import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// `base: "./"` makes the built bundle independent of where it is served: the test bridge serves it
// at `/`, and the Tauri shell (w4) serves it from its own origin.
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Fonts stay files in the bundle; nothing is fetched from the network at run time.
    assetsInlineLimit: 0,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["src/test/setup.ts"],
    include: ["src/test/**/*.test.{ts,tsx}"],
    restoreMocks: true,
  },
});
