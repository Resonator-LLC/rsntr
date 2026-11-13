// The application on phones, in WebKit with the iPhone device profiles:
// it must behave like an app. Focusing the body of a new wave must not
// zoom the page (iOS zooms into inputs and editable text under 16px) nor
// let it scroll sideways (a row wider than the screen), which cut off the
// left of the page up to the "+" of the participants bar.

import { type Browser, type BrowserContext, type Page, devices, expect, test } from "@playwright/test";

import { type User, createUser, signIn, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";

test.use({ browserName: "webkit" });
test.describe.configure({ mode: "serial" });

const SHOTS = process.env.RESONATOR_MOBILE_SHOTS ?? "mobile";
const SCREENS =
  process.env.RESONATOR_SCREENS ??
  "e2e/.results/screens";

let servers: TestServers;
let alice: User;
let bob: User;
const opened: BrowserContext[] = [];

test.beforeAll(async () => {
  servers = await TestServers.start();
  alice = await createUser(servers, "alice");
  bob = await createUser(servers, "bob");
});
test.afterAll(async () => {
  for (const c of opened) {
    await c.close().catch(() => {});
  }
  await servers?.stop();
});

/** The phone profiles to check: the iPhone 15, and an iPhone SE sized screen. */
const PHONES = {
  "390x844": devices["iPhone 15"],
  "375x667": { ...devices["iPhone SE"], viewport: { width: 375, height: 667 } },
} as const;

/** While a screenshot is taken: Playwright injects a stylesheet, which the CSP refuses. */
let shooting = false;

/**
 * Problems on the page, less WebKit not knowing a viewport key Chrome does,
 * and the screenshots' stylesheets.
 */
function problemsOf(page: Page): string[] {
  const problems = watchProblems(page);
  page.on("console", (m) => {
    const text = m.text();
    if (
      text.includes('"interactive-widget" not recognized') ||
      (shooting && text.startsWith("Refused to apply a stylesheet"))
    ) {
      const i = problems.indexOf(`console: ${text}`);
      if (i >= 0) problems.splice(i, 1);
    }
  });
  return problems;
}

async function phone(browser: Browser, size: keyof typeof PHONES): Promise<{ context: BrowserContext; page: Page }> {
  const { defaultBrowserType: _, ...profile } = PHONES[size];
  const context = await browser.newContext(profile);
  opened.push(context);
  const page = await context.newPage();
  return { context, page };
}

async function shot(page: Page, name: string): Promise<void> {
  shooting = true;
  try {
    await page.screenshot({ path: `${SCREENS}/${SHOTS}/${name}.png` });
    // Let the console message of the refused stylesheet arrive.
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(done)));
  } finally {
    shooting = false;
  }
}

interface Fit {
  readonly scrollWidth: number;
  readonly innerWidth: number;
  readonly scale: number;
  readonly pageLeft: number;
  readonly offsetLeft: number;
  readonly scrollX: number;
}

async function fit(page: Page): Promise<Fit> {
  return page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
    innerWidth: window.innerWidth,
    scale: window.visualViewport?.scale ?? 1,
    pageLeft: window.visualViewport?.pageLeft ?? 0,
    offsetLeft: window.visualViewport?.offsetLeft ?? 0,
    scrollX: window.scrollX,
  }));
}

/** No sideways scrolling, no zoom, nothing scrolled out on the left. */
async function expectFits(page: Page, where: string): Promise<void> {
  const f = await fit(page);
  expect.soft(f.scrollWidth, `${where}: ${JSON.stringify(f)}`).toBeLessThanOrEqual(f.innerWidth);
  expect.soft(f.scale, `${where}: ${JSON.stringify(f)}`).toBe(1);
  expect.soft(f.pageLeft + f.scrollX, `${where}: ${JSON.stringify(f)}`).toBe(0);
}

