// The application in Chromium against the real server, served from the
// production build under the server's CSP.

import { type BrowserContext, type Page, expect, test } from "@playwright/test";

import { WaveClient } from "../../src/client";
import { withCookieJar } from "../../src/net";
import { type User, createUser, shoot, signIn, signedInPage, uniqueName, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";

test.describe.configure({ mode: "serial" });

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

async function session(browser: import("@playwright/test").Browser, user: User, options = {}) {
  const s = await signedInPage(browser, servers, user, options);
  opened.push(s.context);
  await s.context.addInitScript(() => {
    (window as unknown as { cspViolations: string[] }).cspViolations = [];
    document.addEventListener("securitypolicyviolation", (e) =>
      (window as unknown as { cspViolations: string[] }).cspViolations.push(`${e.violatedDirective} ${e.blockedURI}`),
    );
  });
  await s.page.reload();
  await s.page.locator(".topbar").waitFor();
  return s;
}

const editing = (page: Page) => page.locator(".blip-editing .doc[contenteditable=true]");

async function addParticipant(page: Page, address: string): Promise<void> {
  await page.locator(".wave-add-participant").click();
  await page.locator("#wave-add-participant-input").fill(address);
  await page.locator("#wave-add-participant-input").press("Enter");
  await expect(page.locator(`.wave-participant[data-participant="${address}"]`)).toBeVisible();
}

const blipText = (page: Page, index: number) =>
  page.locator(".thread-root > .blip > .blip-main .blip-content").nth(index);

async function saved(page: Page): Promise<void> {
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
}

test("registration through the UI, sign out, sign in", async ({ browser }) => {
  const context = await browser.newContext();
  opened.push(context);
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.baseUrl);
  await page.getByRole("button", { name: "Register a new account" }).click();
  const name = uniqueName("carol");
  await page.getByLabel("Username").fill(name);
  await page.getByLabel("Password", { exact: true }).fill("a good password");
  await page.getByLabel("Re-enter password").fill("a good password");
  await shoot(page, "03-register");
  await page.locator(".auth-form button[type=submit]").click();
  await expect(page.locator(".search-empty")).toBeVisible();
  // The address is in Settings, behind the connection button.
  await page.locator(".topbar-settings").click();
  await expect(page.locator("[data-testid=my-address]")).toHaveText(new RegExp(`^${name}@`));
  await page.locator(".settings-back").click();
  await page.locator(".topbar-signout").click();
  await expect(page.locator(".auth-form")).toBeVisible();
  await signIn(page, { name, address: `${name}@example.com`, password: "a good password" });
  await expect(page.locator(".topbar-net")).toHaveText(/Online/);
  // The pending long poll of the search panel is refused when the session ends.
  expect(problems.filter((p) => p !== "http 403 /search/changes")).toEqual([]);
});

let sharedWave = "";

test("create a wave with formatting; add a participant; it appears for them, unread", async ({ browser }) => {
  const a = await session(browser, alice);
  const b = await session(browser, bob);
  const page = a.page;
  await page.locator("[data-action=new-wave]").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Launch plan");
  await page.keyboard.press("Enter");
  await page.keyboard.type("This is ");
  await page.locator('.edit-toolbar [data-command="bold"]').click();
  await page.keyboard.type("important");
  await page.locator('.edit-toolbar [data-command="bold"]').click();
  await page.keyboard.type(" text.");
  await page.keyboard.press("Enter");
  await page.locator('.edit-toolbar [data-command="bullet"]').click();
  await page.keyboard.type("first point");
  await page.keyboard.press("Enter");
  await page.keyboard.type("second point");
  await shoot(page, "04-editing-toolbar");
  await page.locator(".edit-done").click();
  await expect(editing(page)).toHaveCount(0);
  sharedWave = decodeURIComponent(new URL(page.url()).hash.slice(1));
  expect(sharedWave).toMatch(/^example\.com\/w\+/);
  await expect(blipText(page, 0).locator(".doc-run").filter({ hasText: "important" })).toHaveCSS("font-weight", "700");
  await expect(blipText(page, 0).locator(".doc-li")).toHaveCount(2);
  await expect(page.locator(".wave-title")).toHaveText("Launch plan");

  await addParticipant(page, bob.address);
  await saved(page);
  // Bob's inbox shows it, unread (live refresh).
  const digest = b.page.locator(`.digest[data-wave-id="${sharedWave}"]`);
  await expect(digest).toBeVisible({ timeout: 20_000 });
  await expect(digest.locator(".badge-unread")).toHaveText(/1/);
  await expect(digest.locator(".digest-title")).toHaveText("Launch plan");
  await shoot(b.page, "05-inbox-unread");
  // Bob opens it and focuses the blip (read on focus, as the original:
  // opening alone focuses nothing); the badge goes.
  await digest.locator(".digest-link").click();
  await expect(b.page.locator(".wave-title")).toHaveText("Launch plan");
  await expect(b.page.locator(".blip-unread")).toHaveCount(1);
  await blipText(b.page, 0).click();
  await expect(b.page.locator(".blip-unread")).toHaveCount(0);
  await expect(digest.locator(".badge-unread")).toHaveCount(0, { timeout: 20_000 });
  expect(a.problems).toEqual([]);
  expect(b.problems).toEqual([]);
  await a.context.close();
  await b.context.close();
});

