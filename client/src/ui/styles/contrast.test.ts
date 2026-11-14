// The colour tokens meet WCAG 2.2 AA: 4.5:1 for text (1.4.3) and 3:1 for
// the edges of controls and for focus (1.4.11), in light and dark; in high
// contrast 7:1 for text (1.4.6) and 4.5:1 for edges.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./tokens.css", import.meta.url), "utf8");

/** The `--c-*` hex colours of the first block after `header`. */
function colours(header: string): Record<string, string> {
  let start = css.indexOf(header);
  if (start < 0) {
    throw new Error(`no ${header} in tokens.css`);
  }
  start = css.indexOf("{", start) + 1;
  const body = css.slice(start, css.indexOf("}", start));
  return Object.fromEntries([...body.matchAll(/--c-([\w-]+):\s*(#[0-9a-f]{6})\b/gi)].map((m) => [m[1]!, m[2]!]));
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = Number.parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}

export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi! + 0.05) / (lo! + 0.05);
}

const light = colours(":root {");
const dark = { ...light, ...colours(':root[data-theme="dark"] {') };
const themes: Record<string, Record<string, string>> = {
  light,
  dark,
  "light, high contrast": { ...light, ...colours(':root[data-theme="light"][data-contrast="more"] {') },
  "dark, high contrast": { ...dark, ...colours(':root[data-theme="dark"][data-contrast="more"] {') },
};
/** The required ratio: higher in high contrast. */
const required = (theme: string, min: number): number => (theme.includes("high") ? (min >= 4.5 ? 7 : 4.5) : min);

const SURFACES = ["bg", "surface", "surface-2", "surface-3", "accent-soft", "unread-soft", "inline-thread"];
const TEXTS = ["text", "text-2", "text-3", "link", "accent", "danger", "warning", "ok", "unread"];
const PAIRS: [string, string, number][] = [
  ...TEXTS.flatMap((t) => SURFACES.map((s): [string, string, number] => [t, s, 4.5])),
  ["text-on-accent", "accent", 4.5],
  ["text-on-accent", "accent-hover", 4.5],
  ["text-on-danger", "danger", 4.5],
  ["unread-text", "unread", 4.5],
  ["danger", "danger-soft", 4.5],
  ["warning", "warning-soft", 4.5],
  ["topbar-text", "topbar", 4.5],
  ["topbar-text-2", "topbar", 4.5],
  ["topbar-warn", "topbar", 4.5],
  ["topbar-warn-text", "topbar-warn", 4.5],
  ...["bg", "surface", "surface-2"].map((s): [string, string, number] => ["border-strong", s, 3]),
  ...["bg", "surface", "surface-2"].map((s): [string, string, number] => ["accent", s, 3]),
];

describe("colour contrast of the tokens", () => {
  for (const [name, theme] of Object.entries(themes)) {
    it(`${name}: every pair meets AA`, () => {
      const failing = PAIRS.filter(([fg, bg, min]) => {
        expect(theme[fg], `--c-${fg}`).toBeDefined();
        expect(theme[bg], `--c-${bg}`).toBeDefined();
        return contrast(theme[fg]!, theme[bg]!) < required(name, min);
      }).map(([fg, bg, min]) => `${fg} on ${bg}: ${contrast(theme[fg]!, theme[bg]!).toFixed(2)} < ${required(name, min)}`);
      expect(failing).toEqual([]);
    });
  }

  it("computes the WCAG ratio", () => {
    expect(contrast("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrast("#777777", "#ffffff")).toBeCloseTo(4.48, 2);
  });
});
