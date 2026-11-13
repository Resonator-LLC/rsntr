// Port of `org.waveprotocol.wave.concurrencycontrol.common.ResponseCodeTest`
// of Apache Wave (Apache License, Version 2.0).

import { describe, expect, it } from "vitest";

import { ResponseCode } from "..";

describe("ResponseCodeTest", () => {
  it("testOf", () => {
    for (let i = -1; i < ResponseCode.values().length - 1; i++) {
      expect(ResponseCode.of(i), String(i)).toBe(ResponseCode.values()[i + 1]);
      expect(ResponseCode.of(i).getValue(), String(i)).toBe(i);
      expect(ResponseCode.of(i).value, String(i)).toBe(i);
    }
  });

  it("testOfIndexOutOfBounds", () => {
    expect(() => ResponseCode.of(-2)).toThrow(RangeError);
    expect(() => ResponseCode.of(ResponseCode.values().length + 1)).toThrow(RangeError);
  });
});
