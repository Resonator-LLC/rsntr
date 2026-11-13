// Small emulations of `java.lang.String` and `java.net` behaviour that the
// original code relies on. Internal to the id module.
//
// Derived from the behaviour Apache Wave (Apache License 2.0) expects of the
// Java runtime.

/**
 * `String.split` with a single literal separator: empty tokens are kept
 * except trailing ones, and a string without the separator (including the
 * empty string) yields itself.
 */
export function split(s: string, separator: string): string[] {
  if (!s.includes(separator)) {
    return [s];
  }
  const parts = s.split(separator);
  while (parts.length > 0 && parts[parts.length - 1] === "") {
    parts.pop();
  }
  return parts;
}

const UPPER_HEX = "0123456789ABCDEF";

export function percentEncoded(byte: number): string {
  return "%" + UPPER_HEX.charAt(byte >> 4) + UPPER_HEX.charAt(byte & 0xf);
}

const UTF8_ENCODER = new TextEncoder();

/** The UTF-8 encoding of `s`. */
export function utf8Bytes(s: string): Uint8Array {
  return UTF8_ENCODER.encode(s);
}

/** Decodes UTF-8; `null` if the bytes are not valid UTF-8. */
export function utf8DecodeStrict(bytes: Uint8Array): string | null {
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** Decodes UTF-8, replacing invalid sequences with U+FFFD. */
export function utf8DecodeLossy(bytes: Uint8Array): string {
  return new TextDecoder("utf-8", { ignoreBOM: true }).decode(bytes);
}

export function isAsciiAlphanumeric(c: number): boolean {
  return (c >= 0x30 && c <= 0x39) || (c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a);
}

/**
 * Percent-encodes the UTF-8 bytes of `s`, keeping ASCII alphanumerics and
 * the ASCII characters of `safe`. With `spaceAsPlus` a space becomes `+`.
 */
export function percentEscape(s: string, safe: string, spaceAsPlus = false): string {
  let out = "";
  for (const b of utf8Bytes(s)) {
    if (isAsciiAlphanumeric(b) || (b < 0x80 && safe.includes(String.fromCharCode(b)))) {
      out += String.fromCharCode(b);
    } else if (spaceAsPlus && b === 0x20) {
      out += "+";
    } else {
      out += percentEncoded(b);
    }
  }
  return out;
}

/**
 * `URLEncoder.encode(s, "UTF-8")`: alphanumerics and `.-*_` are kept, a
 * space becomes `+`, everything else is percent-encoded as UTF-8.
 */
export function urlEncode(s: string): string {
  return percentEscape(s, ".-*_", true);
}

function hexDigit(c: number | undefined): number {
  if (c === undefined) {
    return -1;
  }
  if (c >= 0x30 && c <= 0x39) {
    return c - 0x30;
  }
  const lower = c | 0x20;
  return lower >= 0x61 && lower <= 0x66 ? lower - 0x61 + 10 : -1;
}

/**
 * The byte-level part of `URLDecoder.decode`: `+` becomes a space and `%XX`
 * becomes a byte. Returns `null` on an incomplete or non-hexadecimal escape,
 * where the original throws `IllegalArgumentException`.
 */
export function urlDecodeBytes(s: string): Uint8Array | null {
  const bytes = utf8Bytes(s);
  const out: number[] = [];
  let i = 0;
  while (i < bytes.length) {
    const b = bytes[i] as number;
    if (b === 0x2b) {
      out.push(0x20);
      i += 1;
    } else if (b === 0x25) {
      const hi = hexDigit(bytes[i + 1]);
      const lo = hexDigit(bytes[i + 2]);
      if (hi < 0 || lo < 0) {
        return null;
      }
      out.push(hi * 16 + lo);
      i += 3;
    } else {
      out.push(b);
      i += 1;
    }
  }
  return Uint8Array.from(out);
}
