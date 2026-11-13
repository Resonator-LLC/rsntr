// Validity rules for the domain and local parts of wave and wavelet ids
// (`WaveIdentifiers`).
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { isCodePointValid, isSurrogate } from "../docop/utf16";
import { isAsciiAlphanumeric } from "./java";

function isSafeAscii(c: number): boolean {
  return isAsciiAlphanumeric(c) || "-._~+*@".includes(String.fromCharCode(c));
}

/** The `ucschar` production of RFC 3987. */
function isUcsChar(c: number): boolean {
  return (
    (c >= 0xa0 && c <= 0xd7ff) ||
    (c >= 0xf900 && c <= 0xfdcf) ||
    (c >= 0xfdf0 && c <= 0xffef) ||
    (c >= 0x10000 && c <= 0xdfffd && (c & 0xffff) <= 0xfffd) ||
    (c >= 0xe1000 && c <= 0xefffd)
  );
}

/**
 * Whether `id` is a valid local identifier: non-empty, and made only of
 * ASCII letters, digits, `-._~+*@` and non-private UCS characters. Unpaired
 * surrogates are invalid.
 */
export function isValidIdentifier(id: string): boolean {
  if (id.length === 0) {
    return false;
  }
  for (let i = 0; i < id.length; ) {
    const c = id.codePointAt(i) ?? 0;
    if (isSurrogate(c) || !isCodePointValid(c)) {
      return false;
    }
    if (c < 0x7f ? !isSafeAscii(c) : !isUcsChar(c)) {
      return false;
    }
    i += c >= 0x10000 ? 2 : 1;
  }
  return true;
}

/**
 * Whether `x`, from UTF-16 offset `start` on, is a valid domain name:
 * dot-separated labels of lower-case ASCII letters, digits and inner dashes,
 * at most 253 code units in total.
 */
export function isValidDomain(start: number, x: string): boolean {
  const length = x.length - start;
  if (length < 1 || length > 253) {
    return false;
  }
  const alnum = (c: number): boolean => (c >= 0x61 && c <= 0x7a) || (c >= 0x30 && c <= 0x39);
  const DASH = 0x2d;
  const DOT = 0x2e;
  let index = start;
  while (index < x.length) {
    let c = x.charCodeAt(index);
    // A label must begin with a letter or a digit.
    if (!alnum(c)) {
      return false;
    }
    let d = c;
    for (;;) {
      index += 1;
      if (index >= x.length) {
        break;
      }
      c = x.charCodeAt(index);
      // Subsequent characters may be letters, digits or the dash.
      if (!alnum(c) && c !== DASH) {
        break;
      }
      d = c;
    }
    if (index >= x.length) {
      return d !== DASH;
    }
    // Labels must be separated by dots, and may not end with the dash.
    if (c !== DOT || d === DASH) {
      return false;
    }
    index += 1;
  }
  // The domain ended in a dot.
  return false;
}