test("two browsers edit the same blip concurrently and converge", async ({ browser }) => {
  const a = await session(browser, alice);
  const b = await session(browser, bob);
  for (const s of [a, b]) {
    await s.page.goto(`${servers.baseUrl}/#${sharedWave}`);
    await expect(s.page.locator(".wave-title")).toHaveText("Launch plan");
  }
  const start = async (page: Page, at: "start" | "end") => {
    await blipText(page, 0).click();
    await page.keyboard.press("Control+e");
    await expect(editing(page)).toBeFocused();
    await page.keyboard.press(at === "start" ? "Control+Home" : "Control+End");
  };
  await start(a.page, "end");
  await start(b.page, "start");
  await Promise.all([
    a.page.keyboard.type(" Alice was here.", { delay: 25 }),
    b.page.keyboard.type("Bob says hi. ", { delay: 25 }),
  ]);
  await saved(a.page);
  await saved(b.page);
  const text = (p: Page) => blipText(p, 0).innerText();
  await expect.poll(async () => (await text(a.page)) === (await text(b.page)), { timeout: 15_000 }).toBe(true);
  const final = await text(a.page);
  expect(final).toContain("Alice was here.");
  expect(final.startsWith("Bob says hi. Launch plan")).toBe(true);
  // What the server stores.
  const fetched = await a.page.request.get(`${servers.baseUrl}/fetch/${sharedWave.replace("/", "/")}`);
  const body = await fetched.text();
  expect(body).toContain("Alice was here.");
  expect(body).toContain("Bob says hi.");
  for (const s of [a, b]) {
    await s.page.keyboard.press("Escape");
  }
  expect(a.problems).toEqual([]);
  expect(b.problems).toEqual([]);
  await a.context.close();
  await b.context.close();
});

// After the concurrent edit the implicit title is the first sentence.
const TITLE = /Bob says hi\./;

test("reply, nested reply, inline reply, delete; reload restores; screenshots", async ({ browser }) => {
  const a = await session(browser, alice);
  const page = a.page;
  await page.goto(`${servers.baseUrl}/#${sharedWave}`);
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  // Reply at the end of the conversation.
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Next Friday works for me.");
  await page.keyboard.press("Shift+Enter");
  // Nested reply to the root blip via its menu.
  await page.locator(".thread-root > .blip").first().locator(".blip-menu-btn").first().click();
  await page.locator('.menu [data-action="reply"]').click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Which venue, though?");
  await page.keyboard.press("Shift+Enter");
  // Inline reply in the middle of the root blip.
  await blipText(page, 0).click();
  await page.keyboard.press("Control+e");
  await expect(editing(page)).toBeFocused();
  await page.keyboard.press("Control+Home");
  await page.keyboard.press("End");
  await page.locator('.edit-toolbar [data-command="inlineReply"]').click();
  await expect(page.locator(".thread-inline .blip-editing .doc")).toBeFocused();
  await page.keyboard.type("Inline: what about Saturday?");
  await page.keyboard.press("Shift+Enter");
  await expect(page.locator(".thread-inline .blip-content")).toHaveText("Inline: what about Saturday?");
  // A blip to delete.
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Delete me");
  await page.keyboard.press("Shift+Enter");
  const doomed = page.locator(".thread-root > .blip").last();
  await expect(doomed).toContainText("Delete me");
  await doomed.locator(".blip-menu-btn").click();
  await page.locator('.menu [data-action="delete"]').click();
  await page.locator("dialog[open] .btn-danger").click();
  await expect(page.locator(".thread-root")).not.toContainText("Delete me");
  await saved(page);
  await page.locator(".thread-root > .blip").first().locator(".blip-main").first().click({ position: { x: 5, y: 5 } });
  await shoot(page, "06-wave-threads");
  // Reload: the same state comes back.
  await page.reload();
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  await expect(page.locator(".thread-inline .blip-content")).toHaveText("Inline: what about Saturday?");
  await expect(page.locator(".blip-replies .blip-content").first()).toContainText("Which venue");
  await expect(page.locator(".thread-root")).toContainText("Next Friday works for me.");
  expect(await page.evaluate(() => (window as unknown as { cspViolations: string[] }).cspViolations)).toEqual([]);
  expect(a.problems).toEqual([]);
  // Dark theme of the same state.
  const dark = await session(browser, alice, { colorScheme: "dark" });
  await dark.page.goto(`${servers.baseUrl}/#${sharedWave}`);
  await expect(dark.page.locator(".thread-inline .blip-content")).toBeVisible();
  await dark.page.waitForTimeout(300);
  await shoot(dark.page, "07-dark");
  await a.context.close();
  await dark.context.close();
});

