// Wave apps with sound and video: the walkie examples with Echo on.
// Chromium's fake microphone and camera feed our buttons; the frames go to
// the app on the node, which sends them back to be played and shown. Each
// test runs over WebRTC and over the client socket (forced with the
// localStorage key of rtc.ts), and an unreachable WebRTC port falls back
// to the socket.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { type Browser, expect, type Page, test } from "@playwright/test";

import { shoot, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";
import { zip } from "./support/zip";

test.use({
  launchOptions: {
    args: ["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"],
  },
});

const WALKIE = readFileSync(
  resolve(fileURLToPath(new URL(".", import.meta.url)), "../../../crates/resonator-apps/tests/fixtures/walkie.wasm"),
);
const BUNDLE = zip([
  [
    "manifest.json",
    Buffer.from(
      JSON.stringify({
        name: "walkie",
        version: "1",
        permissions: ["peers", "streams", "media"],
      }),
    ),
  ],
  ["app.wasm", WALKIE],
]);

const WALKIE_CAM = readFileSync(
  resolve(
    fileURLToPath(new URL(".", import.meta.url)),
    "../../../crates/resonator-apps/tests/fixtures/walkie_cam.wasm",
  ),
);
const CAM_BUNDLE = zip([
  [
    "manifest.json",
    Buffer.from(
      JSON.stringify({
        name: "walkie-cam",
        version: "1",
        permissions: ["peers", "streams", "media", "camera"],
      }),
    ),
  ],
  ["app.wasm", WALKIE_CAM],
]);

let servers: TestServers;

test.beforeAll(async () => {
  servers = await TestServers.start({ device: true, apps: true, webrtc: true });
});
test.afterAll(async () => {
  await servers?.stop();
});

type Path = "rtc" | "socket";

/** A page signed in to `url`, keeping the app's shadow roots for the test. */
async function openPage(
  browser: Browser,
  url: string,
  path: Path,
): Promise<{ page: Page; problems: string[]; close: () => Promise<void> }> {
  const context = await browser.newContext({
    permissions: ["microphone", "camera"],
  });
  await context.addInitScript((socket: boolean) => {
    const roots: ShadowRoot[] = [];
    (window as unknown as { appRoots: ShadowRoot[] }).appRoots = roots;
    const attach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function (init: ShadowRootInit) {
      const root = attach.call(this, init);
      roots.push(root);
      return root;
    };
    if (socket) {
      localStorage.setItem("resonator.app-media", "socket");
    }
  }, path === "socket");
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(url);
  await page.locator(".topbar").waitFor();
  return { page, problems, close: () => context.close() };
}

/** Inserts the bundle in a new wave and waits for it to run, then turns Echo on. */
async function insertApp(page: Page, title: string, file: string, bundle: Buffer) {
  await page.locator("[data-action=new-wave]").click();
  await expect(page.locator(".blip-editing .doc[contenteditable=true]")).toBeFocused();
  await page.keyboard.type(title);
  await page.keyboard.press("Enter");
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('.edit-toolbar [data-command="app"]').click(),
  ]);
  await chooser.setFiles({
    name: file,
    mimeType: "application/zip",
    buffer: bundle,
  });
  const box = page.locator(".doc-app .app-box");
  await expect(box.locator(".app-status")).toHaveText("Running", {
    timeout: 60_000,
  });
  await page.evaluate(() => {
    const root = (window as unknown as { appRoots: ShadowRoot[] }).appRoots[0]!;
    (root.querySelector("button") as HTMLButtonElement).click();
  });
  return box;
}

const supports = async (page: Page, path: Path, what: "audio" | "video"): Promise<boolean> =>
  page.evaluate(
    ([path, what]) =>
      path === "rtc"
        ? typeof RTCPeerConnection !== "undefined"
        : what === "audio"
          ? typeof AudioEncoder !== "undefined"
          : typeof VideoEncoder !== "undefined",
    [path, what] as const,
  );

