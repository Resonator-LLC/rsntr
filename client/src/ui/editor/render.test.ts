// @vitest-environment happy-dom
// The DOM projection (render.ts), the paste sanitiser (paste.ts) and the
// view's wiring (view.ts) in happy-dom. Input handling proper is tested in
// a real browser (e2e/editor).

import { describe, expect, test } from "vitest";

import { createFakeDocument } from "./fakeDocument";
import { parseDocument } from "./model";
import { isPlain, parseHtml } from "./paste";
import { Renderer } from "./render";
import { createBlipView } from "./index";

// Locations: <body> 0, <line/> 1-2, "abcde" 3-7, <line t="li"/> 8-9,
// "x" 10, <reply/> 11-12, "y" 13, <line/> 14-15.
const XML = '<body><line/>abcde<line t="li"/>x<reply id="t1"/>y<line/></body>';

function setup(xml = XML, annotate?: (d: Parameters<Parameters<ReturnType<typeof createFakeDocument>["edit"]>[0]>[0]) => void) {
  const doc = createFakeDocument(xml);
  doc.edit((d) => (annotate ?? ((m) => m.setAnnotation(5, 7, "style/fontWeight", "bold")))(d));
  const root = document.createElement("div");
  document.body.appendChild(root);
  const attached: string[] = [];
  const renderer = new Renderer(root, {
    anchors: { attach: (id) => attached.push(id), detach: () => {} },
    waveLinkHref: (v) => `#${v}`,
  });
  renderer.render(parseDocument(doc.document()));
  return { doc, root, renderer, attached };
}

describe("Renderer", () => {
  test("renders lines, runs, list markers and anchors", () => {
    const { root, attached } = setup();
    const lines = Array.from(root.children) as HTMLElement[];
    expect(lines).toHaveLength(3);
    expect(lines[0]!.textContent).toBe("abcde");
    expect((lines[0]!.children[1] as HTMLElement).style.fontWeight).toBe("bold");
    expect(lines[1]!.classList.contains("doc-li")).toBe(true);
    expect(lines[1]!.getAttribute("data-marker")).toBe("•");
    expect(lines[1]!.querySelector(".doc-anchor")?.getAttribute("contenteditable")).toBe("false");
    expect(attached).toEqual(["t1"]);
    expect(lines[2]!.querySelector("br.doc-filler")).not.toBeNull();
  });

  test("every caret location maps to the DOM and back", () => {
    const { doc, renderer } = setup();
    const model = parseDocument(doc.document());
    for (const line of model.lines) {
      for (let loc = line.contentStart; loc <= line.end; loc++) {
        // Inside the reply anchor (2 items) is not a caret location.
        const inAtom = line.items.some((i) => i.kind !== "text" && loc > line.contentStart + i.offset && loc < line.contentStart + i.offset + i.size);
        if (inAtom) {
          continue;
        }
        const point = renderer.domAt(loc)!;
        expect(renderer.locationOf(point.node, point.offset)?.location, `location ${loc}`).toBe(loc);
      }
    }
  });

  test("an operation re-renders only the line it touched", () => {
    const { doc, root, renderer } = setup();
    const before = Array.from(root.children);
    doc.edit((d) => d.insertText(4, "Z"));
    renderer.render(parseDocument(doc.document()));
    const after = Array.from(root.children);
    expect(after[0]).toBe(before[0]);
    expect(after[1]).toBe(before[1]);
    expect(after[0]!.textContent).toBe("aZbcde");
    // the untouched line's anchor element is the same node
    expect(after[1]!.querySelector(".doc-anchor")).toBe(before[1]!.querySelector(".doc-anchor"));
  });

  test("links get safe hrefs only", () => {
    // "bad good": bad 3-6, good 7-11
    const { root } = setup("<body><line/>bad good</body>", (d) => {
      d.setAnnotation(3, 6, "link/manual", "javascript:alert(1)");
      d.setAnnotation(7, 11, "link/manual", "https://x.example/");
    });
    const links = root.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0]!.getAttribute("href")).toBe("https://x.example/");
    expect(links[0]!.getAttribute("rel")).toBe("noopener noreferrer");
  });
});

describe("parseHtml", () => {
  test("keeps styles, links and lists; drops scripts, images and unsafe links", () => {
    const blocks = parseHtml(
      '<p><b>B</b><i>I</i><a href="javascript:x">J</a><a href="https://ok.example">L</a><script>evil()</script><img src=x onerror=alert(1)></p><ol><li>one</li></ol><h2>Head</h2>',
    );
    expect(blocks.map((b) => b.runs.map((r) => r.text).join(""))).toEqual(["BIJL", "one", "Head"]);
    const runs = blocks[0]!.runs;
    expect(runs[0]!.annotations["style/fontWeight"]).toBe("bold");
    expect(runs[1]!.annotations["style/fontStyle"]).toBe("italic");
    expect(runs[2]!.annotations["link/manual"]).toBeUndefined();
    expect(runs[3]!.annotations["link/manual"]).toBe("https://ok.example");
    expect(blocks[1]).toMatchObject({ type: "li", decimal: true });
    expect(blocks[2]).toMatchObject({ type: "h2" });
    expect(isPlain(parseHtml("<span>just text</span>"))).toBe(true);
  });
});

describe("EditorView", () => {
  test("follows remote operations and switches editing on and off", () => {
    const doc = createFakeDocument("<body><line/>hello</body>");
    const host = document.createElement("div");
    document.body.appendChild(host);
    const view = createBlipView({ host, doc, ariaLabel: "Message" });
    expect(view.element.textContent).toBe("hello");
    expect(view.element.getAttribute("contenteditable")).toBeNull();
    view.setEditable(true);
    expect(view.element.getAttribute("contenteditable")).toBe("true");
    expect(view.element.getAttribute("role")).toBe("textbox");
    doc.applyRemote(createOp(doc));
    expect(view.element.textContent).toBe("hello!");
    view.exec({ type: "insertText", text: "?" });
    expect(doc.xml()).toContain("hello");
    view.destroy();
    expect(host.childNodes).toHaveLength(0);
  });
});

/** An operation appending "!" to the fake document's text. */
function createOp(doc: ReturnType<typeof createFakeDocument>) {
  const scratch = createFakeDocument(doc.xml());
  let op = null as unknown as import("../../model/docop").DocOp;
  scratch.onOp((e) => {
    op = e.op;
  });
  scratch.edit((d) => d.insertText(8, "!"));
  return op;
}