test("offline: edits are kept and sent when the connection is back", async ({ browser }) => {
  const a = await session(browser, alice);
  const b = await session(browser, bob);
  for (const s of [a, b]) {
    await s.page.goto(`${servers.baseUrl}/#${sharedWave}`);
    await expect(s.page.locator(".wave-title")).toHaveText(TITLE);
  }
  await a.context.setOffline(true);
  servers.dropConnections();
  await expect(a.page.locator(".topbar-net")).not.toHaveText(/Online/, { timeout: 15_000 });
  await a.page.locator(".reply-box").click();
  await expect(editing(a.page)).toBeFocused();
  await a.page.keyboard.type("Written while offline.");
  await a.page.keyboard.press("Shift+Enter");
  await expect(a.page.locator(".topbar-saved")).toHaveText(/Unsaved/);
  await shoot(a.page, "08-offline");
  // B is still connected (its socket was cut too; it reconnects) and does not see it yet.
  await b.page.waitForTimeout(500);
  await expect(b.page.locator(".thread-root")).not.toContainText("Written while offline.");
  await a.context.setOffline(false);
  for (let i = 0; i < 20; i++) {
    const t = await a.page.locator(".topbar-net").textContent();
    console.log(`after online ${i}s: ${t}`);
    if (t?.includes("Online")) break;
    if (i === 3) {
      await a.page.locator(".topbar-settings").click();
      await a.page.locator("[data-action=connect-now]").click({ timeout: 2000 }).catch((e) => console.log("click failed", e.message));
      await a.page.locator(".settings-back").click();
    }
    await a.page.waitForTimeout(1000);
  }
  await expect(a.page.locator(".topbar-net")).toHaveText(/Online/, { timeout: 30_000 });
  await saved(a.page);
  await expect(b.page.locator(".thread-root")).toContainText("Written while offline.", { timeout: 15_000 });
  await a.context.close();
  await b.context.close();
});

