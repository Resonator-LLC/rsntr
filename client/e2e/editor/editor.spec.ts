// The editor in a real Chromium: two views over a simulated OT server.

import { type Page, expect, test } from "@playwright/test";

import { startVite } from "./support";

let vite: { url: string; close: () => Promise<void> };

test.beforeAll(async () => {
  vite = await startVite();
});
test.afterAll(async () => {
  await vite?.close();
});

const A = "#editor-a .doc";
const B = "#editor-b .doc";

async function open(page: Page, xml?: string): Promise<void> {
  const q = xml === undefined ? "" : `?xml=${encodeURIComponent(xml)}`;
  page.on("pageerror", (e) => {
    throw e;
  });
  await page.goto(`${vite.url}/e2e/editor/harness/index.html${q}`);
  await page.waitForSelector("body[data-ready=true]");
}

async function settle(page: Page): Promise<void> {
  await page.waitForFunction(() => (window as any).harness.idle());
}

const xml = (page: Page, i: number) => page.evaluate((i) => (window as any).harness.xml(i), i);
const serverXml = (page: Page) => page.evaluate(() => (window as any).harness.serverXml());
const sel = (page: Page, i: number) => page.evaluate((i) => (window as any).harness.selection(i), i);

/** The DOM text of an editor, lines joined by newlines. */
const domText = (page: Page, selector: string) =>
  page.$eval(selector, (el) => Array.from(el.children).map((l) => (l as HTMLElement).textContent).join("\n"));

test("typing, Enter and Backspace edit the document", async ({ page }) => {
  await open(page);
  await page.click(A);
  await page.keyboard.type("Hello world");
  await page.keyboard.press("Enter");
  await page.keyboard.type("second");
  await page.keyboard.press("Backspace");
  await settle(page);
  expect(await xml(page, 0)).toBe("<body><line/>Hello world<line/>secon</body>");
  expect(await domText(page, B)).toBe("Hello world\nsecon");
  expect(await serverXml(page)).toBe(await xml(page, 1));
});

test("bold via shortcut and toolbar; headings and lists", async ({ page }) => {
  await open(page);
  await page.click(A);
  await page.keyboard.press("Control+b");
  await page.keyboard.type("bold");
  await page.keyboard.press("Control+b");
  await page.keyboard.type(" plain");
  await page.keyboard.press("Enter");
  await page.keyboard.type("item");
  await page.click('#toolbar-a [data-command="bullet"]');
  await page.keyboard.press("Enter");
  await page.keyboard.type("two");
  await page.keyboard.press("Tab");
  await settle(page);
  const x = await xml(page, 0);
  expect(x).toContain('<?a "style/fontWeight"="bold"?>bold<?a "style/fontWeight"?> plain');
  expect(x).toContain('<line t="li"/>item<line i="1" t="li"/>two');
  await expect(page.locator(`${B} .doc-run`).first()).toHaveCSS("font-weight", "700");
  await expect(page.locator(`${B} .doc-li`)).toHaveCount(2);
  // Enter on an empty item ends the list
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await page.keyboard.press("Enter");
  await settle(page);
  expect(await xml(page, 0)).toMatch(/two<line i="1" t="li"\/><line\/>|two<line\/>/);
});

test("concurrent typing in the same line converges", async ({ page }) => {
  await open(page, "<body><line/>0123456789</body>");
  await page.evaluate(() => (window as any).harness.setDelay(30));
  await page.click(A);
  await page.evaluate(() => (window as any).harness.focus(0, 3));
  await page.keyboard.type("aaaa", { delay: 15 });
  await page.evaluate(() => (window as any).harness.focus(1, 9));
  await page.keyboard.type("bbbb", { delay: 15 });
  await settle(page);
  const x0 = await xml(page, 0);
  expect(x0).toBe(await xml(page, 1));
  expect(x0).toBe(await serverXml(page));
  expect(x0).toMatch(/aaaa/);
  expect(x0).toMatch(/bbbb/);
  expect(x0.replace(/aaaa|bbbb/g, "")).toBe("<body><line/>0123456789</body>");
  expect(await domText(page, A)).toBe(await domText(page, B));
});

