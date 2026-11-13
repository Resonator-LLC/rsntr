// UTF-16 helpers.
//
// JavaScript strings are sequences of UTF-16 code units, so lengths, retain
// counts and split offsets are native `.length` / `.slice`, and key ordering
// is the native `<` on strings (the same order as Java `String.compareTo`).
// Unlike a Rust `String`, a JavaScript string can hold unpaired surrogates;
// those are rejected as invalid text, as in the Java original.
//
// Derived from Apache Wave (`Utf16Util`), licensed under the Apache License,
// Version 2.0.

import { OperationError } from "./error";

/** Unicode REPLACEMENT CHARACTER. */
export const REPLACEMENT_CHARACTER = "�";

/** Largest count used by the saturating arithmetic on positions. */
export const MAX_COUNT = Number.MAX_SAFE_INTEGER;

/** Addition of non-negative counts, saturating at {@link MAX_COUNT}. */
export function saturatingAdd(a: number, b: number): number {
  const sum = a + b;
  return sum < MAX_COUNT ? sum : MAX_COUNT;
}

/** Length of `s` in UTF-16 code units. */
export function utf16Len(s: string): number {
  return s.length;
}

/**
 * Splits `s` at a UTF-16 offset.
 *
 * Throws if the offset is out of bounds or lands inside a surrogate pair.
 */
export function splitAtUtf16(s: string, offset: number): [string, string] {
  if (!Number.isInteger(offset) || offset < 0 || offset > s.length) {
    throw new OperationError({ kind: "offsetOutOfBounds", offset, len: s.length });
  }
  if (
    offset > 0 &&
    offset < s.length &&
    isHighSurrogate(s.charCodeAt(offset - 1)) &&
    isLowSurrogate(s.charCodeAt(offset))
  ) {
    throw new OperationError({ kind: "surrogateSplit", offset });
  }
  return [s.slice(0, offset), s.slice(offset)];
}

/**
 * Orders strings like Java `String.compareTo`, i.e. by UTF-16 code units.
 * All key ordering (attribute names, annotation keys) goes through this
 * function.
 */
export function cmpUtf16(a: string, b: string): -1 | 0 | 1 {
  return a < b ? -1 : a > b ? 1 : 0;
}

export function isHighSurrogate(unit: number): boolean {
  return unit >= 0xd800 && unit <= 0xdbff;
}

export function isLowSurrogate(unit: number): boolean {
  return unit >= 0xdc00 && unit <= 0xdfff;
}

export function isSurrogate(unit: number): boolean {
  return unit >= 0xd800 && unit <= 0xdfff;
}

export function isSupplementaryCodePoint(c: number): boolean {
  return c >= 0x10000;
}

/** False for Unicode noncharacters. */
export function isCodePointValid(c: number): boolean {
  const d = c & 0xffff;
  if (d === 0xfffe || d === 0xffff) {
    return false;
  }
  return !(c >= 0xfdd0 && c <= 0xfdef);
}

/**
 * Calls `f` for every code point of `s` until it returns false. An unpaired
 * surrogate is passed as its code unit value (0xD800..=0xDFFF). Returns
 * whether `f` accepted everything.
 */
function allCodePoints(s: string, f: (c: number) => boolean): boolean {
  for (let i = 0; i < s.length; ) {
    // In bounds, so never undefined.
    const c = s.codePointAt(i) ?? 0;
    if (!f(c)) {
      return false;
    }
    i += c >= 0x10000 ? 2 : 1;
  }
  return true;
}

/** Classification of a code point for use in blip text. */
export type BlipCodePointResult =
  | "ok"
  | "control"
  | "deprecated"
  | "bidi"
  | "tag"
  | "noncharacter";

/**
 * Whether a code point is acceptable in blip content (based on RFC 5198).
 * `c` must be a Unicode scalar value, not a surrogate.
 */
export function isCodePointGoodForBlip(c: number): BlipCodePointResult {
  if (!isCodePointValid(c)) {
    return "noncharacter";
  }
  if (c <= 0x1f || (c >= 0x7f && c <= 0x9f)) {
    return "control";
  }
  if (c >= 0x206a && c <= 0x206f) {
    return "deprecated";
  }
  if (c === 0x200e || c === 0x200f || (c >= 0x202a && c <= 0x202e)) {
    return "bidi";
  }
  if (c >= 0xe0000 && c <= 0xe007f) {
    return "tag";
  }
  return "ok";
}

const between = (c: number, lo: number, hi: number): boolean => c >= lo && c <= hi;

/** XML `NameStartChar`, restricted to valid Unicode. `c` is a code point. */
export function isXmlNameStartChar(c: number): boolean {
  return (
    c === 0x3a || // ':'
    between(c, 0x41, 0x5a) ||
    c === 0x5f || // '_'
    between(c, 0x61, 0x7a) ||
    between(c, 0xc0, 0xd6) ||
    between(c, 0xd8, 0xf6) ||
    between(c, 0xf8, 0x2ff) ||
    between(c, 0x370, 0x37d) ||
    between(c, 0x37f, 0x1fff) ||
    between(c, 0x200c, 0x200d) ||
    between(c, 0x2070, 0x218f) ||
    between(c, 0x2c00, 0x2fef) ||
    between(c, 0x3001, 0xd7ff) ||
    between(c, 0xf900, 0xfdcf) ||
    between(c, 0xfdf0, 0xfffd) ||
    (between(c, 0x10000, 0xeffff) && isCodePointValid(c))
  );
}

/** XML `NameChar`, restricted to valid Unicode. `c` is a code point. */
export function isXmlNameChar(c: number): boolean {
  if (!isCodePointValid(c)) {
    return false;
  }
  return (
    isXmlNameStartChar(c) ||
    c === 0x2d || // '-'
    c === 0x2e || // '.'
    between(c, 0x30, 0x39) ||
    c === 0xb7 ||
    between(c, 0x0300, 0x036f) ||
    between(c, 0x203f, 0x2040)
  );
}

/** Whether `s` is an XML `Name`. Unpaired surrogates are never name chars. */
export function isXmlName(s: string): boolean {
  if (s.length === 0) {
    return false;
  }
  let first = true;
  return allCodePoints(s, (c) => {
    const ok = first ? isXmlNameStartChar(c) : isXmlNameChar(c);
    first = false;
    return ok;
  });
}

/** Java `Utf16Util.isValidUtf16`: no noncharacters, no unpaired surrogates. */
export function isValidText(s: string): boolean {
  return allCodePoints(s, (c) => !isSurrogate(c) && isCodePointValid(c));
}

/** Whether every code point of `s` is acceptable blip text. */
export function isGoodForBlip(s: string): boolean {
  return allCodePoints(s, (c) => !isSurrogate(c) && isCodePointGoodForBlip(c) === "ok");
}

/** Maps every code point of `s` through `f` (unpaired surrogates included). */
export function mapCodePoints(s: string, f: (c: number) => string): string {
  let out = "";
  allCodePoints(s, (c) => {
    out += f(c);
    return true;
  });
  return out;
}