test("search, archive and back to the inbox", async ({ browser }) => {
  const b = await session(browser, bob);
  const page = b.page;
  const digest = page.locator(`.digest[data-wave-id="${sharedWave}"]`);
  await expect(digest).toBeVisible();
  // Free text search.
  await page.locator(".search-input").fill("venue");
  await page.locator(".search-input").press("Enter");
  await expect(page.locator(".digest")).toHaveCount(1, { timeout: 15_000 });
  await page.locator(".search-input").fill("nonexistentword");
  await page.locator(".search-input").press("Enter");
  await expect(page.locator(".search-empty")).toBeVisible({ timeout: 15_000 });
  // Archive from the inbox.
  await page.getByRole("button", { name: "Inbox", exact: true }).click();
  await expect(digest).toBeVisible();
  // Archive from the row's menu; Undo brings it back; archive again.
  const fromMenu = async (action: string) => {
    await digest.locator(".digest-more").click();
    await page.locator(`[role=menu] [data-action="${action}"]`).click();
  };
  await fromMenu("archive");
  await expect(digest).toHaveCount(0, { timeout: 15_000 });
  await expect(page.locator(".toast")).toContainText("Archived");
  await page.locator('.toast [data-action="undo"]').click();
  await expect(digest).toBeVisible({ timeout: 15_000 });
  await fromMenu("archive");
  await expect(digest).toHaveCount(0, { timeout: 15_000 });
  await page.getByRole("button", { name: "Archive", exact: true }).click();
  await expect(digest).toBeVisible({ timeout: 15_000 });
  await fromMenu("inbox");
  await expect(digest).toHaveCount(0, { timeout: 15_000 });
  await page.getByRole("button", { name: "Inbox", exact: true }).click();
  await expect(digest).toBeVisible({ timeout: 15_000 });
  // Mute and unmute from the wave's own toolbar.
  await digest.locator(".digest-link").click();
  await page.locator('.wave-tool[data-action="mute"]').click();
  await expect(digest).toHaveCount(0, { timeout: 15_000 });
  await page.getByRole("button", { name: "Muted", exact: true }).click();
  await expect(digest).toBeVisible({ timeout: 15_000 });
  await page.locator('.wave-tool[data-action="unmute"]').click();
  await page.getByRole("button", { name: "Inbox", exact: true }).click();
  await expect(digest).toBeVisible({ timeout: 15_000 });
  expect(b.problems).toEqual([]);
  await b.context.close();
});

// A 1x1 PNG is too small to be interesting; this is 8x8, red.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEklEQVR4nGP4z8CAFWEXHbQSACj/P8Fu7N9hAAAAAElFTkSuQmCC",
  "base64",
);

test("attachment upload shows a thumbnail", async ({ browser }) => {
  const a = await session(browser, alice);
  const page = a.page;
  await page.goto(`${servers.baseUrl}/#${sharedWave}`);
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Here is the floor plan: ");
  // The button opens the file chooser; the upload starts with the choice.
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('.edit-toolbar [data-command="attach"]').click(),
  ]);
  await chooser.setFiles({ name: "plan.png", mimeType: "image/png", buffer: PNG });
  const thumb = page.locator(".doc-image-thumb");
  await expect(thumb).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => thumb.evaluate((img) => (img as HTMLImageElement).naturalWidth)).toBeGreaterThan(0);
  await page.keyboard.press("Shift+Enter");
  await saved(page);
  expect(await page.evaluate(() => (window as unknown as { cspViolations: string[] }).cspViolations)).toEqual([]);
  expect(a.problems).toEqual([]);
  await a.context.close();
});

test("phone layout: list, wave, back", async ({ browser }) => {
  const a = await session(browser, alice, { viewport: { width: 360, height: 740 }, hasTouch: true, isMobile: true });
  const page = a.page;
  await expect(page.locator(".app-search")).toBeVisible();
  await expect(page.locator(".app-main")).toBeHidden();
  await shoot(page, "09-phone-list");
  await page.locator(`.digest[data-wave-id="${sharedWave}"] .digest-link`).click();
  await expect(page.locator(".app-search")).toBeHidden();
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  await expect(page.locator(".thread-inline .blip-content")).toHaveText(/Saturday/);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await shoot(page, "10-phone-wave");
  await page.locator(".wave-back").click();
  await expect(page.locator(".app-search")).toBeVisible();
  // Browser back/forward follow the fragment.
  await page.goBack();
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  await page.goBack();
  await expect(page.locator(".app-search")).toBeVisible();
  expect(a.problems).toEqual([]);
  await a.context.close();
});

