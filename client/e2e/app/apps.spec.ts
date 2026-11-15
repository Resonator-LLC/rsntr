// Wave apps: a bundle inserted with Insert app runs on the node and draws
// in a closed shadow root in the blip; clicks reach it and it redraws.
// Closed roots are out of Playwright's reach, so an init script keeps them
// on `window` for the test.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { type BrowserContext, type Page, expect, test } from "@playwright/test";

import { shoot, watchProblems } from "./support/app";
import { TestServers } from "./support/servers";

test.describe.configure({ mode: "serial" });

const COUNTER = readFileSync(
  resolve(fileURLToPath(new URL(".", import.meta.url)), "../../../crates/resonator-apps/tests/fixtures/counter_rs.wasm"),
);

let servers: TestServers;
const opened: BrowserContext[] = [];

test.beforeAll(async () => {
  servers = await TestServers.start({ device: true, apps: true });
});
test.afterAll(async () => {
  for (const c of opened) {
    await c.close().catch(() => {});
  }
  await servers?.stop();
});

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(data: Uint8Array): number {
  let c = 0xffffffff;
  for (const byte of data) {
    c = CRC_TABLE[(c ^ byte) & 0xff]! ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

/** A zip of stored (uncompressed) files. */
function zip(files: readonly (readonly [string, Uint8Array])[]): Buffer {
  const local: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const [name, data] of files) {
    const nameBytes = Buffer.from(name);
    const crc = crc32(data);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt32LE(crc, 14);
    header.writeUInt32LE(data.length, 18);
    header.writeUInt32LE(data.length, 22);
    header.writeUInt16LE(nameBytes.length, 26);
    local.push(header, nameBytes, Buffer.from(data));
    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0);
    entry.writeUInt16LE(20, 4);
    entry.writeUInt16LE(20, 6);
    entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(data.length, 20);
    entry.writeUInt32LE(data.length, 24);
    entry.writeUInt16LE(nameBytes.length, 28);
    entry.writeUInt32LE(offset, 42);
    central.push(entry, nameBytes);
    offset += header.length + nameBytes.length + data.length;
  }
  const centralBytes = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, centralBytes, end]);
}

const BUNDLE = zip([
  ["manifest.json", Buffer.from(JSON.stringify({ name: "counter", version: "1", permissions: ["wave", "kv"] }))],
  ["app.wasm", COUNTER],
]);

/** The text of the app's closed shadow roots. */
const appText = (page: Page) =>
  page.evaluate(() =>
    ((window as unknown as { appRoots: ShadowRoot[] }).appRoots ?? []).map((r) => r.textContent ?? "").join("|"),
  );

test("Insert app runs the counter in the blip, and clicks reach it", async ({ browser }) => {
  const context = await browser.newContext();
  opened.push(context);
  await context.addInitScript(() => {
    const roots: ShadowRoot[] = [];
    (window as unknown as { appRoots: ShadowRoot[] }).appRoots = roots;
    const attach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function (init: ShadowRootInit) {
      const root = attach.call(this, init);
      roots.push(root);
      return root;
    };
  });
  const page = await context.newPage();
  const problems = watchProblems(page);
  await page.goto(servers.deviceUrl!);
  await page.locator(".topbar").waitFor();

  await page.locator("[data-action=new-wave]").click();
  const editing = page.locator(".blip-editing .doc[contenteditable=true]");
  await expect(editing).toBeFocused();
  await page.keyboard.type("A counter");
  await page.keyboard.press("Enter");
  const [chooser] = await Promise.all([
    page.waitForEvent("filechooser"),
    page.locator('.edit-toolbar [data-command="app"]').click(),
  ]);
  await chooser.setFiles({ name: "counter.zip", mimeType: "application/zip", buffer: BUNDLE });

  const box = page.locator(".doc-app .app-box");
  await expect(box.locator(".app-name")).toHaveText("counter", { timeout: 60_000 });
  await expect(box.locator(".app-status")).toHaveText("Running", { timeout: 60_000 });
  await expect.poll(() => appText(page), { timeout: 30_000 }).toContain("0+1");

  // Inside the app is not the editor: the click goes to the app.
  for (let i = 0; i < 2; i++) {
    await page.evaluate(() => {
      const root = (window as unknown as { appRoots: ShadowRoot[] }).appRoots[0]!;
      (root.querySelector("button") as HTMLButtonElement).click();
    });
  }
  await expect.poll(() => appText(page), { timeout: 15_000 }).toContain("2+1");
  // The stylesheet came from our origin; nothing in the app's root is a
  // password field or a link.
  const shape = await page.evaluate(() => {
    const root = (window as unknown as { appRoots: ShadowRoot[] }).appRoots[0]!;
    return {
      sheet: root.querySelector("link[rel=stylesheet]")?.getAttribute("href") ?? "",
      sheets: root.styleSheets.length,
      passwords: root.querySelectorAll("input[type=password]").length,
      links: root.querySelectorAll("a, script, iframe").length,
    };
  });
  expect(shape.sheet).toMatch(/^\/assets\/app-ui-.*\.css$/);
  expect(shape.sheets).toBe(1);
  expect(shape.passwords).toBe(0);
  expect(shape.links).toBe(0);

  await page.locator("[data-command=done]").click();
  await expect(page.locator(".topbar-saved")).toHaveText("Saved", { timeout: 15_000 });
  await shoot(page, "apps-01-counter");

  // Each click posted ("inc", count) to the app wavelet: the Log shows them,
  // and the wave shows nothing else (no blip, no unread).
  await page.locator(".doc-app .app-log-button").click();
  const items = page.locator(".app-log-item");
  await expect(items).toHaveCount(2, { timeout: 15_000 });
  await expect(items.nth(0).locator(".app-log-kind")).toHaveText("inc");
  await expect(items.nth(1).locator(".app-log-body")).toHaveText("2");
  await expect(items.nth(0).locator(".app-log-author")).toHaveText(/^me@/);
  await shoot(page, "apps-02-log");
  await page.locator(".dialog-actions .btn-primary").click();
  await expect(page.locator(".blip")).toHaveCount(1);

  // After a reload the app starts again from its kv, already allowed.
  await page.reload();
  await expect(page.locator(".doc-app .app-status")).toHaveText("Running", { timeout: 60_000 });
  await expect.poll(() => appText(page), { timeout: 30_000 }).toContain("2+1");
  expect(await page.evaluate(() => (window as unknown as { cspViolations?: string[] }).cspViolations ?? [])).toEqual([]);
  expect(problems).toEqual([]);
  await context.close();
});
