// Tests of the hexadecimal helpers.

import { expect, test } from "vitest";

import { HexError, bytesToHex, bytesToLowerHex, hexToBytes, tryHexToBytes } from "../hex";

test("bytes to hex", () => {
  expect(bytesToHex(new Uint8Array())).toBe("");
  expect(bytesToHex(Uint8Array.of(0, 1, 0xab, 0xff))).toBe("0001ABFF");
  expect(bytesToLowerHex(Uint8Array.of(0, 1, 0xab, 0xff))).toBe("0001abff");
});

test("hex to bytes accepts either case", () => {
  expect(hexToBytes("")).toEqual(new Uint8Array());
  expect(hexToBytes("0001ABFF")).toEqual(Uint8Array.of(0, 1, 0xab, 0xff));
  expect(hexToBytes("0001abFf")).toEqual(Uint8Array.of(0, 1, 0xab, 0xff));
  const all = Uint8Array.from({ length: 256 }, (_, i) => i);
  expect(hexToBytes(bytesToHex(all))).toEqual(all);
  expect(hexToBytes(bytesToLowerHex(all))).toEqual(all);
});

test("malformed hex", () => {
  for (const bad of ["A", "ABC", "zz", "0g", " 0", "0x", "\u0661\u0661", "+1"]) {
    expect(tryHexToBytes(bad), bad).toBeNull();
    expect(() => hexToBytes(bad), bad).toThrow(HexError);
  }
});
