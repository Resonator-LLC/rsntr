// From annotation values to CSS, with validation: the values come from
// other participants and are applied through CSSOM only (never as markup).

import {
  LINK_AUTO,
  LINK_MANUAL,
  LINK_PLAIN,
  LINK_WAVE,
  type RunAnnotations,
  STYLE_BG_COLOR,
  STYLE_COLOR,
  STYLE_FONT_FAMILY,
  STYLE_FONT_SIZE,
  STYLE_FONT_STYLE,
  STYLE_FONT_WEIGHT,
  STYLE_TEXT_DECORATION,
  STYLE_VERTICAL_ALIGN,
} from "./model";

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const RGB_COLOR =
  /^rgba?\(\s*\d{1,3}\s*,\s*\d{1,3}\s*,\s*\d{1,3}\s*(?:,\s*(?:0|1|0?\.\d{1,4})\s*)?\)$/i;
const NAMED_COLOR = /^[a-z]{3,24}$/i;
const NOT_COLORS = new Set(["inherit", "initial", "unset", "revert", "transparent", "currentcolor"]);

/** The colour if it is a plain colour value (hex, rgb(), a name), else null. */
export function safeColor(value: string | null | undefined): string | null {
  if (value === null || value === undefined) {
    return null;
  }
  const v = value.trim();
  if (HEX_COLOR.test(v) || RGB_COLOR.test(v)) {
    return v;
  }
  if (NAMED_COLOR.test(v) && !NOT_COLORS.has(v.toLowerCase())) {
    return v;
  }
  return null;
}

const FONT_FAMILY = /^[a-z0-9 ,'"-]{1,120}$/i;

/** A font family list of plain names, else null. */
export function safeFontFamily(value: string | null | undefined): string | null {
  if (value === null || value === undefined) {
    return null;
  }
  const v = value.trim();
  return FONT_FAMILY.test(v) ? v : null;
}

const FONT_SIZE = /^(\d{1,3}(?:\.\d{1,20})?)(px|pt|em|rem|%)$/;
const SIZE_LIMITS: Readonly<Record<string, readonly [number, number]>> = {
  px: [6, 96],
  pt: [5, 72],
  em: [0.4, 6],
  rem: [0.4, 6],
  "%": [40, 600],
};

/** A font size within sane bounds, else null. */
export function safeFontSize(value: string | null | undefined): string | null {
  if (value === null || value === undefined) {
    return null;
  }
  const m = FONT_SIZE.exec(value.trim());
  if (m === null) {
    return null;
  }
  const n = Number.parseFloat(m[1]!);
  const limits = SIZE_LIMITS[m[2]!]!;
  return n >= limits[0] && n <= limits[1] ? `${m[1]}${m[2]}` : null;
}

export function isBold(value: string | null | undefined): boolean {
  return value === "bold" || value === "bolder" || value === "700" || value === "800" || value === "900";
}

export function isItalic(value: string | null | undefined): boolean {
  return value === "italic" || value === "oblique";
}

export function decorations(value: string | null | undefined): { underline: boolean; strike: boolean } {
  const tokens = (value ?? "").split(/\s+/);
  return { underline: tokens.includes("underline"), strike: tokens.includes("line-through") };
}

export function decorationValue(underline: boolean, strike: boolean): string | null {
  if (underline && strike) {
    return "underline line-through";
  }
  return underline ? "underline" : strike ? "line-through" : null;
}

/** Applies the style annotations of a run to its element. */
export function applyRunStyle(el: HTMLElement, a: RunAnnotations): void {
  const s = el.style;
  s.fontWeight = isBold(a[STYLE_FONT_WEIGHT]) ? "bold" : "";
  s.fontStyle = isItalic(a[STYLE_FONT_STYLE]) ? "italic" : "";
  const d = decorations(a[STYLE_TEXT_DECORATION]);
  s.textDecoration = decorationValue(d.underline, d.strike) ?? "";
  const va = a[STYLE_VERTICAL_ALIGN];
  s.verticalAlign = va === "super" || va === "sub" ? va : "";
  el.classList.toggle("doc-script", va === "super" || va === "sub");
  s.color = safeColor(a[STYLE_COLOR]) ?? "";
  s.backgroundColor = safeColor(a[STYLE_BG_COLOR]) ?? "";
  s.fontFamily = safeFontFamily(a[STYLE_FONT_FAMILY]) ?? "";
  s.fontSize = safeFontSize(a[STYLE_FONT_SIZE]) ?? "";
}

export type LinkKind = "manual" | "auto" | "wave";

export interface LinkInfo {
  readonly kind: LinkKind;
  readonly value: string;
}

/**
 * The link of a run, if any, in the original's order of precedence
 * (`LinkAnnotationHandler.getLink`): link, link/manual, link/wave, link/auto.
 */
export function linkOf(a: RunAnnotations): LinkInfo | null {
  const manual = a[LINK_PLAIN] ?? a[LINK_MANUAL];
  if (manual !== undefined) {
    return { kind: "manual", value: manual };
  }
  const wave = a[LINK_WAVE];
  if (wave !== undefined) {
    return { kind: "wave", value: wave };
  }
  const auto = a[LINK_AUTO];
  if (auto !== undefined) {
    return { kind: "auto", value: auto };
  }
  return null;
}

/** Only these may be navigated to from a document. */
const WEB_LINK = /^(?:https?:\/\/|mailto:)/i;
const HAS_SCHEME = /^[a-z][a-z0-9+.-]*:/i;
const CONTROL = /[\u0000- \u007f]/;

export interface Href {
  readonly href: string;
  /** Open in a new browsing context (web links), or in place (wave links). */
  readonly external: boolean;
}

/**
 * The navigable form of a link annotation, or null if it must render as
 * plain text (javascript:, data:, anything unknown).
 */
export function hrefOf(link: LinkInfo, waveLinkHref: (value: string) => string): Href | null {
  const value = link.value.trim();
  if (value === "" || CONTROL.test(value)) {
    return null;
  }
  if (WEB_LINK.test(value)) {
    return { href: value, external: true };
  }
  if (/^wave:\/\//i.test(value)) {
    return { href: waveLinkHref(value.slice("wave://".length)), external: false };
  }
  if (link.kind === "wave" && !HAS_SCHEME.test(value)) {
    return { href: waveLinkHref(value), external: false };
  }
  if (!HAS_SCHEME.test(value) && /^[\w-]+(\.[\w-]+)+([/?#].*)?$/.test(value)) {
    // "example.com/x": the original prepends http:// (`Link.normalizeLink`).
    return { href: `http://${value}`, external: true };
  }
  return null;
}
