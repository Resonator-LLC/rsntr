// Ports of the Java unit tests `HashedVersionTest` and
// `HashedVersionSerializerTest` (`org.waveprotocol.wave.model.version`), via
// the Rust `version/tests.rs`, plus checks of the SHA-256 implementation.
//
// The serializer of the original maps to `toString` and
// `HashedVersion.fromString`.

import { describe, expect, test } from "vitest";

import { WaveletName } from "../../id";
import { HashedVersion, HashedVersionFactory, HashedVersionParseError, sha256 } from "..";

const ascii = (s: string): Uint8Array => new TextEncoder().encode(s);
const hex = (bytes: Uint8Array): string =>
  [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");

describe("HashedVersionTest", () => {
  const AAA = ascii("AAA");
  const BBB = ascii("BBB");

  function assertCompare(
    v1: number,
    h1: Uint8Array,
    v2: number,
    h2: Uint8Array,
    cmp: -1 | 0 | 1,
  ): void {
    expect(HashedVersion.of(v1, h1).compareTo(HashedVersion.of(v2, h2))).toBe(cmp);
    expect(HashedVersion.of(v2, h2).compareTo(HashedVersion.of(v1, h1))).toBe(-cmp + 0);
  }

  test("equality", () => {
    expect(HashedVersion.of(10, AAA).equals(HashedVersion.of(10, AAA))).toBe(true);
    expect(HashedVersion.of(10, AAA).equals(HashedVersion.of(10, BBB))).toBe(false);
    expect(HashedVersion.of(10, AAA).equals(HashedVersion.of(12, AAA))).toBe(false);
    expect(HashedVersion.of(10, AAA).equals(HashedVersion.of(12, BBB))).toBe(false);
  });

  test("comparisons", () => {
    assertCompare(10, AAA, 10, AAA, 0);
    // Different versions.
    assertCompare(10, AAA, 12, AAA, -1);
    // Different hashes.
    assertCompare(10, AAA, 10, BBB, -1);
    // Both different, version takes precedence.
    assertCompare(10, BBB, 12, AAA, -1);
  });

  test("unsigned", () => {
    expect(HashedVersion.unsigned(10).version).toBe(10);
    expect(HashedVersion.unsigned(10).historyHash.length).toBe(0);
  });

  test("unsigned comparisons", () => {
    expect(HashedVersion.unsigned(10).compareTo(HashedVersion.unsigned(10))).toBe(0);
    // unsigned(10) is smaller than every other hashed version with version
    // number 10
    expect(HashedVersion.unsigned(10).compareTo(HashedVersion.of(10, AAA))).toBe(-1);
    // and bigger than hashed versions with smaller version number
    expect(HashedVersion.unsigned(10).compareTo(HashedVersion.of(9, AAA))).toBe(1);
  });

  test("hash bytes compare as signed values and the hash is copied", () => {
    assertCompare(1, Uint8Array.of(0x80), 1, Uint8Array.of(0x7f), -1);
    assertCompare(1, Uint8Array.of(1), 1, Uint8Array.of(1, 0x80), -1);
    const bytes = Uint8Array.of(1, 2, 3);
    const version = HashedVersion.of(1, bytes);
    bytes[0] = 9;
    expect([...version.historyHash]).toEqual([1, 2, 3]);
  });
});

describe("HashedVersionSerializerTest", () => {
  // The signed bytes -127, -45, 0, 45, 110, 127.
  const typical = HashedVersion.of(5678, Uint8Array.of(129, 211, 0, 45, 110, 127));
  const unsigned = HashedVersion.unsigned(5678);

  test("serialize deserialize", () => {
    expect(typical.toString()).toBe("5678:gdMALW5/");
    expect(unsigned.toString()).toBe("5678:");

    expect(HashedVersion.fromString(typical.toString()).equals(typical)).toBe(true);
    expect(HashedVersion.fromString(unsigned.toString()).equals(unsigned)).toBe(true);
  });

  test("padding and lenient parsing", () => {
    for (let length = 0; length < 8; length++) {
      const version = HashedVersion.of(3, Uint8Array.from({ length }, (_, i) => 250 + i));
      const text = version.toString();
      expect(text.length % 4).toBe("3:".length % 4);
      expect(HashedVersion.fromString(text).equals(version)).toBe(true);
      // Padding is optional.
      expect(HashedVersion.fromString(text.replaceAll("=", "")).equals(version)).toBe(true);
    }
    expect(HashedVersion.fromString("7").equals(HashedVersion.unsigned(7))).toBe(true);
    expect(() => HashedVersion.fromString("x:")).toThrow(HashedVersionParseError);
    expect(() => HashedVersion.fromString("-1:")).toThrow(HashedVersionParseError);
    expect(() => HashedVersion.fromString("")).toThrow(HashedVersionParseError);
    expect(() => HashedVersion.fromString("1:*")).toThrow(HashedVersionParseError);
    expect(() => HashedVersion.fromString("1:A")).toThrow(HashedVersionParseError);
  });
});

describe("HashedVersionFactory", () => {
  test("version zero is the wavelet name URI", () => {
    const same = WaveletName.ofParts("example.com", "w+abc", "example.com", "conv+root");
    expect(new TextDecoder().decode(HashedVersionFactory.createVersionZero(same).historyHash)).toBe(
      "wave://example.com/w+abc/conv+root",
    );
    const differ = WaveletName.ofParts("example.com", "w+abc", "other.org", "conv+root");
    expect(
      new TextDecoder().decode(HashedVersionFactory.createVersionZero(differ).historyHash),
    ).toBe("wave://other.org/example.com!w+abc/conv+root");
  });

  test("create chains the hash", () => {
    const zero = HashedVersion.of(0, ascii("ab"));
    const next = HashedVersionFactory.create(ascii("c"), zero, 3);
    expect(next.version).toBe(3);
    expect(hex(next.historyHash)).toBe("ba7816bf8f01cfea414140de5dae2223b00361a3");
    const full = HashedVersionFactory.createWithHashSize(ascii("c"), zero, 3, 4096);
    expect(hex(full.historyHash)).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });
});

describe("sha256", () => {
  test("known digests", () => {
    expect(hex(sha256())).toBe("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    expect(hex(sha256(ascii("abc")))).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
    expect(hex(sha256(ascii("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")))).toBe(
      "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
    );
  });

  test("agrees with the platform for every padding length", async () => {
    const data = Uint8Array.from({ length: 300 }, (_, i) => (i * 131 + 7) & 0xff);
    for (let length = 0; length <= data.length; length++) {
      const message = data.subarray(0, length);
      const expected = new Uint8Array(await crypto.subtle.digest("SHA-256", message.slice()));
      expect(hex(sha256(message)), `length ${length}`).toBe(hex(expected));
    }
  });
});
