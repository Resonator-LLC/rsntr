// Conversion of values to and from attribute strings (`Serializer`).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.util.Serializer` and
// `org.waveprotocol.wave.model.supplement` (`WaveletIdSerializer`,
// `HashedVersionSerializer`) of Apache Wave, licensed under the Apache
// License, Version 2.0.
//
// Rust attaches the conversion to the value type (`Serializable`); here, as
// in the original, there is one serializer object per type, which also
// carries the equality and order the data types need.

import { ModernIdSerialiser, WaveletId } from "../id";
import { HashedVersion } from "../version";
import { AdtError } from "./error";

/** Converts values of one type to and from attribute strings. */
export interface Serializer<T> {
  /**
   * The attribute form of the value (`Serializer.toString`). Throws
   * {@link AdtError} `unserialisable`.
   */
  toAttribute(value: T): string;

  /**
   * The value of an attribute, or `null` if the string is not a value of
   * this type (`Serializer.fromString`, which throws in that case).
   */
  fromAttribute(attribute: string): T | null;

  equals(a: T, b: T): boolean;

  /** Negative, zero or positive: the order of sorted listings. */
  compare(a: T, b: T): number;
}

const nativeOrder = <T>(a: T, b: T): number => (a < b ? -1 : a > b ? 1 : 0);

const INT_PATTERN = /^[+-]?[0-9]+$/;

/** `Integer.parseInt` (ASCII digits only); `null` where it throws. */
export function parseJavaInt(text: string): number | null {
  if (!INT_PATTERN.test(text)) {
    return null;
  }
  const value = Number(text);
  // `|| 0` turns "-0" into zero.
  return value >= -2147483648 && value <= 2147483647 ? value || 0 : null;
}

/** Whether `Long.parseLong` accepts a string (ASCII digits only). */
export function isJavaLong(text: string): boolean {
  if (!INT_PATTERN.test(text)) {
    return false;
  }
  const value = BigInt(text.startsWith("+") ? text.slice(1) : text);
  return value >= -(2n ** 63n) && value < 2n ** 63n;
}

/**
 * `Long.parseLong`, for values that are safe integers; `null` where it
 * throws and for longs a `number` cannot hold exactly.
 */
export function parseJavaLong(text: string): number | null {
  if (!INT_PATTERN.test(text)) {
    return null;
  }
  const value = Number(text);
  return Number.isSafeInteger(value) ? value || 0 : null;
}

/** `Serializer.STRING`. */
export const STRING: Serializer<string> = {
  toAttribute: (value) => value,
  fromAttribute: (attribute) => attribute,
  equals: (a, b) => a === b,
  compare: nativeOrder,
};

/** `Serializer.INTEGER`: a 32-bit signed integer. */
export const INTEGER: Serializer<number> = {
  toAttribute: (value) => String(value),
  fromAttribute: parseJavaInt,
  equals: (a, b) => a === b,
  compare: (a, b) => a - b,
};

/**
 * `Serializer.LONG`, restricted to safe integers: a long beyond 2^53 is not
 * a value.
 */
export const LONG: Serializer<number> = {
  toAttribute: (value) => String(value),
  fromAttribute: parseJavaLong,
  equals: (a, b) => a === b,
  compare: nativeOrder,
};

/** `Serializer.BOOLEAN`: anything but "true" in any letter case is false. */
export const BOOLEAN: Serializer<boolean> = {
  toAttribute: (value) => String(value),
  fromAttribute: (attribute) => /^[tT][rR][uU][eE]$/.test(attribute),
  equals: (a, b) => a === b,
  compare: (a, b) => Number(a) - Number(b),
};

/**
 * `Serializer.DOUBLE`, with the string forms of `Double.toString` and
 * `Double.parseDouble`.
 */
export const DOUBLE: Serializer<number> = {
  toAttribute: javaDoubleToString,
  fromAttribute: parseJavaDouble,
  equals: (a, b) => a === b,
  compare: nativeOrder,
};

/** `WaveletIdSerializer`: the modern form, `domain/id`. */
export const WAVELET_ID: Serializer<WaveletId> = {
  toAttribute(value) {
    try {
      return ModernIdSerialiser.INSTANCE.serialiseWaveletId(value);
    } catch (e) {
      throw AdtError.unserialisable(e instanceof Error ? e.message : String(e));
    }
  },
  fromAttribute(attribute) {
    try {
      return ModernIdSerialiser.INSTANCE.deserialiseWaveletId(attribute);
    } catch {
      return null;
    }
  },
  equals: (a, b) => a.equals(b),
  compare: (a, b) => a.compareTo(b),
};

/**
 * `HashedVersionSerializer`: `<version>:<base64 hash>`; when reading, the
 * colon and hash may be absent. Unlike the original, a negative version is
 * not a value.
 */
