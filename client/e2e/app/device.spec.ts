// The application against a device node (`--mode device`): the page the
// app wrapping the node opens (`/?token=`) lands in the inbox without a
// sign-in screen, the bar names the person and this device and has no
// sign-out, and @mention suggestions come from the contacts.

import { type BrowserContext, type Page, expect, test } from "@playwright/test";

import { shoot, watchProblems } from "./support/app";
import { DOMAIN, TestServers } from "./support/servers";

test.describe.configure({ mode: "serial" });

const ME = `me@${DOMAIN}`;
const ZOE = `zoe@${DOMAIN}`;
const YURI = `yuri@other.example`;

let servers: TestServers;
const opened: BrowserContext[] = [];

/** A session cookie of the device's account, from the token (as the app's WebView gets it). */
async function deviceCookie(): Promise<string> {
  const token = new URL(servers.deviceUrl!).searchParams.get("token")!;
  const response = await fetch(`${servers.backendUrl}/?token=${encodeURIComponent(token)}`, {
    redirect: "manual",
  });
  expect(response.status).toBe(303);
  return (response.headers.get("set-cookie") ?? "").split(";")[0]!;
}

test.beforeAll(async () => {
  servers = await TestServers.start({ device: true });
  expect(servers.deviceUrl).not.toBeNull();
  // Contacts added by address, as from a shared `resonator:contact/` link.
  const cookie = await deviceCookie();
  for (const body of [{ address: ZOE }, { link: `resonator:contact/${YURI}` }]) {
    const response = await fetch(`${servers.backendUrl}/contacts`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json",
        cookie,
        origin: servers.backendUrl,
      },
      body: JSON.stringify(body),
    });
    // The other domain is probed (it does not federate here) and added all the same.
    expect(response.status, await response.text()).toBe(201);
  }
});
test.afterAll(async () => {
  for (const c of opened) {
    await c.close().catch(() => {});
  }
  await servers?.stop();
});

const editing = (page: Page) => page.locator(".blip-editing .doc[contenteditable=true]");

async function devicePage(
  browser: import("@playwright/test").Browser,
  options: Parameters<import("@playwright/test").Browser["newContext"]>[0] = {},
) {
  const context = await browser.newContext(options);
  opened.push(context);
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.deviceUrl!);
  await page.locator(".topbar").waitFor();
  return { context, page, problems };
}

test("without the token: no sign-in form, a pointer to the app", async ({ browser }) => {
  const context = await browser.newContext();
  opened.push(context);
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.baseUrl);
  await expect(page.locator(".auth-device")).toContainText("Open Resonator from its app");
  await expect(page.locator(".auth-form")).toHaveCount(0);
  // A wrong token is refused.
  const refused = await page.goto(`${servers.baseUrl}/?token=wrong`);
  expect(refused?.status()).toBe(403);
  expect(problems.filter((p) => p !== "http 403 /")).toEqual([]);
  await context.close();
});

test("the token opens the inbox signed in; a wave with an @mention from the contacts", async ({ browser }) => {
  const { context, page, problems } = await devicePage(browser);
  // The token is gone from the address bar: the server redirected to `/`.
  expect(new URL(page.url()).search).toBe("");
  await expect(page.locator(".auth-form")).toHaveCount(0);
  await expect(page.locator(".topbar-device")).toHaveCount(0);
  await expect(page.locator(".topbar-signout")).toHaveCount(0);
  await expect(page.locator(".search-empty")).toBeVisible();
  await expect(page.locator(".topbar-net")).toHaveText(/Online/);
  // This device node is not in person mode: Settings has the address, and
  // asking for the person is answered 404.
  await page.locator(".topbar-settings").click();
  await expect(page.locator("[data-testid=my-address]")).toHaveText(ME);
  await page.locator(".settings-back").click();

  await page.locator("[data-action=new-wave]").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("From my phone");
  await page.keyboard.press("Enter");
  await page.keyboard.type("Hello @zo");
  const options = page.locator(".mention-option");
  await expect(options).toHaveCount(1);
  await expect(options.first()).toHaveAttribute("data-address", ZOE);
  await page.keyboard.press("Enter");
  await expect(editing(page).locator(".doc-mention")).toHaveAttribute("data-mention", ZOE);
  await expect(page.locator(`.wave-participant[data-participant="${ZOE}"]`)).toBeVisible();
  // The contact of the other domain is suggested too.
  await page.keyboard.type(" and @yu");
  await expect(options.first()).toHaveAttribute("data-address", YURI);
  await page.keyboard.press("Escape");
  await page.locator("[data-command=done]").click();
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
  await expect(page.locator(".wave-title")).toHaveText("From my phone");
  await expect(page.locator(".digest-link")).toHaveCount(1);
  await shoot(page, "device-01-wave");
  // The directory was never asked: contacts stand in for it.
  expect(problems.filter((p) => p !== "http 404 /person")).toEqual([]);
  await context.close();
});

test("on a phone-sized screen the bar fits", async ({ browser }) => {
  const { context, page, problems } = await devicePage(browser, {
    viewport: { width: 375, height: 667 },
    isMobile: true,
    hasTouch: true,
  });
  await expect(page.locator(".topbar-signout")).toHaveCount(0);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shoot(page, "device-02-phone");
  expect(problems).toEqual([]);
  await context.close();
});
