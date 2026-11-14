// Screenshots of the main screens at phone, tablet and desktop sizes, in
// light and dark, at 100% and 200% text and in high contrast: the list, a
// wave and Settings. A changed pixel fails: look at the
// diff, and when the change is wanted, `npm run test:visual:update` and
// review the new images before committing them. Run with
// `npm run test:visual` (see playwright.visual.config.ts).

import { type Page, expect, test } from "@playwright/test";

import { type User, createUser, signIn } from "../app/support/app";
import { TestServers } from "../app/support/servers";

const SIZES = {
  phone: { width: 390, height: 844, mobile: true },
  tablet: { width: 834, height: 1112, mobile: true },
  desktop: { width: 1280, height: 860, mobile: false },
} as const;
/** Text at 100% and 200%, and high contrast (the app's setting) at 100%. */
const LOOKS = {
  text100: { fontSize: "", contrast: false },
  text200: { fontSize: "200%", contrast: false },
  contrast: { fontSize: "", contrast: true },
} as const;

let servers: TestServers;
let anna: User;
let ben: User;

test.beforeAll(async ({ browser }) => {
  servers = await TestServers.start();
  anna = await createUser(servers, "anna", true);
  ben = await createUser(servers, "ben", true);
  const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
  await page.goto(servers.baseUrl);
  await signIn(page, ben);
  await page.locator("[data-action=new-wave]").click();
  await page.keyboard.type("Launch plan for the Friday evening at the riverside venue");
  await page.keyboard.press("Enter");
  await page.keyboard.type("The venue is booked for Friday. Details are at https://rsntr.net and in the attached notes.");
  await page.locator(".edit-done").click();
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
  await page.locator(".wave-add-participant").click();
  await page.locator("#wave-add-participant-input").fill(anna.address);
  await page.locator("#wave-add-participant-input").press("Enter");
  await expect(page.locator(`.wave-participant[data-participant="${anna.address}"]`)).toBeVisible();
  await page.locator(".reply-box").click();
  await page.keyboard.type("Can we start at seven instead?");
  await page.keyboard.press("Shift+Enter");
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
  await page.locator(".thread-root > .blip").first().locator(".blip-main").first().focus();
  await page.keyboard.press("Enter");
  await page.keyboard.type("Seven works for me.");
  await page.keyboard.press("Shift+Enter");
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
  await page.close();
});
test.afterAll(async () => {
  await servers?.stop();
});

/** Times change from run to run: they are covered, and get one width
 *  (a later hour is wider), so the text beside them does not move. */
const shot = (page: Page) => ({
  mask: [page.locator("time"), page.locator(".digest-date, .blip-time")],
  style: ".digest-date, .blip-time { display: inline-block; width: 4.5em; overflow: hidden; white-space: nowrap; }",
});

async function noSidewaysScroll(page: Page, where: string): Promise<void> {
  const overflow = await page.evaluate(() => {
    // Content of a scroller of its own (the phone toolbar) may run past the edge.
    const inScroller = (el: HTMLElement): boolean => {
      for (let p = el.parentElement; p !== null && p !== document.body; p = p.parentElement) {
        if (["auto", "scroll", "hidden", "clip"].includes(getComputedStyle(p).overflowX)) {
          return true;
        }
      }
      return false;
    };
    const wide = [...document.querySelectorAll<HTMLElement>("body *")]
      .filter((el) => el.offsetParent !== null && el.getBoundingClientRect().right > window.innerWidth + 1)
      .filter((el) => !inScroller(el))
      .slice(0, 5)
      .map((el) => `${el.tagName.toLowerCase()}.${el.className}`);
    return { scroll: document.documentElement.scrollWidth - window.innerWidth, wide };
  });
  expect.soft(overflow, where).toEqual({ scroll: 0, wide: [] });
}

for (const [size, { width, height, mobile }] of Object.entries(SIZES)) {
  for (const colorScheme of ["light", "dark"] as const) {
    for (const [look, { fontSize, contrast }] of Object.entries(LOOKS)) {
      const name = `${size}-${colorScheme}-${look}`;
      test(name, async ({ browser }) => {
        const context = await browser.newContext({
          viewport: { width, height },
          isMobile: mobile,
          hasTouch: mobile,
          colorScheme,
          reducedMotion: "reduce",
        });
        if (contrast) {
          await context.addInitScript(() => localStorage.setItem("resonator.prefs", '{"contrast":"more"}'));
        }
        const page = await context.newPage();
        await page.goto(servers.baseUrl);
        if (fontSize !== "") {
          // Text size as the browser's or the app's text-size setting sets it.
          await page.evaluate((value) => {
            document.documentElement.style.fontSize = value;
          }, fontSize);
        }
        await signIn(page, anna);
        await expect(page.locator(".digest")).toHaveCount(1);
        await expect(page.locator(".topbar-net")).toHaveText(/Online/);
        await expect(page).toHaveScreenshot(`${name}-list.png`, shot(page));
        await noSidewaysScroll(page, "list");

        await page.locator(".digest-link").first().click();
        await expect(page.locator(".blip-main")).toHaveCount(3);
        await expect(page).toHaveScreenshot(`${name}-wave.png`, shot(page));
        await noSidewaysScroll(page, "wave");

        await page.locator(".topbar-net").click();
        await expect(page.locator("[data-section=accessibility]")).toBeVisible();
        // The name and photo editor, loaded (the initials picture too).
        await expect(page.locator("[data-action=change-photo]")).toBeEnabled();
        await expect
          .poll(() => page.locator("img.avatar-xl").evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 0))
          .toBe(true);
        await expect(page).toHaveScreenshot(`${name}-settings.png`, shot(page));
        await noSidewaysScroll(page, "settings");
        await page.locator("[data-section=you]").scrollIntoViewIfNeeded();
        await expect(page).toHaveScreenshot(`${name}-you.png`, shot(page));
        await context.close();
      });
    }
  }
}
