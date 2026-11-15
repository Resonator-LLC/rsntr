// Wave apps with sound: the walkie example with Echo on. Chromium's fake
// microphone feeds the Mic button; the frames go to the app on the node,
// which sends them back for the speaker, where they are decoded and
// played.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

import { shoot, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";
import { zip } from "./support/zip";

test.use({
  launchOptions: { args: ["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"] },
});

const WALKIE = readFileSync(
  resolve(fileURLToPath(new URL(".", import.meta.url)), "../../../crates/resonator-apps/tests/fixtures/walkie.wasm"),
);
const BUNDLE = zip([
  [
    "manifest.json",
    Buffer.from(JSON.stringify({ name: "walkie", version: "1", permissions: ["peers", "streams", "media"] })),
  ],
  ["app.wasm", WALKIE],
]);

const WALKIE_CAM = readFileSync(
  resolve(fileURLToPath(new URL(".", import.meta.url)), "../../../crates/resonator-apps/tests/fixtures/walkie_cam.wasm"),
);
const CAM_BUNDLE = zip([
  [
    "manifest.json",
    Buffer.from(
      JSON.stringify({ name: "walkie-cam", version: "1", permissions: ["peers", "streams", "media", "camera"] }),
    ),
  ],
  ["app.wasm", WALKIE_CAM],
]);

let servers: TestServers;

test.beforeAll(async () => {
  servers = await TestServers.start({ device: true, apps: true });
});
test.afterAll(async () => {
  await servers?.stop();
});

test("the Mic button feeds the app, and what it sends back is played", async ({ browser }) => {
  const context = await browser.newContext({ permissions: ["microphone"] });
  await context.addInitScript(() => {
    const roots: ShadowRoot[] = [];
    (window as unknown as { appRoots: ShadowRoot[] }).appRoots = roots;
    const attach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function (init: ShadowRootInit) {
      const root = attach.call(this, init);
      roots.push(root);
      return root;
    };
  });
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.deviceUrl!);
  await page.locator(".topbar").waitFor();
  const supported = await page.evaluate(() => typeof AudioEncoder !== "undefined");
  test.skip(!supported, "this browser has no WebCodecs audio");

  await page.locator("[data-action=new-wave]").click();
  await expect(page.locator(".blip-editing .doc[contenteditable=true]")).toBeFocused();
  await page.keyboard.type("Walkie");
  await page.keyboard.press("Enter");
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('.edit-toolbar [data-command="app"]').click(),
  ]);
  await chooser.setFiles({ name: "walkie.zip", mimeType: "application/zip", buffer: BUNDLE });
  const box = page.locator(".doc-app .app-box");
  await expect(box.locator(".app-status")).toHaveText("Running", { timeout: 60_000 });

  // Echo on (the app's button), then the microphone (ours).
  await page.evaluate(() => {
    const root = (window as unknown as { appRoots: ShadowRoot[] }).appRoots[0]!;
    (root.querySelector("button") as HTMLButtonElement).click();
  });
  const mic = box.locator(".app-mic-button");
  await expect(mic).toHaveText("Turn microphone on");
  await mic.click();
  await expect(mic).toHaveText("Turn microphone off", { timeout: 15_000 });
  await expect.poll(async () => Number(await box.getAttribute("data-media-sent")), { timeout: 15_000 }).toBeGreaterThan(5);
  await expect
    .poll(async () => Number(await box.getAttribute("data-media-played")), { timeout: 15_000 })
    .toBeGreaterThan(5);
  await mic.click();
  await expect(mic).toHaveText("Turn microphone on");
  expect(problems).toEqual([]);
  await context.close();
});

test("the camera button feeds the app, and the video it sends back is shown", async ({ browser }) => {
  const context = await browser.newContext({ permissions: ["microphone", "camera"] });
  await context.addInitScript(() => {
    const roots: ShadowRoot[] = [];
    (window as unknown as { appRoots: ShadowRoot[] }).appRoots = roots;
    const attach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function (init: ShadowRootInit) {
      const root = attach.call(this, init);
      roots.push(root);
      return root;
    };
  });
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.deviceUrl!);
  await page.locator(".topbar").waitFor();
  const supported = await page.evaluate(() => typeof VideoEncoder !== "undefined");
  test.skip(!supported, "this browser has no WebCodecs video");

  await page.locator("[data-action=new-wave]").click();
  await expect(page.locator(".blip-editing .doc[contenteditable=true]")).toBeFocused();
  await page.keyboard.type("Walkie cam");
  await page.keyboard.press("Enter");
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('.edit-toolbar [data-command="app"]').click(),
  ]);
  await chooser.setFiles({ name: "walkie-cam.zip", mimeType: "application/zip", buffer: CAM_BUNDLE });
  const box = page.locator(".doc-app .app-box");
  await expect(box.locator(".app-status")).toHaveText("Running", { timeout: 60_000 });

  // Echo on, then the camera: our own picture, and the app's echo of it
  // in a tile of its own (channel 0).
  await page.evaluate(() => {
    const root = (window as unknown as { appRoots: ShadowRoot[] }).appRoots[0]!;
    (root.querySelector("button") as HTMLButtonElement).click();
  });
  const camera = box.locator(".app-camera-button");
  await expect(camera).toHaveText("Turn camera on");
  await camera.click();
  await expect(camera).toHaveText("Turn camera off", { timeout: 15_000 });
  await expect(box.locator(".app-video-self")).toBeVisible();
  await expect.poll(async () => Number(await box.getAttribute("data-video-sent")), { timeout: 15_000 }).toBeGreaterThan(5);
  await expect.poll(async () => Number(await box.getAttribute("data-video-shown")), { timeout: 15_000 }).toBeGreaterThan(5);
  await expect(box.locator('.app-video-tile[data-channel="0"]')).toBeVisible();
  await shoot(page, "apps-03-camera");
  await camera.click();
  await expect(camera).toHaveText("Turn camera on");
  await expect(box.locator(".app-video-self")).toBeHidden();
  expect(problems).toEqual([]);
  await context.close();
});
