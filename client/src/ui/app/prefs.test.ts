// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";

import { DEFAULT_PREFS, PrefsStore, attributes, parsePrefs } from "./prefs";

const NO_SYSTEM = { dark: false, moreContrast: false, reduceMotion: false };

function fakeMedia(state: Record<string, boolean>) {
  const listeners: Record<string, (() => void)[]> = {};
  const media = (query: string) =>
    ({
      get matches() {
        return state[query] ?? false;
      },
      addEventListener: (_: string, f: () => void) => (listeners[query] ??= []).push(f),
    }) as unknown as MediaQueryList;
  const change = (query: string, value: boolean) => {
    state[query] = value;
    for (const f of listeners[query] ?? []) f();
  };
  return { media, change };
}

describe("prefs", () => {
  it("falls back to the defaults on missing, broken or unknown values", () => {
    expect(parsePrefs(null)).toEqual(DEFAULT_PREFS);
    expect(parsePrefs("{oops")).toEqual(DEFAULT_PREFS);
    expect(parsePrefs('{"theme":"pink","textSize":"150","underlineLinks":"yes"}')).toEqual({
      ...DEFAULT_PREFS,
      textSize: "150",
    });
  });

  it("resolves system values from the media queries", () => {
    expect(attributes(DEFAULT_PREFS, { dark: true, moreContrast: true, reduceMotion: true })).toEqual({
      "data-theme": "dark",
      "data-contrast": "more",
      "data-motion": "reduce",
      "data-text-size": "system",
      "data-spacing": "normal",
      "data-targets": "normal",
      "data-underline-links": null,
    });
    const chosen = { ...DEFAULT_PREFS, theme: "light", contrast: "normal", underlineLinks: true } as const;
    expect(attributes(chosen, { dark: true, moreContrast: true, reduceMotion: false })).toMatchObject({
      "data-theme": "light",
      "data-contrast": "normal",
      "data-motion": "normal",
      "data-underline-links": "",
    });
    expect(attributes({ ...DEFAULT_PREFS, motion: "reduce" }, NO_SYSTEM)["data-motion"]).toBe("reduce");
  });

  it("keeps changes, applies them to <html> and follows the system", () => {
    const saved = new Map<string, string>();
    const storage = { getItem: (k: string) => saved.get(k) ?? null, setItem: (k: string, v: string) => void saved.set(k, v) };
    const { media, change } = fakeMedia({});
    const store = new PrefsStore(storage, media);
    const root = document.createElement("html");
    const stop = store.apply(root);
    expect(root.getAttribute("data-theme")).toBe("light");
    change("(prefers-color-scheme: dark)", true);
    expect(root.getAttribute("data-theme")).toBe("dark");
    store.set("textSize", "200");
    store.set("underlineLinks", true);
    expect(root.getAttribute("data-text-size")).toBe("200");
    expect(root.hasAttribute("data-underline-links")).toBe(true);
    expect(new PrefsStore(storage, media).prefs.value).toMatchObject({ textSize: "200", underlineLinks: true });
    stop();
  });

  it("works without storage", () => {
    const broken = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };
    const store = new PrefsStore(broken, null);
    store.set("spacing", "reading");
    expect(store.prefs.value.spacing).toBe("reading");
  });
});
