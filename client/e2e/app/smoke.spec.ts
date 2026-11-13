import { expect, test } from "@playwright/test";

import { createUser, shoot, signIn, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";

let servers: TestServers;
test.beforeAll(async () => {
  servers = await TestServers.start();
});
test.afterAll(async () => {
  await servers?.stop();
});

test("smoke: sign in, new wave, type, reply, inline reply", async ({ page }) => {
  const problems = watchProblems(page);
  const user = await createUser(servers, "smoke");
  await page.goto(servers.baseUrl);
  await page.getByLabel("Username").waitFor();
  await shoot(page, "00-signin");
  await signIn(page, user);
  await page.waitForTimeout(500);
  await shoot(page, "01-inbox-empty");
  await page.locator("[data-action=new-wave]").click();
  await page.locator(".blip-editing .doc[contenteditable=true]").waitFor();
  await page.keyboard.type("Planning the launch");
  await page.keyboard.press("Enter");
  await page.keyboard.type("We need to decide on the date and the venue.");
  await page.keyboard.press("Shift+Enter");
  await page.waitForTimeout(300);
  // reply
  await page.locator(".reply-box").click();
  await page.locator(".blip-editing .doc[contenteditable=true]").waitFor();
  await page.keyboard.type("How about next Friday?");
  await page.keyboard.press("Shift+Enter");
  // reply to the first blip (nested)
  const first = page.locator(".thread-root > .blip").first().locator(".blip-main").first();
  await first.click();
  await page.keyboard.press("Enter");
  await page.locator(".blip-editing .doc[contenteditable=true]").waitFor();
  await page.keyboard.type("A nested reply.");
  await page.keyboard.press("Shift+Enter");
  await page.waitForTimeout(800);
  await shoot(page, "02-wave");
  console.log(problems.join("\n"));
  expect(problems).toEqual([]);
});
