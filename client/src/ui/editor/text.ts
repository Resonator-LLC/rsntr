// Text helpers: what may be inserted, where characters and words end, and
// the minimal difference between two strings.

function isHigh(u: number): boolean {
  return u >= 0xd800 && u <= 0xdbff;
}

function isLow(u: number): boolean {
  return u >= 0xdc00 && u <= 0xdfff;
}

/**
 * Text as a blip may hold it: no control characters (tabs and other
 * whitespace controls become spaces), no bidi formatting characters, no
 * noncharacters, no unpaired surrogates (the document schema rejects them).
 * Line breaks are the caller's business: none may be left in `text`.
 */
export function sanitizeText(text: string): string {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    const u = text.charCodeAt(i);
    if (isHigh(u)) {
      const next = text.charCodeAt(i + 1);
      if (isLow(next)) {
        const c = (u - 0xd800) * 0x400 + (next - 0xdc00) + 0x10000;
        i++;
        const low16 = c & 0xffff;
        if (low16 === 0xfffe || low16 === 0xffff || (c >= 0xe0000 && c <= 0xe007f)) {
          continue;
        }
        out += text[i - 1]! + text[i]!;
      } else {
        out += "\uFFFD";
      }
      continue;
    }
    if (isLow(u)) {
      out += "\uFFFD";
      continue;
    }
    if (u === 0x09 || u === 0x0b || u === 0x0c) {
      out += " ";
      continue;
    }
    if (u <= 0x1f || (u >= 0x7f && u <= 0x9f)) {
      continue;
    }
    if ((u >= 0x206a && u <= 0x206f) || u === 0x200e || u === 0x200f || (u >= 0x202a && u <= 0x202e)) {
      continue;
    }
    if (u === 0xfffe || u === 0xffff || (u >= 0xfdd0 && u <= 0xfdef) || u === 0xfffc) {
      continue;
    }
    out += text[i]!;
  }
  return out;
}

/** Splits pasted or typed text into lines. */
export function splitLines(text: string): string[] {
  return text.split(/\r\n|\r|\n|\u2028|\u2029/);
}

type SegmenterLike = { segment(text: string): Iterable<{ index: number; segment: string; isWordLike?: boolean }> };
type SegmenterCtor = new (locale?: string, options?: { granularity: string }) => SegmenterLike;

function segmenter(granularity: "grapheme" | "word"): SegmenterLike | null {
  const ctor = (Intl as unknown as { Segmenter?: SegmenterCtor }).Segmenter;
  return ctor === undefined ? null : new ctor(undefined, { granularity });
}

let graphemes: SegmenterLike | null | undefined;

function boundaries(text: string): number[] {
  if (graphemes === undefined) {
    graphemes = segmenter("grapheme");
  }
  const result: number[] = [];
  if (graphemes !== null) {
    for (const s of graphemes.segment(text)) {
      result.push(s.index);
    }
  } else {
    for (let i = 0; i < text.length; i++) {
      if (!(i > 0 && isLow(text.charCodeAt(i)) && isHigh(text.charCodeAt(i - 1)))) {
        result.push(i);
      }
    }
  }
  result.push(text.length);
  return result;
}

/** The start of the grapheme cluster that ends at (or contains) `index`. */
export function prevGrapheme(text: string, index: number): number {
  let prev = 0;
  for (const b of boundaries(text)) {
    if (b >= index) {
      break;
    }
    prev = b;
  }
  return prev;
}

/** The end of the grapheme cluster that starts at (or contains) `index`. */
export function nextGrapheme(text: string, index: number): number {
  for (const b of boundaries(text)) {
    if (b > index) {
      return b;
    }
  }
  return text.length;
}

/** Moves an index off the middle of a surrogate pair (towards the start). */
export function alignToCodePoint(text: string, index: number): number {
  if (index > 0 && index < text.length && isLow(text.charCodeAt(index)) && isHigh(text.charCodeAt(index - 1))) {
    return index - 1;
  }
  return index;
}

const WORD = /[\p{L}\p{N}\p{M}_]/u;

function cls(text: string, i: number): number {
  const ch = String.fromCodePoint(text.codePointAt(i) ?? 32);
  return /\s/.test(ch) ? 0 : WORD.test(ch) ? 1 : 2;
}

/** Where a word deletion backwards from `index` starts. */
export function prevWord(text: string, index: number): number {
  let i = index;
  const step = (): number => prevGrapheme(text, i);
  while (i > 0 && cls(text, step()) === 0) {
    i = step();
  }
  if (i === 0) {
    return 0;
  }
  const kind = cls(text, step());
  while (i > 0 && cls(text, step()) === kind) {
    i = step();
  }
  return i;
}