test("a 500-blip wave opens and stays responsive", async ({ browser }) => {
  test.setTimeout(180_000);
  const client = new WaveClient({ baseUrl: servers.backendUrl, fetch: withCookieJar(fetch) });
  await client.signIn(alice.address, alice.password);
  await client.whenConnected();
  const { wave, rootBlipId } = await client.createWave({ text: "Big wave\nwith many blips" });
  let parent = rootBlipId;
  for (let i = 1; i < 500; i++) {
    const reply = wave.reply(i % 10 === 0 ? rootBlipId : parent, { text: `Blip number ${i}: some text to read.` });
    if (i % 25 === 0) {
      parent = reply.blipId;
    }
  }
  await wave.whenSaved();
  const waveId = wave.waveId.serialise();
  client.dispose();

  const a = await session(browser, alice);
  const page = a.page;
  const started = Date.now();
  await page.goto(`${servers.baseUrl}/#${waveId}`);
  await expect(page.locator(".wave-title")).toContainText("Big wave");
  await expect(page.locator(".blip-content .doc").first()).toBeVisible();
  const firstPaint = Date.now() - started;
  await expect.poll(() => page.locator("[data-blip-id]").count(), { timeout: 60_000 }).toBe(500);
  const allBlips = Date.now() - started;
  await expect.poll(() => page.locator(".blip-content > .doc").count(), { timeout: 60_000 }).toBe(500);
  const allDocs = Date.now() - started;
  // Typing latency in a blip of this wave.
  await page.locator(".blip-content").first().click();
  await page.keyboard.press("Control+e");
  await expect(editing(page)).toBeFocused();
  const latency = await page.evaluate(async () => {
    const el = document.querySelector(".blip-editing .doc") as HTMLElement;
    const times: number[] = [];
    for (let i = 0; i < 20; i++) {
      const t = performance.now();
      el.dispatchEvent(new InputEvent("beforeinput", { inputType: "insertText", data: "x", bubbles: true, cancelable: true }));
      times.push(performance.now() - t);
    }
    times.sort((x, y) => x - y);
    return { median: times[10], max: times[19] };
  });
  const navigation = await page.evaluate(async () => {
    const t = performance.now();
    for (let i = 0; i < 50; i++) {
      document.querySelector(".wave-scroll")!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    }
    await new Promise((r) => requestAnimationFrame(() => r(null)));
    return (performance.now() - t) / 50;
  });
  console.log(
    `500-blip wave: shown ${firstPaint} ms, all 500 blips mounted ${allBlips} ms, all documents rendered ${allDocs} ms, ` +
      `typing median ${latency.median?.toFixed(2)} ms max ${latency.max?.toFixed(2)} ms, ` +
      `focus move ${navigation.toFixed(2)} ms per step`,
  );
  expect(latency.median).toBeLessThan(16);
  await page.keyboard.press("Escape");
  await a.context.close();
});

test("a reply opened and left empty is discarded", async ({ browser }) => {
  const a = await session(browser, alice);
  const page = a.page;
  await page.goto(`${servers.baseUrl}/#${sharedWave}`);
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  const blips = page.locator(".thread-root > .blip");
  const before = await blips.count();
  // Click the reply box, then click elsewhere without typing.
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await expect(blips).toHaveCount(before + 1);
  await page.locator(".wave-title").click();
  await blipText(page, 0).click();
  await expect(blips).toHaveCount(before);
  await expect(page.locator(".reply-box")).toHaveCount(1);
  // Again, ending with the keyboard.
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.press("Shift+Enter");
  await expect(blips).toHaveCount(before);
  // A reply with text is kept.
  await page.locator(".reply-box").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Kept");
  await expect(editing(page)).toContainText("Kept");
  await blipText(page, 0).click();
  await expect(blips).toHaveCount(before + 1);
  await saved(page);
  await a.context.close();
});

test("Done after typing sends one no-op (finished editing); Done without typing sends nothing", async ({
  browser,
}) => {
  const a = await session(browser, alice);
  const page = a.page;
  await page.goto(`${servers.baseUrl}/#${sharedWave}`);
  await expect(page.locator(".wave-title")).toHaveText(TITLE);
  // The root wavelet's version on the server.
  const version = async (): Promise<number> => {
    const fetched = await page.request.get(`${servers.baseUrl}/fetch/${sharedWave}`);
    // A WaveViewSnapshot: wavelet (2) → version (4) → version (1).
    const body = (await fetched.json()) as { "2": { "4": { "1": number } }[] };
    return body["2"][0]!["4"]["1"];
  };
  const settled = async (): Promise<number> => {
    await saved(page);
    let last = -1;
    await expect
      .poll(async () => {
        const now = await version();
        const same = now === last;
        last = now;
        return same;
      })
      .toBe(true);
    return last;
  };
  const edit = async () => {
    // At its top left: the middle of the root blip may hold the button
    // that continues a thread, which would open a new reply.
    await blipText(page, 0).click({ position: { x: 4, y: 4 } });
    await page.keyboard.press("Control+e");
    await expect(editing(page)).toBeFocused();
  };

  // Edit mode entered and left without typing: no delta.
  const start = await settled();
  await edit();
  await page.waitForTimeout(300);
  await page.locator(".edit-done").click();
  await expect(editing(page)).toHaveCount(0);
  await page.waitForTimeout(500);
  expect(await settled()).toBe(start);

  // Typing, then Done: one operation more than the typing.
  await edit();
  await page.keyboard.press("Control+End");
  await page.keyboard.type(" Done.");
  const typed = await settled();
  expect(typed).toBeGreaterThan(start);
  await page.locator(".edit-done").click();
  await expect(editing(page)).toHaveCount(0);
  await expect.poll(version).toBe(typed + 1);
  await page.waitForTimeout(500);
  expect(await settled()).toBe(typed + 1);
  expect(a.problems).toEqual([]);
  await a.context.close();
});

