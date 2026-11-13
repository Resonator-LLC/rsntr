// @vitest-environment happy-dom

import { afterEach, describe, expect, it } from "vitest";

import { type ViewportWindow, followVisualViewport } from "./viewport";

class FakeViewport extends EventTarget {
  height = 844;
  offsetTop = 0;
  scale = 1;
}

function fakeWindow(vv: FakeViewport): ViewportWindow & { scrolled: [number, number][]; y: number } {
  const w = {
    visualViewport: vv,
    innerHeight: 844,
    scrollX: 0,
    y: 0,
    get scrollY() {
      return w.y;
    },
    document,
    scrolled: [] as [number, number][],
    scrollTo(x: number, y: number) {
      w.scrolled.push([x, y]);
      w.y = y;
    },
    requestAnimationFrame(cb: () => void) {
      queueMicrotask(cb);
      return 1;
    },
    cancelAnimationFrame() {},
    getComputedStyle: (el: Element) => getComputedStyle(el),
  };
  return w;
}

const root = document.documentElement;
let stop = () => {};
afterEach(() => stop());

describe("following the visual viewport", () => {
  it("sizes the app to the visible part and marks the keyboard", async () => {
    const vv = new FakeViewport();
    const win = fakeWindow(vv);
    stop = followVisualViewport(win);
    expect(root.style.getPropertyValue("--viewport-h")).toBe("844px");
    expect(root.classList.contains("keyboard-open")).toBe(false);

    // The keyboard opens; iOS pans the page to the caret.
    vv.height = 508;
    win.y = 120;
    vv.dispatchEvent(new Event("resize"));
    await Promise.resolve();
    expect(root.style.getPropertyValue("--viewport-h")).toBe("508px");
    expect(root.style.getPropertyValue("--keyboard-h")).toBe("336px");
    expect(root.classList.contains("keyboard-open")).toBe(true);
    expect(win.scrolled).toEqual([[0, 0]]);

    vv.height = 844;
    vv.dispatchEvent(new Event("resize"));
    await Promise.resolve();
    expect(root.classList.contains("keyboard-open")).toBe(false);
  });

  it("leaves a pinch zoom alone, and cleans up", async () => {
    const vv = new FakeViewport();
    const win = fakeWindow(vv);
    stop = followVisualViewport(win);
    vv.scale = 2;
    vv.height = 300;
    vv.dispatchEvent(new Event("resize"));
    await Promise.resolve();
    expect(root.style.getPropertyValue("--viewport-h")).toBe("844px");
    stop();
    stop = () => {};
    expect(root.style.getPropertyValue("--viewport-h")).toBe("");
  });

  it("does nothing without the API", () => {
    const win = { ...fakeWindow(new FakeViewport()), visualViewport: null };
    followVisualViewport(win)();
    expect(root.style.getPropertyValue("--viewport-h")).toBe("");
  });
});
