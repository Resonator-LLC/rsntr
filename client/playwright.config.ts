import { defineConfig, devices } from "@playwright/test";

// End-to-end tests in a real Chromium. `e2e/editor` drives the editor alone
// on a harness page; `e2e/app` starts the real server and the built client.
// Each suite starts what it needs itself, on ephemeral ports of 127.0.0.1.
export default defineConfig({
  testDir: "e2e",
  testMatch: "**/*.spec.ts",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: [["list"]],
  outputDir: "e2e/.results",
  use: {
    ...devices["Desktop Chrome"],
    trace: "retain-on-failure",
  },
});