/** Controls with a box smaller than `min` on a side, visible ones only. */
async function smallTargets(page: Page, selector: string, min = 44): Promise<string[]> {
  return page.evaluate(
    ({ selector, min }) =>
      [...document.querySelectorAll<HTMLElement>(selector)]
        .filter((el) => el.offsetParent !== null)
        .map((el) => ({ el, r: el.getBoundingClientRect() }))
        .filter(({ r }) => r.width > 0 && (r.width < min - 0.5 || r.height < min - 0.5))
        .map(
          ({ el, r }) =>
            `${el.className || el.tagName} ${el.getAttribute("aria-label") ?? el.textContent?.trim() ?? ""} ${Math.round(r.width)}x${Math.round(r.height)}`,
        ),
    { selector, min },
  );
}

/** Font sizes under 16px of the fields one can type into. */
async function smallFields(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>("input, textarea, select, [contenteditable=true]")]
      .filter((el) => el.offsetParent !== null)
      .map((el) => ({ el, size: Number.parseFloat(getComputedStyle(el).fontSize) }))
      .filter(({ size }) => size < 16)
      .map(({ el, size }) => `${el.tagName}.${el.className} ${size}px`),
  );
}

for (const size of Object.keys(PHONES) as (keyof typeof PHONES)[]) {
  test(`${size}: sign-in, new wave, tap the body: no zoom, no sideways scrolling`, async ({ browser }) => {
    const { page } = await phone(browser, size);
    const problems = problemsOf(page);
    await page.goto(servers.baseUrl);
    await page.locator(".auth-form").waitFor();
    await shot(page, `${size}-01-sign-in`);
    expect.soft(await smallFields(page)).toEqual([]);
    expect.soft(await smallTargets(page, ".auth-form button, .auth-form input")).toEqual([]);
    await page.getByLabel("Username").tap();
    await expectFits(page, "sign-in form focused");
    await signIn(page, alice);
    await expectFits(page, "wave list");
    await shot(page, `${size}-02-wave-list`);
    expect.soft(await smallTargets(page, ".topbar button, .search-new, .search-folders button, .digest-link")).toEqual([]);

    await page.locator("[data-action=new-wave]").tap();
    const body = page.locator(".blip-editing .doc[contenteditable=true]");
    await expect(body).toBeVisible();
    await shot(page, `${size}-03-new-wave`);
    await expectFits(page, "new wave");
    await body.tap();
    await page.keyboard.type("Holiday plans");
    // Let a zoom or a scroll into the caret happen if it is going to.
    await page.waitForTimeout(400);
    await shot(page, `${size}-04-new-wave-body-tapped`);
    await expectFits(page, "new wave, body tapped");
    expect.soft(await smallFields(page)).toEqual([]);
    await expect.soft(page.locator(".wave-add-participant")).toBeInViewport({ ratio: 1 });
    await expect.soft(page.locator(".wave-back")).toBeInViewport({ ratio: 1 });
    expect.soft(await smallTargets(page, ".wave-header button, .wave-participants button, .edit-toolbar .edit-button"),
    ).toEqual([]);

    // The add-participant dialog.
    await page.locator(".wave-add-participant").tap();
    const input = page.locator("#wave-add-participant-input");
    await input.tap();
    await input.fill(bob.name.slice(0, 4));
    await expect(page.locator(`.suggestion[data-address="${bob.address}"]`)).toBeVisible();
    await page.waitForTimeout(300);
    await shot(page, `${size}-05-add-participant`);
    await expectFits(page, "add-participant dialog");
    expect.soft(await smallFields(page)).toEqual([]);
    expect.soft(await smallTargets(page, "dialog button")).toEqual([]);
    await page.locator(`.suggestion[data-address="${bob.address}"]`).tap();
    await page.getByRole("button", { name: "Close" }).tap();
    await expect(page.locator(`.wave-participant[data-participant="${bob.address}"]`)).toBeVisible();

    // The open wave, read mode.
    await page.locator("[data-command=done]").tap();
    await expect(page.locator(".wave-toolbar")).toBeVisible();
    await page.waitForTimeout(300);
    await shot(page, `${size}-06-open-wave`);
    await expectFits(page, "open wave");
    expect.soft(await smallTargets(page, ".wave-toolbar button, .blip-menu-btn, .reply-box")).toEqual([]);
    expect(problems).toEqual([]);
  });
}