/** Where a word deletion forwards from `index` ends. */
export function nextWord(text: string, index: number): number {
  let i = index;
  const n = text.length;
  while (i < n && cls(text, i) === 0) {
    i = nextGrapheme(text, i);
  }
  if (i >= n) {
    return n;
  }
  const kind = cls(text, i);
  while (i < n && cls(text, i) === kind) {
    i = nextGrapheme(text, i);
  }
  return i;
}

export interface TextDiff {
  /** Where the texts start to differ. */
  readonly start: number;
  /** How many units of the old text are replaced. */
  readonly removed: number;
  readonly inserted: string;
}

/**
 * The single replacement that turns `before` into `after`, never cutting a
 * surrogate pair. `caret` (an index into `after`), if given, is where the
 * change is known to end: among equivalent diffs (typing a repeated
 * character) the one ending there is chosen.
 */
export function diffText(before: string, after: string, caret?: number): TextDiff | null {
  if (before === after) {
    return null;
  }
  const max = Math.min(before.length, after.length);
  let suffix = 0;
  const suffixLimit = caret === undefined ? max : Math.min(max, Math.max(after.length - caret, 0));
  while (
    suffix < suffixLimit &&
    before.charCodeAt(before.length - 1 - suffix) === after.charCodeAt(after.length - 1 - suffix)
  ) {
    suffix++;
  }
  let prefix = 0;
  const prefixLimit = max - suffix;
  while (prefix < prefixLimit && before.charCodeAt(prefix) === after.charCodeAt(prefix)) {
    prefix++;
  }
  // Keep whole code points on both sides.
  if (prefix > 0 && isHigh(before.charCodeAt(prefix - 1))) {
    prefix--;
  }
  const cut = (s: string): boolean => {
    const at = s.length - suffix;
    return suffix > 0 && at > 0 && isLow(s.charCodeAt(at)) && isHigh(s.charCodeAt(at - 1));
  };
  if (cut(before) || cut(after)) {
    suffix--;
  }
  return {
    start: prefix,
    removed: before.length - suffix - prefix,
    inserted: after.slice(prefix, after.length - suffix),
  };
}

// ---- URLs ----

// The original has no client-side auto-linker (`link/auto` was written by
// an external agent); this detection is ours.
const URL = /(?:https?:\/\/|www\.)[^\s<>"\uFFFC]+/gi;

export interface UrlMatch {
  readonly start: number;
  readonly end: number;
  readonly href: string;
}

/** The URLs in a line of text. */
export function findUrls(text: string): UrlMatch[] {
  const result: UrlMatch[] = [];
  URL.lastIndex = 0;
  for (let m = URL.exec(text); m !== null; m = URL.exec(text)) {
    const start = m.index;
    if (start > 0 && /[\p{L}\p{N}_@.\/-]/u.test(text[start - 1]!)) {
      continue;
    }
    let url = m[0];
    // Trailing punctuation belongs to the sentence, not to the URL.
    for (;;) {
      const last = url[url.length - 1]!;
      if (/[.,;:!?'*]/.test(last)) {
        url = url.slice(0, -1);
      } else if (last === ")" && !url.includes("(")) {
        url = url.slice(0, -1);
      } else if (last === "]" && !url.includes("[")) {
        url = url.slice(0, -1);
      } else {
        break;
      }
    }
    const isWww = /^www\./i.test(url);
    const rest = url.replace(/^(?:https?:\/\/|www\.)/i, "");
    if (rest === "" || (isWww && !/^[^/?#]*\.[^/?#]/.test(rest))) {
      continue;
    }
    result.push({ start, end: start + url.length, href: isWww ? `http://${url}` : url });
  }
  return result;
}

/** A link target as typed by a user, in the form it is stored. */
export function normalizeLink(raw: string): string | null {
  const value = raw.trim();
  if (value === "" || /[\u0000- \u007f]/.test(value)) {
    return null;
  }
  if (/^(?:https?:\/\/|mailto:|wave:\/\/)/i.test(value)) {
    return value;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(value)) {
    return null;
  }
  if (/^[^@\s/]+@[^@\s/]+\.[^@\s/]+$/.test(value)) {
    return `mailto:${value}`;
  }
  if (/^[\w-]+(\.[\w-]+)+(?::\d+)?(?:[/?#].*)?$/.test(value)) {
    return `http://${value}`;
  }
  return null;
}
