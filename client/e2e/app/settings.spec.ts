// The settings page of a device node in person mode (`--mode device`
// with federation and `[person]` on): this person and device, pairing
// (a short code, a QR code and link) and joining by a code, the recovery
// phrase behind a confirmation, and contacts; on a phone-sized screen
// too, without sideways scrolling. Pairing codes go to a pkarr relay in
// miniature (see `support/servers.ts`).

import { type Browser, type BrowserContext, type Page, expect, test } from "@playwright/test";

import { shoot, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";

test.describe.configure({ mode: "serial" });

let servers: TestServers;
const opened: BrowserContext[] = [];

test.beforeAll(async () => {
  servers = await TestServers.start({ device: true, person: true });
  expect(servers.deviceUrl).not.toBeNull();
});
test.afterAll(async () => {
  for (const c of opened) {
    await c.close().catch(() => {});
  }
  await servers?.stop();
});

async function settingsPage(browser: Browser, options: Parameters<Browser["newContext"]>[0] = {}) {
  const context = await browser.newContext(options);
  opened.push(context);
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.deviceUrl!);
  await page.locator(".topbar").waitFor();
  await page.locator(".topbar-settings").click();
  await expect(page.locator(".settings-page")).toBeVisible();
  await expect(page.locator("[data-section=devices]")).toBeVisible();
  return { context, page, problems };
}

/** How far the page scrolls sideways (0: not at all). */
async function sideways(page: Page): Promise<number> {
  return page.evaluate(() => {
    const scroll = document.querySelector(".settings-scroll");
    const inner = scroll === null ? 0 : scroll.scrollWidth - scroll.clientWidth;
    return Math.max(document.documentElement.scrollWidth - window.innerWidth, inner);
  });
}

test("the person, this device and a pairing link with its QR code", async ({ browser }) => {
  const { context, page, problems } = await settingsPage(browser);
  expect(new URL(page.url()).hash).toBe("#settings");
  // The person domain is the account's domain.
  const address = (await page.locator("[data-testid=my-address]").textContent()) ?? "";
  const domain = address.split("@")[1]?.split(" ")[0] ?? "";
  expect(domain.length).toBe(52);
  await expect(page.locator(".settings-person")).toHaveText(domain);
  const self = page.locator(".settings-device").first();
  await expect(self).toContainText("e2e phone");
  await expect(self).toContainText("this device");
  await expect(self).toContainText("admin");
  await expect(page.locator(".settings-lease")).toHaveText("This device hosts your waves.");

  await page.locator("[data-action=pair]").click();
  await expect(page.locator(".settings-pair-link")).toHaveValue(/^resonator:pair\/\S+$/);
  await expect(page.locator(".settings-qr svg")).toBeVisible();
  await shoot(page, "settings-01-pair");
  await page.locator("[data-action=cancel-invitation]").click();
  await expect(page.locator(".settings-pair-link")).toHaveCount(0);
  await expect(page.locator("[data-action=pair]")).toBeVisible();

  // The phrase only after confirming, and gone once hidden.
  await page.locator("[data-action=ask-phrase]").click();
  await expect(page.locator(".settings-phrase")).toHaveCount(0);
  await page.locator("[data-action=reveal-phrase]").click();
  await expect(page.locator(".settings-phrase li")).toHaveCount(24);
  await page.locator("[data-action=hide-phrase]").click();
  await expect(page.locator(".settings-phrase")).toHaveCount(0);

  // Back to the waves.
  await page.locator(".settings-back").click();
  await expect(page.locator(".settings-page")).toHaveCount(0);
  await expect(page.locator(".app-search")).toBeVisible();
  expect(problems).toEqual([]);
  await context.close();
});

/** The code an invitation shows, as typed: `<digits>-<word>-<word>`. */
async function shownCode(page: Page): Promise<string> {
  await expect(page.locator("[data-testid=pair-code] .settings-code-group")).toHaveCount(3);
  const groups = await page.locator("[data-testid=pair-code] .settings-code-group").allTextContents();
  expect(groups).toHaveLength(3);
  return groups.join("-");
}