test("adding a participant: people on the server are suggested", async ({ browser }) => {
  const a = await session(browser, alice);
  const page = a.page;
  const carol = await createUser(servers, "carol");
  await page.locator("[data-action=new-wave]").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Suggestions wave");
  await page.keyboard.press("Shift+Enter");
  await page.locator(".wave-add-participant").click();
  // Everyone on the server is listed, except those already in the wave.
  const list = page.locator(".suggestion-list");
  await expect(list.locator(`[data-address="${carol.address}"]`)).toBeVisible();
  await expect(list.locator(`[data-address="${bob.address}"]`)).toBeVisible();
  await expect(list.locator(`[data-address="${alice.address}"]`)).toHaveCount(0);
  await shoot(page, "11-add-participant");
  // Typing narrows the list.
  await page.locator("#wave-add-participant-input").fill(carol.name.slice(0, 6));
  await expect(list.locator(`[data-address="${carol.address}"]`)).toBeVisible();
  await expect(list.locator(`[data-address="${bob.address}"]`)).toHaveCount(0);
  // One click adds; the dialog stays open and the person leaves the list.
  await list.locator(`[data-address="${carol.address}"]`).click();
  await expect(page.locator(`.wave-participant[data-participant="${carol.address}"]`)).toBeVisible();
  await expect(page.locator("#wave-add-participant-input")).toHaveValue("");
  await expect(list.locator(`[data-address="${carol.address}"]`)).toHaveCount(0);
  await expect(list.locator(`[data-address="${bob.address}"]`)).toBeVisible();
  await page.locator("dialog[open] button[type=submit]").click();
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  await saved(page);
  await a.context.close();
});

test("@mention by keyboard: participants first, Escape dismisses, Enter picks and adds", async ({ browser }) => {
  const a = await session(browser, alice);
  const page = a.page;
  const dave = await createUser(servers, "dave");
  await page.locator("[data-action=new-wave]").click();
  await expect(editing(page)).toBeFocused();
  await page.keyboard.type("Plans");
  await addParticipant(page, bob.address);
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  await editing(page).click();
  await page.keyboard.press("End");
  await page.keyboard.type(" @");
  const options = page.locator(".mention-option");
  // The wave's participants come first (not the viewer), then the directory.
  await expect(options.first()).toHaveAttribute("data-address", bob.address);
  await expect(page.locator(`.mention-option[data-address="${alice.address}"]`)).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(options).toHaveCount(0);
  // Escape dismissed this "@" for good: a fresh one asks again.
  await page.keyboard.press("Backspace");
  await page.keyboard.type(`@${dave.name.slice(0, 5)}`);
  await expect(options).toHaveCount(1);
  await expect(options.first()).toHaveAttribute("data-address", dave.address);
  await page.keyboard.press("Enter");
  await expect(editing(page).locator(".doc-mention")).toHaveAttribute("data-mention", dave.address);
  // Not in the wave: added.
  await expect(page.locator(`.wave-participant[data-participant="${dave.address}"]`)).toBeVisible();
  await page.keyboard.type("hi");
  await page.locator("[data-command=done]").click();
  await saved(page);
  await expect(blipText(page, 0).locator(".doc-mention")).toHaveAttribute("data-mention", dave.address);
  await expect(blipText(page, 0)).toContainText(new RegExp(`@${dave.name} hi`, "i"));
  expect(a.problems).toEqual([]);
  await a.context.close();
});
