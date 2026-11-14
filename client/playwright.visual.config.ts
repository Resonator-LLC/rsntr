import { defineConfig } from "@playwright/test";

// Screenshot tests. Rendering depends on fonts and browser builds, so the
// browsers are always those of the official Playwright image (same version
// as @playwright/test), here and in CI: `npm run test:visual` starts it and
// connects through PW_VISUAL_WS. The tests and the servers run outside.
const wsEndpoint = process.env.PW_VISUAL_WS;
if (wsEndpoint === undefined || wsEndpoint === "") {
  throw new Error("Run the screenshot tests with `npm run test:visual` (browsers of the Playwright image).");
}

export default defineConfig({
  testDir: "e2e/visual",
  testMatch: "**/*.spec.ts",
  fullyParallel: false,
  workers: 1,
  timeout: 120_000,
  expect: {
    timeout: 10_000,
    toHaveScreenshot: { animations: "disabled", caret: "hide", maxDiffPixels: 0 },
  },
  snapshotPathTemplate: "e2e/visual/screens/{arg}{ext}",
  reporter: [["list"]],
  outputDir: "e2e/.results-visual",
  use: {
    connectOptions: { wsEndpoint },
    trace: "retain-on-failure",
    locale: "en-US",
    timezoneId: "UTC",
  },
});