for (const path of ["rtc", "socket"] as const) {
  test(`the Mic button feeds the app, and what it sends back is played (${path})`, async ({ browser }) => {
    const { page, problems, close } = await openPage(browser, servers.deviceUrl!, path);
    test.skip(!(await supports(page, path, "audio")), `this browser cannot send sound over the ${path}`);
    const box = await insertApp(page, "Walkie", "walkie.zip", BUNDLE);
    await expect(box).toHaveAttribute("data-media-path", path, {
      timeout: 15_000,
    });

    const mic = box.locator(".app-mic-button");
    await expect(mic).toHaveText("Turn microphone on");
    await mic.click();
    await expect(mic).toHaveText("Turn microphone off", { timeout: 15_000 });
    await expect
      .poll(async () => Number(await box.getAttribute("data-media-sent")), {
        timeout: 15_000,
      })
      .toBeGreaterThan(5);
    await expect
      .poll(async () => Number(await box.getAttribute("data-media-played")), {
        timeout: 15_000,
      })
      .toBeGreaterThan(5);
    await mic.click();
    await expect(mic).toHaveText("Turn microphone on");
    expect(problems).toEqual([]);
    await close();
  });

  test(`the camera button feeds the app, and the video it sends back is shown (${path})`, async ({ browser }) => {
    const { page, problems, close } = await openPage(browser, servers.deviceUrl!, path);
    test.skip(!(await supports(page, path, "video")), `this browser cannot send video over the ${path}`);
    const box = await insertApp(page, "Walkie cam", "walkie-cam.zip", CAM_BUNDLE);
    await expect(box).toHaveAttribute("data-media-path", path, {
      timeout: 15_000,
    });

    // Our own picture, and the app's echo of it in a tile of its own
    // (channel 0).
    const camera = box.locator(".app-camera-button");
    await expect(camera).toHaveText("Turn camera on");
    await camera.click();
    await expect(camera).toHaveText("Turn camera off", { timeout: 15_000 });
    await expect(box.locator(".app-video-self")).toBeVisible();
    await expect
      .poll(async () => Number(await box.getAttribute("data-video-sent")), {
        timeout: 15_000,
      })
      .toBeGreaterThan(5);
    await expect
      .poll(async () => Number(await box.getAttribute("data-video-shown")), {
        timeout: 15_000,
      })
      .toBeGreaterThan(5);
    await expect(box.locator('.app-video-tile[data-channel="0"]')).toBeVisible();
    if (path === "rtc") {
      await shoot(page, "apps-03-camera");
    }
    await camera.click();
    await expect(camera).toHaveText("Turn camera on");
    await expect(box.locator(".app-video-self")).toBeHidden();
    expect(problems).toEqual([]);
    await close();
  });
}

test("where the WebRTC port cannot be reached, sound goes over the socket", async ({ browser }) => {
  const unreachable = await TestServers.start({
    device: true,
    apps: true,
    webrtc: "unreachable",
  });
  try {
    const { page, problems, close } = await openPage(browser, unreachable.deviceUrl!, "rtc");
    test.skip(!(await supports(page, "socket", "audio")), "this browser has no WebCodecs audio");
    const box = await insertApp(page, "Walkie", "walkie.zip", BUNDLE);
    await expect(box).toHaveAttribute("data-media-path", "connecting");
    await expect(box).toHaveAttribute("data-media-path", "socket", {
      timeout: 15_000,
    });
    const mic = box.locator(".app-mic-button");
    await mic.click();
    await expect(mic).toHaveText("Turn microphone off", { timeout: 15_000 });
    await expect
      .poll(async () => Number(await box.getAttribute("data-media-played")), {
        timeout: 15_000,
      })
      .toBeGreaterThan(5);
    await mic.click();
    expect(problems).toEqual([]);
    await close();
  } finally {
    await unreachable.stop();
  }
});
