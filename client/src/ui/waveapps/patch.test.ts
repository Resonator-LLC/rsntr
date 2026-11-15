// @vitest-environment happy-dom
// The app view patcher: the whitelist, updates in place, events, forms and
// secret fields.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AppUiNode } from "../../wire";
import { type AppEventOut, AppPatcher, INPUT_DEBOUNCE_MS } from "./patch";

function node(tag: string, parent?: number, extra: Partial<AppUiNode> = {}): AppUiNode {
  return { tag, attrs: [], on: [], ...(parent !== undefined ? { parent } : {}), ...extra };
}

function attrs(record: Record<string, string>): AppUiNode["attrs"] {
  return Object.entries(record).map(([name, value]) => ({ name, value }));
}

let root: HTMLElement;
let sent: AppEventOut[];
let patcher: AppPatcher;

beforeEach(() => {
  root = document.createElement("div");
  document.body.appendChild(root);
  sent = [];
  patcher = new AppPatcher(root, {
    send: (event) => sent.push(event),
    attachmentUrl: (id) => `/attachment/${id}`,
    appName: "counter",
  });
});

afterEach(() => {
  root.remove();
  vi.useRealTimers();
});

describe("AppPatcher", () => {
  it("builds the tree from parent indexes", () => {
    patcher.apply([
      node("div", undefined, { attrs: attrs({ class: "app-row" }) }),
      node("span", 0, { text: "0" }),
      node("button", 0, { text: "+1", on: ["click"] }),
    ]);
    expect(root.innerHTML).toBe('<div class="app-row"><span>0</span><button>+1</button></div>');
  });

  it("leaves out tags, attributes and events that are not allowed, and what is under them", () => {
    patcher.apply([
      node("div"),
      node("script", 0, { text: "alert(1)" }),
      node("span", 1, { text: "inside a script" }),
      node("iframe", 0),
      node("surface", 0),
      node("a", 0, { attrs: attrs({ href: "https://example.com" }), text: "link" }),
      node("p", 0, {
        text: "ok",
        attrs: attrs({ onclick: "alert(1)", style: "color:red", href: "x", class: "c" }),
        on: ["mouseover"],
      }),
    ]);
    expect(root.innerHTML).toBe('<div><p class="c">ok</p></div>');
  });

  it("keeps elements across renders and only changes what changed", () => {
    patcher.apply([node("div"), node("span", 0, { text: "0" }), node("button", 0, { text: "+1" })]);
    const [div, span, button] = patcher.elements();
    patcher.apply([node("div"), node("span", 0, { text: "1" }), node("button", 0, { text: "+1" })]);
    const after = patcher.elements();
    expect(after[0]).toBe(div);
    expect(after[1]).toBe(span);
    expect(after[2]).toBe(button);
    expect(span!.textContent).toBe("1");
    patcher.apply([node("div"), node("p", 0, { text: "gone" })]);
    expect(root.innerHTML).toBe("<div><p>gone</p></div>");
  });

  it("sends the events a node asks for, with its index", () => {
    patcher.apply([node("div"), node("button", 0, { text: "a", on: ["click"] }), node("button", 0, { text: "b" })]);
    const [, a, b] = patcher.elements() as HTMLElement[];
    a!.click();
    b!.click();
    expect(sent).toEqual([{ node: 1, name: "click" }]);
  });

  it("debounces input and keeps what the user typed until the app changes the value", () => {
    vi.useFakeTimers();
    patcher.apply([node("input", undefined, { attrs: attrs({ value: "a" }), on: ["input", "change"] })]);
    const input = patcher.elements()[0] as HTMLInputElement;
    expect(input.value).toBe("a");
    input.value = "ab";
    input.dispatchEvent(new Event("input"));
    input.value = "abc";
    input.dispatchEvent(new Event("input"));
    vi.advanceTimersByTime(INPUT_DEBOUNCE_MS + 1);
    expect(sent).toEqual([{ node: 0, name: "input", value: "abc" }]);
    // A render with the same value does not undo the typing...
    patcher.apply([node("input", undefined, { attrs: attrs({ value: "a" }), on: ["input"] })]);
    expect(input.value).toBe("abc");
    // ...a new value from the app replaces it.
    patcher.apply([node("input", undefined, { attrs: attrs({ value: "" }), on: ["input"] })]);
    expect(input.value).toBe("");
  });

  it("submits a form as one event with its fields, and never navigates", () => {
    patcher.apply([
      node("form", undefined, { on: ["submit"] }),
      node("input", 0, { attrs: attrs({ name: "who", value: "ann" }) }),
      node("input", 0, { attrs: attrs({ name: "pin", type: "password" }) }),
      node("input", 0, { attrs: attrs({ name: "ok", type: "checkbox", checked: "true" }) }),
      node("input", 0, { attrs: attrs({ name: "no", type: "checkbox" }) }),
      node("button", 0, { text: "Go" }),
    ]);
    const els = patcher.elements();
    (els[2] as HTMLInputElement).value = "1234";
    const event = new Event("submit", { cancelable: true });
    els[0]!.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(sent).toEqual([
      {
        node: 0,
        name: "submit",
        fields: [
          { name: "who", value: "ann" },
          { name: "pin", value: "1234" },
          { name: "ok", value: "on" },
        ],
      },
    ]);
  });

  it("draws password fields as masked text fields the password manager ignores", () => {
    patcher.apply([node("input", undefined, { attrs: attrs({ type: "password", name: "pin" }) })]);
    const input = patcher.elements()[0] as HTMLInputElement;
    expect(input.getAttribute("type")).toBe("text");
    expect(input.classList.contains("app-secret")).toBe(true);
    expect(input.getAttribute("autocomplete")).toBe("off");
    expect(input.getAttribute("title")).toBe("Sent to counter");
  });

  it("allows images of attachments only", () => {
    patcher.apply([
      node("div"),
      node("img", 0, { attrs: attrs({ src: "example.com/abc123", alt: "x" }) }),
      node("img", 0, { attrs: attrs({ src: "https://evil.example/x.png" }) }),
      node("img", 0, { attrs: attrs({ src: "javascript:alert(1)" }) }),
    ]);
    const [, ok, remote, script] = patcher.elements();
    expect(ok!.getAttribute("src")).toBe("/attachment/example.com/abc123");
    expect(remote!.hasAttribute("src")).toBe(false);
    expect(script!.hasAttribute("src")).toBe(false);
  });

  it("keeps the fixed nodes of the root", () => {
    const link = document.createElement("link");
    link.setAttribute("data-app-fixed", "");
    root.appendChild(link);
    patcher.apply([node("p", undefined, { text: "a" })]);
    patcher.apply([node("span", undefined, { text: "b" })]);
    expect(root.firstChild).toBe(link);
    expect(root.childNodes.length).toBe(2);
  });
});
