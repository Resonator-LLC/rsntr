// The on-screen keyboard. Chrome on Android shrinks the layout above it
// (`interactive-widget=resizes-content` in the viewport meta); iOS Safari
// does not: it leaves the layout as tall as the screen, covers its bottom
// with the keyboard and pans the page to bring the caret into view, which
// takes the edit toolbar out of view and leaves the page scrolled.
//
// So the layout follows the visual viewport instead: the root gets
// `--viewport-h` (the visible height, which mobile.css gives the app) and
// `--keyboard-h`, and the `keyboard-open` class while the keyboard is up;
// the page is panned back to the top, and the caret is scrolled into view
// inside its own scrolling panel.

/** A keyboard is open when the visible height is this much below the page's. */
const KEYBOARD_MIN = 120;

/** The part of the viewport the visual viewport API reports. */
export interface VisualViewportLike extends EventTarget {
  readonly height: number;
  readonly offsetTop: number;
  readonly scale: number;
}

export interface ViewportWindow {
  readonly visualViewport: VisualViewportLike | null;
  readonly innerHeight: number;
  readonly scrollX: number;
  readonly scrollY: number;
  readonly document: Document;
  scrollTo(x: number, y: number): void;
  requestAnimationFrame(callback: () => void): number;
  cancelAnimationFrame(handle: number): void;
  getComputedStyle(element: Element): CSSStyleDeclaration;
}

/** The nearest ancestor of `node` that scrolls vertically. */
function scroller(node: Node | null, win: ViewportWindow): HTMLElement | null {
  let el: Element | null = node instanceof Element ? node : (node?.parentElement ?? null);
  while (el !== null && el !== win.document.documentElement) {
    const overflow = win.getComputedStyle(el).overflowY;
    if ((overflow === "auto" || overflow === "scroll") && el.scrollHeight > el.clientHeight) {
      return el as HTMLElement;
    }
    el = el.parentElement;
  }
  return null;
}

/** Scrolls the caret of a focused editable into the visible part. */
export function revealCaret(win: ViewportWindow, visibleHeight: number, margin = 24): void {
  const doc = win.document;
  const active = doc.activeElement;
  if (!(active instanceof HTMLElement) || !(active.isContentEditable || active.matches("input, textarea"))) {
    return;
  }
  const selection = doc.getSelection();
  let rect: DOMRect | null = null;
  if (active.isContentEditable && selection !== null && selection.rangeCount > 0) {
    const range = selection.getRangeAt(0);
    const rects = range.getClientRects();
    rect = rects.length > 0 ? rects[rects.length - 1]! : range.getBoundingClientRect();
    if (rect.height === 0 && rect.top === 0) {
      rect = null;
    }
  }
  rect ??= active.getBoundingClientRect();
  const panel = scroller(active, win);
  if (panel === null) {
    return;
  }
  const box = panel.getBoundingClientRect();
  const bottom = Math.min(box.bottom, visibleHeight) - margin;
  const top = box.top + margin;
  if (rect.bottom > bottom) {
    panel.scrollTop += rect.bottom - bottom;
  } else if (rect.top < top) {
    panel.scrollTop -= top - rect.top;
  }
}

/**
 * Keeps `--viewport-h`, `--keyboard-h` and `keyboard-open` on the root in
 * step with the visual viewport. Returns the function that stops it.
 */
export function followVisualViewport(win: ViewportWindow): () => void {
  const vv = win.visualViewport;
  if (vv === null) {
    return () => {};
  }
  const root = win.document.documentElement;
  let frame = 0;
  let wasOpen = false;
  const update = (): void => {
    frame = 0;
    // Pinch zoom (when allowed, e.g. by accessibility settings) is the
    // user's: leave the layout alone then.
    if (vv.scale > 1.01) {
      return;
    }
    const height = Math.round(vv.height);
    const keyboard = Math.max(0, Math.round(win.innerHeight - vv.height));
    const open = keyboard >= KEYBOARD_MIN;
    root.style.setProperty("--viewport-h", `${height}px`);
    root.style.setProperty("--keyboard-h", `${keyboard}px`);
    root.classList.toggle("keyboard-open", open);
    // iOS pans the page under the keyboard; the app now fits what is
    // visible, so pan it back.
    if (win.scrollY !== 0 || win.scrollX !== 0) {
      win.scrollTo(0, 0);
    }
    if (open) {
      revealCaret(win, height);
    }
    wasOpen = open;
  };
  const schedule = (): void => {
    if (frame === 0) {
      frame = win.requestAnimationFrame(update);
    }
  };
  vv.addEventListener("resize", schedule);
  vv.addEventListener("scroll", schedule);
  const onFocus = (): void => {
    if (wasOpen) {
      schedule();
    }
  };
  win.document.addEventListener("focusin", onFocus);
  update();
  return () => {
    vv.removeEventListener("resize", schedule);
    vv.removeEventListener("scroll", schedule);
    win.document.removeEventListener("focusin", onFocus);
    if (frame !== 0) {
      win.cancelAnimationFrame(frame);
    }
    root.style.removeProperty("--viewport-h");
    root.style.removeProperty("--keyboard-h");
    root.classList.remove("keyboard-open");
  };
}
