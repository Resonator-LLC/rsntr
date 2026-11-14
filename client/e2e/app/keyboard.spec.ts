// The keyboard: nothing typed right after opening an editor is lost, and
// Tab never traps focus in a message (WCAG 2.1.1, 2.1.2).

import { type Page, expect, test } from "@playwright/test";

import { createUser, signIn, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";

let servers: TestServers;
test.beforeAll(async () => {
  servers = await TestServers.start();
});
test.afterAll(async () => {
  await servers?.stop();
});

const editing = (page: Page) => page.locator(".blip-editing .doc[contenteditable=true]");
const blips = (page: Page) => page.locator(".thread-root > .blip > .blip-main .blip-content");

test("typing at once after New wave, the reply box and Enter keeps every character", async ({ page }) => {
  const problems = watchProblems(page);
  await page.goto(servers.baseUrl);
  await signIn(page, await createUser(servers, "keys"));
  // No waiting for the editor: keys go out right after the click.
  await page.locator("[data-action=new-wave]").click();
  await page.keyboard.type("Planning the launch");
  await expect(editing(page)).toHaveText("Planning the launch");
  await page.keyboard.press("Shift+Enter");
  await expect(blips(page).nth(0)).toHaveText("Planning the launch");

  await page.locator(".reply-box").click();
  await page.keyboard.type("A reply");
  await page.keyboard.press("Shift+Enter");
  await expect(blips(page).nth(1)).toHaveText("A reply");

  await page.locator(".thread-root > .blip").first().locator(".blip-main").first().focus();
  await page.keyboard.press("Enter");
  await page.keyboard.type("Another reply");
  await page.keyboard.press("Shift+Enter");
  await expect(page.locator(".blip-replies .blip-content").first()).toHaveText("Another reply");
  expect(problems).toEqual([]);
});

test("Tab leaves a message being edited, and indents only in lists", async ({ page }) => {
  await page.goto(servers.baseUrl);
  await signIn(page, await createUser(servers, "tab"));
  await page.locator("[data-action=new-wave]").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Plain text");
  await page.keyboard.press("Tab");
  await expect(editing(page)).not.toBeFocused();
  await expect(editing(page)).toHaveCount(1);
  // Back in, a list item indents on Tab and keeps focus.
  await editing(page).click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("item");
  await page.locator('.edit-toolbar [data-command="bullet"]').click();
  await page.keyboard.press("Tab");
  await expect(editing(page)).toBeFocused();
  await expect(editing(page).locator(".doc-li")).toHaveCount(1);
  // Shift+Tab walks back out once the item is not indented.
  await page.keyboard.press("Shift+Tab");
  await expect(editing(page)).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(editing(page)).not.toBeFocused();
});
