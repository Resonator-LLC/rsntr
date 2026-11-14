// WCAG 2.2 AA, checked by axe on every screen: sign-in, the list of waves,
// a wave read and edited, in light and dark, on desktop Chromium and on an
// iPhone in WebKit. Any violation fails.

import AxeBuilder from "@axe-core/playwright";
import { type Browser, type BrowserContextOptions, type Page, chromium, devices, expect, test, webkit } from "@playwright/test";

import { type User, createUser, signIn } from "./support/app";
import { TestServers } from "./support/servers";

const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"];

let servers: TestServers;
let alice: User;
let bob: User;
let waveId = "";
const browsers: Record<string, Browser> = {};

const CLIENTS: Record<string, { engine: "chromium" | "webkit"; options: BrowserContextOptions }> = {
  desktop: { engine: "chromium", options: { viewport: { width: 1280, height: 860 } } },
  iphone: { engine: "webkit", options: { ...devices["iPhone 15"] } },
};

test.beforeAll(async () => {
  servers = await TestServers.start();
  alice = await createUser(servers, "alice");
  bob = await createUser(servers, "bob");
  browsers.chromium = await chromium.launch();
  browsers.webkit = await webkit.launch();
  // A wave with a reply, a nested reply, a link, and something unread for Alice.
  const page = await browsers.chromium.newPage();
  await page.goto(servers.baseUrl);
  await signIn(page, bob);
  await page.locator("[data-action=new-wave]").click();
  await page.keyboard.type("Launch plan");
  await page.keyboard.press("Enter");
  await page.keyboard.type("The venue is booked for Friday, details at https://rsntr.net today.");
  await page.locator(".edit-done").click();
  await page.locator(".wave-add-participant").click();
  await page.locator("#wave-add-participant-input").fill(alice.address);
  await page.locator("#wave-add-participant-input").press("Enter");
  await expect(page.locator(`.wave-participant[data-participant="${alice.address}"]`)).toBeVisible();
  await page.locator(".reply-box").click();
  await page.keyboard.type("Can we start at seven instead?");
  await page.keyboard.press("Shift+Enter");
  await page.locator(".thread-root > .blip").first().locator(".blip-main").first().focus();
  await page.keyboard.press("Enter");
  await page.keyboard.type("Seven works.");
  await page.keyboard.press("Shift+Enter");
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
  waveId = decodeURIComponent(new URL(page.url()).hash.slice(1));
  await page.close();
});
test.afterAll(async () => {
  for (const b of Object.values(browsers)) {
    await b.close();
  }
  await servers?.stop();
});

async function violations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  return result.violations.map(
    (v) => `${v.id} (${v.impact}): ${v.nodes.slice(0, 3).map((n) => n.target.join(" ")).join(", ")}`,
  );
}

for (const [client, { engine, options }] of Object.entries(CLIENTS)) {
  for (const colorScheme of ["light", "dark"] as const) {
    test(`${client} ${colorScheme}: no WCAG violations on any screen`, async () => {
      const context = await browsers[engine]!.newContext({ ...options, colorScheme });
      const page = await context.newPage();
      await page.goto(servers.baseUrl);
      await page.locator(".auth-form").waitFor();
      expect.soft(await violations(page), "sign-in").toEqual([]);

      await signIn(page, alice);
      await expect(page.locator(".digest").first()).toBeVisible();
      expect.soft(await violations(page), "list").toEqual([]);

      await page.locator(".digest-link").first().click();
      await expect(page.locator(".blip-main").nth(2)).toBeVisible();
      expect.soft(await violations(page), "wave").toEqual([]);

      await page.locator(".blip-main").first().focus();
      await page.keyboard.press("Control+e");
      await expect(page.locator(".blip-editing .doc[contenteditable=true]")).toBeFocused();
      expect.soft(await violations(page), "wave, editing").toEqual([]);
      await page.keyboard.press("Escape");
      expect(waveId).not.toBe("");
      await context.close();
    });
  }
}
