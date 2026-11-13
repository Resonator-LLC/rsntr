// Hexadecimal encoding of byte strings, as used for `bytes` fields in the
// JSON form of the protocol messages.
//
// Follows `org.waveprotocol.wave.communication.Codec` of Apache Wave,
// licensed under the Apache License, Version 2.0: upper-case digits when
// writing; either case is accepted when reading.

/** A string that is not an even number of hexadecimal digits. */
export class HexError extends Error {
  readonly input: string;

  constructor(input: string) {
    super(
      input.length % 2 !== 0
        ? `odd number of hexadecimal digits (${input.length})`
        : "invalid hexadecimal digit",
    );
    this.name = "HexError";
    this.input = input;
  }
}

const UPPER: readonly string[] = Array.from({ length: 256 }, (_, i) =>
  i.toString(16).toUpperCase().padStart(2, "0"),
);

/** The value of a hexadecimal digit's code unit, or -1. */
function digit(code: number): number {
  if (code >= 0x30 && code <= 0x39) {
    return code - 0x30;
  }
  if (code >= 0x41 && code <= 0x46) {
    return code - 0x41 + 10;
  }
  if (code >= 0x61 && code <= 0x66) {
    return code - 0x61 + 10;
  }
  return -1;
}

/** The bytes as upper-case hexadecimal digits, two per byte. */
export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += UPPER[bytes[i]!]!;
  }
  return out;
}

/** The bytes as lower-case hexadecimal digits, two per byte. */
export function bytesToLowerHex(bytes: Uint8Array): string {
  return bytesToHex(bytes).toLowerCase();
}

/** The bytes for a string of hexadecimal digits of either case, or null. */
export function tryHexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0) {
    return null;
  }
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    const high = digit(hex.charCodeAt(2 * i));
    const low = digit(hex.charCodeAt(2 * i + 1));
    if (high < 0 || low < 0) {
      return null;
    }
    bytes[i] = (high << 4) | low;
  }
  return bytes;
}

/**
 * The bytes for a string of hexadecimal digits of either case. Throws
 * {@link HexError} on an odd length or a character that is not a digit.
 */
export function hexToBytes(hex: string): Uint8Array {
  const bytes = tryHexToBytes(hex);
  if (bytes === null) {
    throw new HexError(hex);
  }
  return bytes;
}