test("390x844: writing mode: with the keyboard up only the toolbar and the text stay", async ({ browser }) => {
  const { page } = await phone(browser, "390x844");
  const problems = problemsOf(page);
  await page.goto(servers.baseUrl);
  await signIn(page, alice);
  await page.locator("[data-action=new-wave]").tap();
  const body = page.locator(".blip-editing .doc[contenteditable=true]");
  await body.tap();
  await page.keyboard.type("Notes for the trip");
  // WebKit here has no on-screen keyboard: mark it open as viewport.ts
  // does when the visual viewport shrinks.
  await page.evaluate(() => document.documentElement.classList.add("keyboard-open"));
  for (const hidden of [".topbar", ".wave-header", ".wave-participants", ".blip-editing .blip-meta"]) {
    await expect.soft(page.locator(hidden)).toBeHidden();
  }
  const done = page.locator(".edit-done");
  await expect(done).toBeInViewport({ ratio: 1 });
  await expect(page.locator(".edit-toolbar")).toBeInViewport();
  await shot(page, "390x844-08-writing-mode");
  await expectFits(page, "writing mode");

  // Enter twice (iOS sends the second as Shift+Enter) makes lines, and
  // never ends the edit.
  await page.keyboard.press("Enter");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.type("Day one");
  await expect(page.locator(".blip-editing")).toHaveCount(1);
  await expect(body).toContainText("Day one");

  // The keyboard goes: everything is back.
  await page.evaluate(() => document.documentElement.classList.remove("keyboard-open"));
  await expect(page.locator(".wave-participants")).toBeVisible();
  await expect(page.locator(".topbar")).toBeVisible();

  // Done from the pinned button ends the edit.
  await page.evaluate(() => document.documentElement.classList.add("keyboard-open"));
  await done.tap();
  await expect(page.locator(".blip-editing")).toHaveCount(0);
  expect(problems).toEqual([]);
});

test("390x844: @mention a person from the participants, by touch", async ({ browser }) => {
  const { page } = await phone(browser, "390x844");
  const problems = problemsOf(page);
  await page.goto(servers.baseUrl);
  await signIn(page, alice);
  await page.locator("[data-action=new-wave]").tap();
  const body = page.locator(".blip-editing .doc[contenteditable=true]");
  await body.tap();
  await page.keyboard.type(`Lunch with @${bob.name.slice(0, 3)}`);
  const option = page.locator(`.mention-option[data-address="${bob.address}"]`);
  await expect(option).toBeVisible();
  await expect(page.locator(".mention-popup")).toBeInViewport({ ratio: 1 });
  await shot(page, "390x844-07-mention-popup");
  await option.tap();
  await expect(body.locator(".doc-mention")).toHaveAttribute("data-mention", bob.address);
  await page.keyboard.type("today");
  await expect(body).toContainText(new RegExp(`Lunch with @${bob.name} today`, "i"));
  // Mentioned and not in the wave: added.
  await expect(page.locator(`.wave-participant[data-participant="${bob.address}"]`)).toBeVisible();
  await shot(page, "390x844-08-mention-inserted");
  await expectFits(page, "mention inserted");
  expect(problems).toEqual([]);
});

test("390x844: the viewport is an app's: no zoom, safe areas, a manifest", async ({ browser }) => {
  const { page } = await phone(browser, "390x844");
  await page.goto(servers.baseUrl);
  const viewport = await page.locator('meta[name="viewport"]').getAttribute("content");
  expect(viewport).toContain("maximum-scale=1");
  expect(viewport).toContain("viewport-fit=cover");
  const manifest = await page.locator('link[rel="manifest"]').getAttribute("href");
  expect(manifest).not.toBeNull();
  const response = await page.request.get(new URL(manifest!, servers.baseUrl).href);
  expect(response.ok()).toBe(true);
  const json = (await response.json()) as { display: string; icons: unknown[] };
  expect(json.display).toBe("standalone");
  expect(json.icons.length).toBeGreaterThan(0);
  const touchAction = await page.evaluate(() => getComputedStyle(document.body).touchAction);
  expect(touchAction).toBe("manipulation");
});