test("simultaneous typing in both views interleaves and converges", async ({ page }) => {
  await open(page, "<body><line/>xy</body>");
  await page.evaluate(() => (window as any).harness.setDelay(20));
  // B's caret at the end, A types; remote ops arrive in B while it types too.
  const typeIn = async (i: number, text: string) => {
    for (const ch of text) {
      await page.evaluate(
        ([i, ch]) => {
          const h = (window as any).harness;
          h.clients[i].view.element.focus();
          h.exec(i, { type: "insertText", text: ch });
        },
        [i, ch] as const,
      );
    }
  };
  await page.evaluate(() => {
    const h = (window as any).harness;
    h.setSelection(0, 3, 3);
    h.setSelection(1, 4, 4);
  });
  await Promise.all([typeIn(0, "AAAAA"), typeIn(1, "BBBBB")]);
  await settle(page);
  const x0 = await xml(page, 0);
  expect(x0).toBe(await xml(page, 1));
  expect(x0).toBe("<body><line/>AAAAAxBBBBBy</body>");
});

test("a remote insert before the caret keeps the caret on its text", async ({ page }) => {
  await open(page, "<body><line/>hello</body>");
  await page.click(A);
  await page.keyboard.press("End");
  await page.evaluate(() => (window as any).harness.insertAtServer(3, "XYZ"));
  await settle(page);
  await page.keyboard.type("!");
  await settle(page);
  expect(await xml(page, 0)).toBe("<body><line/>XYZhello!</body>");
  expect(await sel(page, 0)).toEqual({ start: 12, end: 12 });
});

test("IME composition (simulated through CDP) survives remote edits", async ({ page }) => {
  await open(page, "<body><line/>abc</body><!-- -->".replace("<!-- -->", ""));
  await page.click(A);
  await page.keyboard.press("End");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.imeSetComposition", { text: "に", selectionStart: 1, selectionEnd: 1 });
  await cdp.send("Input.imeSetComposition", { text: "にほ", selectionStart: 2, selectionEnd: 2 });
  // A remote edit in the composed line, before the composition.
  await page.evaluate(() => (window as any).harness.insertAtServer(3, "Z"));
  await settle(page);
  await cdp.send("Input.imeSetComposition", { text: "にほん", selectionStart: 3, selectionEnd: 3 });
  await cdp.send("Input.insertText", { text: "日本" });
  await settle(page);
  expect(await xml(page, 0)).toBe("<body><line/>Zabc日本</body>");
  expect(await xml(page, 1)).toBe("<body><line/>Zabc日本</body>");
  expect(await domText(page, A)).toBe("Zabc日本");
  await page.keyboard.type("!");
  await settle(page);
  expect(await xml(page, 0)).toBe("<body><line/>Zabc日本!</body>");
});

test("emoji, combining marks and RTL text", async ({ page }) => {
  await open(page);
  await page.click(A);
  await page.keyboard.insertText("a😀é");
  await page.keyboard.press("Backspace"); // removes e + combining mark
  await page.keyboard.press("Backspace"); // removes the emoji (a surrogate pair)
  await page.keyboard.press("Enter");
  await page.keyboard.insertText("שלום");
  await settle(page);
  expect(await xml(page, 0)).toBe("<body><line/>a<line/>שלום</body>");
  await expect(page.locator(`${A} .doc-line`).nth(1)).toHaveAttribute("dir", "auto");
});

test("paste: rich HTML maps to annotations, scripts never run", async ({ page }) => {
  await open(page);
  await page.click(A);
  await page.evaluate(() => {
    const dt = new DataTransfer();
    dt.setData("text/html", '<p><b>Bold</b> and <a href="https://example.com">link</a><img src=x onerror="window.pwned=1"></p><ul><li>one</li></ul>');
    dt.setData("text/plain", "Bold and link\none");
    document.querySelector("#editor-a .doc")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  await settle(page);
  const x = await xml(page, 0);
  expect(x).toContain('"style/fontWeight"="bold"');
  expect(x).toContain('"link/manual"="https://example.com"');
  expect(x).toMatch(/<line t="li"\/>(<\?a "link\/manual"\?>)?one<\/body>/);
  expect(await page.evaluate(() => (window as any).pwned)).toBeUndefined();
  await expect(page.locator(`${B} a.doc-link`)).toHaveAttribute("rel", "noopener noreferrer");
});

test("auto-links and undo/redo", async ({ page }) => {
  await open(page);
  await page.click(A);
  await page.keyboard.type("see https://wave.example/x ok");
  await settle(page);
  expect(await xml(page, 0)).toContain('"link/auto"="https://wave.example/x"');
  await page.keyboard.press("Control+z");
  await settle(page);
  const after = await xml(page, 0);
  expect(after.length).toBeLessThan((await xml(page, 1)).length + 100);
  await page.keyboard.press("Control+Shift+z");
  await settle(page);
  expect(await xml(page, 0)).toContain("ok");
  expect(await xml(page, 0)).toBe(await xml(page, 1));
});
