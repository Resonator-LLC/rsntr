// Tests of the UTF-16 helpers (the unit tests at the end of `utf16.rs`).

import { expect, test } from "vitest";

import {
  cmpUtf16,
  isValidText,
  isXmlName,
  saturatingAdd,
  splitAtUtf16,
  utf16Len,
} from "../utf16";
import { expectOperationError } from "./helpers";

test("lengths are utf16", () => {
  expect(utf16Len("")).toBe(0);
  expect(utf16Len("abc")).toBe(3);
  expect(utf16Len("é")).toBe(1);
  expect(utf16Len("a😀b")).toBe(4);
});

test("split on boundaries", () => {
  expect(splitAtUtf16("abc", 1)).toEqual(["a", "bc"]);
  expect(splitAtUtf16("abc", 0)).toEqual(["", "abc"]);
  expect(splitAtUtf16("abc", 3)).toEqual(["abc", ""]);
  expect(splitAtUtf16("a😀b", 1)).toEqual(["a", "😀b"]);
  expect(splitAtUtf16("a😀b", 3)).toEqual(["a😀", "b"]);
  expect(splitAtUtf16("é😀", 3)).toEqual(["é😀", ""]);
});

test("split inside surrogate pair is an error", () => {
  const e = expectOperationError(() => splitAtUtf16("a😀b", 2), "surrogateSplit");
  expect(e.detail).toEqual({ kind: "surrogateSplit", offset: 2 });
});

test("split out of bounds is an error", () => {
  expect(expectOperationError(() => splitAtUtf16("abc", 4)).detail).toEqual({
    kind: "offsetOutOfBounds",
    offset: 4,
    len: 3,
  });
  expect(expectOperationError(() => splitAtUtf16("a😀", 5)).detail).toEqual({
    kind: "offsetOutOfBounds",
    offset: 5,
    len: 3,
  });
});

// TypeScript only: offsets a Rust `usize` cannot hold.
test("split at a negative or fractional offset is an error", () => {
  expectOperationError(() => splitAtUtf16("abc", -1), "offsetOutOfBounds");
  expectOperationError(() => splitAtUtf16("abc", 1.5), "offsetOutOfBounds");
  expectOperationError(() => splitAtUtf16("abc", Number.NaN), "offsetOutOfBounds");
});

test("ordering matches java compareTo", () => {
  expect(cmpUtf16("a", "b")).toBe(-1);
  expect(cmpUtf16("ab", "a")).toBe(1);
  expect(cmpUtf16("a", "a")).toBe(0);
  // U+FF5E sorts after U+1F600 in UTF-16 (0xFF5E > 0xD83D) but before it in
  // UTF-8 / code point order.
  const bmp = "～";
  const astral = "\u{1F600}";
  expect(bmp.codePointAt(0)!).toBeLessThan(astral.codePointAt(0)!);
  expect(cmpUtf16(bmp, astral)).toBe(1);
  // The default array sort uses the same order.
  expect([bmp, astral].sort()).toEqual([astral, bmp]);
});

test("xml names", () => {
  expect(isXmlName("body")).toBe(true);
  expect(isXmlName("_t")).toBe(true);
  expect(isXmlName("a-b.c9")).toBe(true);
  expect(isXmlName("")).toBe(false);
  expect(isXmlName("<")).toBe(false);
  expect(isXmlName("9a")).toBe(false);
  expect(isXmlName(".!$*.4!(,")).toBe(false);
});

test("noncharacters are invalid", () => {
  expect(isValidText("a😀")).toBe(true);
  expect(isValidText("￾")).toBe(false);
  expect(isValidText("﷐")).toBe(false);
  expect(isValidText("\u{1FFFF}")).toBe(false);
});

// TypeScript only: a Rust string cannot hold unpaired surrogates.
test("unpaired surrogates are invalid", () => {
  expect(isValidText("\uD83D")).toBe(false);
  expect(isValidText("a\uDE00")).toBe(false);
  expect(isValidText("\uDE00\uD83D")).toBe(false);
  expect(isXmlName("a\uD83D")).toBe(false);
  expect(isXmlName("\uDE00")).toBe(false);
});

test("saturating addition stays within safe integers", () => {
  expect(saturatingAdd(1, 2)).toBe(3);
  expect(saturatingAdd(Number.MAX_SAFE_INTEGER, 5)).toBe(Number.MAX_SAFE_INTEGER);
  expect(saturatingAdd(Number.MAX_VALUE, Number.MAX_VALUE)).toBe(Number.MAX_SAFE_INTEGER);
});