export const HASHED_VERSION: Serializer<HashedVersion> = {
  toAttribute: (value) => value.toString(),
  fromAttribute(attribute) {
    try {
      return HashedVersion.fromString(attribute);
    } catch {
      return null;
    }
  },
  equals: (a, b) => a.equals(b),
  compare: (a, b) => a.compareTo(b),
};

/**
 * A serializer of the constants of a string enumeration, stored by name
 * (`Serializer.EnumSerializer`). The constants are ordered as listed.
 */
export function enumSerializer<T extends string>(names: readonly T[]): Serializer<T> {
  return {
    toAttribute: (value) => value,
    fromAttribute: (attribute) => names.find((name) => name === attribute) ?? null,
    equals: (a, b) => a === b,
    compare: (a, b) => names.indexOf(a) - names.indexOf(b),
  };
}

/**
 * `Double.toString` (as specified since JDK 19): plain decimal notation
 * for magnitudes in `[1e-3, 1e7)`, otherwise "computerized scientific
 * notation" such as `1.0E10`, always with at least one digit after the
 * point, and with as few digits as identify the value.
 */
export function javaDoubleToString(value: number): string {
  if (Number.isNaN(value)) {
    return "NaN";
  }
  if (!Number.isFinite(value)) {
    return value > 0 ? "Infinity" : "-Infinity";
  }
  const sign = value < 0 || Object.is(value, -0) ? "-" : "";
  const magnitude = Math.abs(value);
  if (magnitude === 0) {
    return `${sign}0.0`;
  }
  // Shortest digits and decimal exponent: d.ddd * 10^exponent. Where one
  // digit would do, Java takes the two-digit decimal closest to the value
  // (which matters for the smallest subnormal numbers: 4.9E-324).
  let scientific = magnitude.toExponential();
  if (!scientific.includes(".")) {
    scientific = magnitude.toExponential(1);
  }
  const at = scientific.indexOf("e");
  const mantissa = scientific.slice(0, at);
  const exponent = Number(scientific.slice(at + 1));
  const digits = mantissa.replace(".", "").replace(/0+$/, "") || "0";
  if (exponent >= -3 && exponent < 7) {
    if (exponent < 0) {
      return `${sign}0.${"0".repeat(-exponent - 1)}${digits}`;
    }
    const point = exponent + 1;
    if (digits.length <= point) {
      return `${sign}${digits}${"0".repeat(point - digits.length)}.0`;
    }
    return `${sign}${digits.slice(0, point)}.${digits.slice(point)}`;
  }
  const fraction = digits.length > 1 ? digits.slice(1) : "0";
  return `${sign}${digits.charAt(0)}.${fraction}E${exponent}`;
}

const DECIMAL_FLOAT = /^(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$/;
const HEX_FLOAT = /^([0-9a-fA-F]*)(?:\.([0-9a-fA-F]*))?[pP]([+-]?[0-9]+)$/;

/**
 * `Double.parseDouble`: surrounding whitespace is ignored; an optional
 * sign is followed by `NaN`, `Infinity`, a decimal number with optional
 * exponent, or a hexadecimal floating-point number, and an optional type
 * suffix `f`, `F`, `d` or `D`. `null` where the original throws.
 */
export function parseJavaDouble(text: string): number | null {
  // String.trim removes everything up to U+0020 from both ends.
  // eslint-disable-next-line no-control-regex
  const trimmed = text.replace(/^[\u0000- ]+/, "").replace(/[\u0000- ]+$/, "");
  if (trimmed.length === 0) {
    return null;
  }
  const first = trimmed.charAt(0);
  const negative = first === "-";
  let body = first === "-" || first === "+" ? trimmed.slice(1) : trimmed;
  let magnitude: number | null;
  if (body === "NaN") {
    magnitude = Number.NaN;
  } else if (body === "Infinity") {
    magnitude = Number.POSITIVE_INFINITY;
  } else {
    if (/[fFdD]$/.test(body)) {
      body = body.slice(0, -1);
    }
    magnitude =
      body.startsWith("0x") || body.startsWith("0X")
        ? parseHexFloat(body.slice(2))
        : DECIMAL_FLOAT.test(body)
          ? Number(body)
          : null;
  }
  if (magnitude === null) {
    return null;
  }
  return negative ? -magnitude : magnitude;
}

/**
 * A hexadecimal floating-point number without its `0x`: hex digits with an
 * optional point, then a mandatory binary exponent `p<decimal>`.
 */
function parseHexFloat(body: string): number | null {
  const match = HEX_FLOAT.exec(body);
  if (match === null) {
    return null;
  }
  const whole = match[1] ?? "";
  const fraction = match[2] ?? "";
  if (whole.length === 0 && fraction.length === 0) {
    return null;
  }
  // Not correctly rounded for mantissas of more than 53 significant bits,
  // which is of no consequence for validation.
  let value = 0;
  for (const digit of whole + fraction) {
    value = value * 16 + Number.parseInt(digit, 16);
  }
  const exponent = Number(match[3]) - 4 * fraction.length;
  return value * 2 ** Math.max(-5000, Math.min(5000, exponent));
}
