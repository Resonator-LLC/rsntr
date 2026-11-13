// A change of host rewinds a wavelet's history (device plan, phase 1):
// the browser hears of it while it has edits on their way, takes the
// rewound deltas back, keeps its edits and sends them again. The server's
// test hook rewinds and replays on one server what a change of host does
// across several.

import { type Page, expect, test } from "@playwright/test";

import { type User, createUser, signedInPage } from "./support/app";
import { TestServers } from "./support/servers";

test.describe.configure({ mode: "serial" });

let servers: TestServers;
let alice: User;

test.beforeAll(async () => {
  servers = await TestServers.start({ hooks: true });
  alice = await createUser(servers, "alice");
});
test.afterAll(async () => {
  await servers?.stop();
});

const editing = (page: Page) =>
  page.locator(".blip-editing .doc[contenteditable=true]");

async function saved(page: Page): Promise<void> {
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", {
    timeout: 15_000,
  });
}

async function rewind(
  wave: string,
  back: number,
): Promise<{ fork: number; applied: number; skipped: number }> {
  const wavelet = `${wave}/~/conv+root`;
  const response = await fetch(
    `${servers.backendUrl}/e2e/rewind?wavelet=${encodeURIComponent(wavelet)}&back=${back}`,
    { method: "POST" },
  );
  if (!response.ok) {
    throw new Error(`rewind: ${response.status} ${await response.text()}`);
  }
  return (await response.json()) as {
    fork: number;
    applied: number;
    skipped: number;
  };
}

test("edits on their way survive a rewind of the history", async ({
  browser,
}) => {
  const a = await signedInPage(browser, servers, alice);
  const page = a.page;
  // What the server sends, from the reload on.
  const received: string[] = [];
  page.on("websocket", (socket) =>
    socket.on("framereceived", (frame) => received.push(String(frame.payload))),
  );
  await page.reload();
  await page.locator(".topbar").waitFor();
  await page.locator("[data-action=new-wave]").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Rewind plan");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Alpha.");
  await page.locator(".edit-done").click();
  await saved(page);
  const wave = decodeURIComponent(new URL(page.url()).hash.slice(1));
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Beta.");
  await page.keyboard.press("Shift+Enter");
  await saved(page);

  // A reply on its way: sent, not yet arrived.
  servers.holdUploads();
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Gamma.");
  await page.keyboard.press("Shift+Enter");
  await expect(page.locator(".topbar-saved")).toHaveText(/Unsaved|Saving/);
  await page.waitForTimeout(500);

  // The history goes back two deltas the browser has seen; they come
  // back rebased as a new host would apply them.
  const outcome = await rewind(wave, 2);
  expect(outcome.applied).toBe(2);
  // The browser hears of it (field 8 of the update: the reset version).
  await expect
    .poll(
      () =>
        received.filter(
          (f) => f.includes('"ProtocolWaveletUpdate"') && f.includes('"8":{'),
        ).length,
    )
    .toBe(1);
  servers.releaseUploads();

  await saved(page);
  const thread = page.locator(".thread-root");
  for (const text of ["Alpha.", "Beta.", "Gamma."]) {
    await expect(thread).toContainText(text);
  }
  await expect(thread.getByText("Gamma.", { exact: true })).toHaveCount(1);

  // The server has the same.
  await page.reload();
  await expect(page.locator(".wave-title")).toHaveText("Rewind plan");
  for (const text of ["Alpha.", "Beta.", "Gamma."]) {
    await expect(page.locator(".thread-root")).toContainText(text);
  }
  await expect(
    page.locator(".thread-root").getByText("Gamma.", { exact: true }),
  ).toHaveCount(1);
  expect(a.problems).toEqual([]);
  await a.context.close();
});
