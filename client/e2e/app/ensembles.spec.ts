// Ensembles: a saved query plus a label of its own, shown as pills over the
// wave list. Waves get the label by drag and drop, or from their menu.

import { type Page, expect, test } from "@playwright/test";

import { createUser, shoot, signedInPage } from "./support/app";
import { TestServers } from "./support/servers";

let servers: TestServers;
test.beforeAll(async () => {
  servers = await TestServers.start();
});
test.afterAll(async () => {
  await servers?.stop();
});

async function newWave(page: Page, title: string, body: string): Promise<string> {
  await page.locator("[data-action=new-wave]").click();
  const editing = page.locator(".blip-editing .doc[contenteditable=true]");
  await expect(editing).toBeFocused();
  await page.keyboard.type(title);
  await page.keyboard.press("Enter");
  await page.keyboard.type(body);
  await page.locator(".edit-done").click();
  await expect(editing).toHaveCount(0);
  return decodeURIComponent(new URL(page.url()).hash.slice(1));
}

test("an ensemble lists its query's waves and the waves dragged in, and stays", async ({ browser }) => {
  const user = await createUser(servers, "ens");
  const { context, page, problems } = await signedInPage(browser, servers, user);
  const relay = await newWave(page, "Relay load", "The relay is busy.");
  const lease = await newWave(page, "Office lease", "Sign by Friday.");
  const notes = await newWave(page, "Notes", "Nothing to see.");
  const digest = (waveId: string) => page.locator(`.digest[data-wave-id="${waveId}"]`);
  await expect(digest(notes)).toBeVisible({ timeout: 15_000 });

  // Make one with a query; the editor counts its waves.
  await page.locator(".ens-new").click();
  await page.locator(".ens-editor").getByLabel("Name").fill("Relay");
  await page.locator(".ens-editor").getByLabel("Query").fill("relay");
  await expect(page.locator(".ens-check")).toHaveText("1 wave in inbox", { timeout: 15_000 });
  await shoot(page, "ensembles-editor");
  await page.locator(".ens-editor").getByRole("button", { name: "Create" }).click();
  const pill = page.locator(".ens-pill", { hasText: "Relay" });
  await expect(pill).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".digest")).toHaveCount(1, { timeout: 15_000 });
  await expect(digest(relay)).toBeVisible();

  // Back to the whole inbox; drag the lease wave onto the pill.
  await pill.click();
  await expect(digest(lease)).toBeVisible({ timeout: 15_000 });
  await digest(lease).locator(".digest-link").dragTo(pill);
  await expect(page.locator(".toast")).toContainText("Added to Relay");
  await expect(digest(lease).locator(".ens-chip")).toHaveText(/Relay/, { timeout: 15_000 });
  await pill.click();
  await expect(page.locator(".digest")).toHaveCount(2, { timeout: 15_000 });
  await shoot(page, "ensembles-list");

  // The ensemble is kept in the settings wave: it is there after a reload,
  // and that wave is not in the list.
  await page.reload();
  await expect(page.locator(".ens-pill", { hasText: "Relay" })).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(".digest")).toHaveCount(3, { timeout: 15_000 });

  // From the menu (as on a touch screen): take the lease wave out again.
  await digest(lease).locator(".digest-more").click();
  await page.locator('[role=menu] [data-action="ensemble"]').click();
  await expect(page.locator(".toast")).toContainText("Taken out of Relay");
  await expect(digest(lease).locator(".ens-chip")).toHaveCount(0, { timeout: 15_000 });

  // An SQL query, with its error shown while it is wrong.
  await page.locator(".ens-new").click();
  await page.locator(".ens-editor").getByLabel("Name").fill("SQL");
  await page.locator(".ens-editor").getByLabel("Query").fill("SELECT nope FROM waves");
  await expect(page.locator(".ens-check")).toContainText("no such column", { timeout: 15_000 });
  await page
    .locator(".ens-editor")
    .getByLabel("Query")
    .fill("SELECT wave_id FROM texts WHERE texts MATCH 'friday'");
  await expect(page.locator(".ens-check")).toHaveText("1 wave in inbox", { timeout: 15_000 });
  await page.locator(".ens-editor").getByRole("button", { name: "Create" }).click();
  await expect(page.locator(".digest")).toHaveCount(1, { timeout: 15_000 });
  await expect(digest(lease)).toBeVisible();

  // Delete, with Undo.
  await page.locator(".ens-edit").click();
  await page.locator(".ens-editor").getByRole("button", { name: "Delete" }).click();
  await expect(page.locator(".ens-pill", { hasText: "SQL" })).toHaveCount(0);
  await page.locator('.toast [data-action="undo"]').click();
  await expect(page.locator(".ens-pill", { hasText: "SQL" })).toBeVisible();

  expect(problems).toEqual([]);
  await context.close();
});