test("the inviting device shows a short code, with the time left and a copy button", async ({ browser }) => {
  const { context, page, problems } = await settingsPage(browser);
  await page.locator("[data-action=pair]").click();
  const code = page.locator("[data-testid=pair-code]");
  await expect(code).toBeVisible();
  expect(await shownCode(page)).toMatch(/^\d{6}-[a-z]{3,8}-[a-z]{3,8}$/);
  await expect(page.locator("[data-testid=pair-remaining]")).toHaveText(/^Expires in (10:00|9:[0-5]\d)$/);
  await expect(page.locator("[data-action=copy-code]")).toBeVisible();
  // The QR code and link are still there, below the code.
  await expect(page.locator(".settings-qr svg")).toBeVisible();
  await expect(page.locator(".settings-pair-link")).toHaveValue(/^resonator:pair\//);
  const codeBox = await code.boundingBox();
  const qrBox = await page.locator(".settings-qr").boundingBox();
  expect(codeBox!.y).toBeLessThan(qrBox!.y);
  // Big and monospace.
  const style = await code.evaluate((el) => {
    const s = getComputedStyle(el);
    return { size: parseFloat(s.fontSize), family: s.fontFamily };
  });
  expect(style.size).toBeGreaterThanOrEqual(20);
  expect(style.family).toMatch(/mono/i);
  await shoot(page, "settings-03-pair-code");
  await page.locator("[data-action=cancel-invitation]").click();
  await expect(code).toHaveCount(0);
  expect(problems).toEqual([]);
  await context.close();
});

test("joining with a malformed, unknown or own code says why", async ({ browser }) => {
  const { context, page, problems } = await settingsPage(browser);
  const join = page.locator("[data-section=join]");
  await expect(join.locator("h3")).toHaveText("Join with a code");
  const input = join.locator("input[name=pair-code]");
  await expect(input).toHaveAttribute("autocapitalize", "off");
  await expect(input).toHaveAttribute("autocorrect", "off");
  await expect(input).toHaveJSProperty("spellcheck", false);
  await expect(input).toHaveAttribute("inputmode", "text");

  // Malformed: refused here, nothing sent.
  await input.fill("12345 ocean tiger");
  await join.locator("[data-action=join]").click();
  await expect(page.locator(".settings-message")).toContainText("6 digits and 2 words");
  // A word that is not a code word, told as it is typed.
  await input.fill("482913 ocean tigr ");
  await expect(join.locator(".settings-code-hint")).toHaveText('"tigr" is not one of the code words.');
  // Suggestions complete a word.
  await input.fill("482913 oce");
  await join.locator("[data-suggestion=ocean]").click();
  await expect(input).toHaveValue("482913-ocean-");

  // Unknown: no device published that number.
  await input.fill("000017-abandon-zoo");
  await join.locator("[data-action=join]").click();
  await expect(page.locator(".settings-message")).toContainText("Code not found or expired", { timeout: 20_000 });

  // This device's own code: found through the rendezvous, then refused.
  await page.locator("[data-action=pair]").click();
  const own = await shownCode(page);
  await input.fill(own.toUpperCase().replaceAll("-", " "));
  await join.locator("[data-action=join]").click();
  await expect(page.locator(".settings-message")).toContainText("inviting device itself", { timeout: 20_000 });
  await page.locator("[data-action=cancel-invitation]").click();
  // The two refusals above are the only failed requests.
  expect(problems).toEqual(["http 404 /person/pair/join", "http 409 /person/pair/join"]);
  await context.close();
});

test("contacts are added by address and by link, and removed", async ({ browser }) => {
  const { context, page, problems } = await settingsPage(browser);
  const input = page.locator("input[name=contact]");
  await input.fill("zoe@friends.example");
  await page.locator("[data-action=add-contact]").click();
  await expect(page.locator('.settings-contact[data-address="zoe@friends.example"]')).toBeVisible({ timeout: 20_000 });
  await expect(input).toHaveValue("");
  await input.fill("resonator:contact/yuri@other.example");
  await page.locator("[data-action=add-contact]").click();
  await expect(page.locator('.settings-contact[data-address="yuri@other.example"]')).toBeVisible({ timeout: 20_000 });
  await page.locator('.settings-contact[data-address="zoe@friends.example"] [data-action=remove-contact]').click();
  await expect(page.locator('.settings-contact[data-address="zoe@friends.example"]')).toHaveCount(0);
  await expect(page.locator(".settings-contact")).toHaveCount(1);
  expect(problems).toEqual([]);
  await context.close();
});

test("on a phone-sized screen: app-like, no sideways scrolling", async ({ browser }) => {
  const { context, page, problems } = await settingsPage(browser, {
    viewport: { width: 375, height: 667 },
    isMobile: true,
    hasTouch: true,
  });
  // The settings take the whole width; the panels are gone.
  await expect(page.locator(".app-search")).toHaveCount(0);
  const box = await page.locator(".settings-page").boundingBox();
  expect(box?.width).toBeGreaterThanOrEqual(370);
  expect(await sideways(page)).toBeLessThanOrEqual(0);
  await page.locator("[data-action=pair]").click();
  await expect(page.locator(".settings-pair-link")).toHaveValue(/^resonator:pair\//);
  await expect(page.locator("[data-testid=pair-code]")).toBeVisible();
  expect(await sideways(page)).toBeLessThanOrEqual(0);
  await shoot(page, "settings-04-phone-code");
  // Typing a code: 16px (no zoom on iOS), suggestions, no sideways scroll.
  const codeInput = page.locator("input[name=pair-code]");
  await codeInput.scrollIntoViewIfNeeded();
  expect(await codeInput.evaluate((el) => parseFloat(getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(16);
  await codeInput.fill("482913 ti");
  await expect(page.locator("[data-suggestion]")).toHaveCount(4);
  expect(await sideways(page)).toBeLessThanOrEqual(0);
  await shoot(page, "settings-05-phone-join");
  await codeInput.fill("");
  await page.locator("[data-action=ask-phrase]").click();
  await page.locator("[data-action=reveal-phrase]").click();
  await expect(page.locator(".settings-phrase li")).toHaveCount(24);
  expect(await sideways(page)).toBeLessThanOrEqual(0);
  await page.locator("[data-action=hide-phrase]").click();
  await page.locator("[data-section=contacts]").scrollIntoViewIfNeeded();
  await shoot(page, "settings-02-phone");
  expect(await sideways(page)).toBeLessThanOrEqual(0);
  // The browser's back button leaves the settings.
  await page.goBack();
  await expect(page.locator(".settings-page")).toHaveCount(0);
  expect(problems).toEqual([]);
  await context.close();
});
