// Differential test against the original Java implementation: the version
// records of `wave_golden.txt`. Port of `version_cases` of the Rust
// `golden_tests.rs` (tags HVS, HVC), plus the history hashes of the
// scenarios: SCN (version zero), and V, the version after each applied delta
// whose bytes are given by the preceding A record. The remaining scenario
// and transform records (D, T, U, W, WD, S, F, XS, X, DP) need the
// operation, data and protobuf modules; they are parsed and skipped here.

import { expect, test } from "vitest";

import { WaveletName } from "../../id";
import { Golden, RECORD_COUNT, hexDecode, hexEncode } from "../../id/tests/waveGolden";
import { HashedVersion, HashedVersionFactory, HashedVersionParseError } from "..";

function versionCases(golden: Golden): number {
  const versions: HashedVersion[] = [];
  let unsafe = 0;
  for (let r = golden.take("HVS"); r !== null; r = golden.take("HVS")) {
    const [number, hash, text] = r.fields as [string, string, string];
    const version = HashedVersion.of(Number(number), hexDecode(hash));
    versions.push(version);
    if (!Number.isSafeInteger(version.version)) {
      // Long.MAX_VALUE: versions here are safe integers, so the text form
      // cannot be reproduced. The (rounded) value still takes part in the
      // comparisons below.
      expect(() => HashedVersion.fromString(text)).toThrow(HashedVersionParseError);
      unsafe += 1;
      continue;
    }
    expect(version.toString(), `golden line ${r.line}`).toBe(text);
    expect(HashedVersion.fromString(text).equals(version), `golden line ${r.line}`).toBe(true);
  }
  expect(versions.length).toBeGreaterThan(10);
  const n = golden.checkAll("HVC", 2, (f) => {
    const a = versions[Number(f[0])] as HashedVersion;
    const b = versions[Number(f[1])] as HashedVersion;
    expect(a.equals(b)).toBe(a.compareTo(b) === 0);
    return [String(a.compareTo(b))];
  });
  expect(n).toBeGreaterThan(100);
  return unsafe;
}

const NAMES: readonly (readonly [string, string, string, string])[] = [
  ["example.com", "w+abc", "example.com", "conv+root"],
  ["example.com", "w+abc", "other.org", "user+a@other.org"],
  ["a.b.c", "w+caf\u{E9}", "a.b.c", "conv+\u{4E2D}\u{6587}"],
  ["example.com", "prof+j\u{F6}rg@t\u{16B}dali\u{146}.lv", "example.com", "conv+x*y"],
  ["x.org", "w+x\u{10000}y", "y.org", "conv+~~a"],
];

const SKIPPED = new Set(["D", "T", "U", "W", "WD", "S", "F", "XS", "X", "DP"]);

/** Checks the hash chain of the scenarios and skips everything else. */
function historyHashes(golden: Golden): Map<string, number> {
  const counts = new Map<string, number>();
  let v: HashedVersion | null = null;
  let applied: Uint8Array | null = null;
  while (golden.next < golden.records.length) {
    const record = golden.records[golden.next] as readonly string[];
    const line = ++golden.next;
    const tag = record[0] as string;
    counts.set(tag, (counts.get(tag) ?? 0) + 1);
    if (tag === "SCN") {
      const seed = Number(record[1]);
      const n = NAMES[seed % NAMES.length] as readonly [string, string, string, string];
      v = HashedVersionFactory.createVersionZero(WaveletName.ofParts(n[0], n[1], n[2], n[3]));
      expect(v.version).toBe(0);
      expect(hexEncode(v.historyHash), `golden line ${line} (SCN)`).toBe(record[2]);
      applied = null;
    } else if (tag === "A") {
      applied = hexDecode(record[1] as string);
    } else if (tag === "V") {
      if (v === null || applied === null) {
        throw new Error(`golden line ${line}: V without SCN and A`);
      }
      const version = Number(record[1]);
      const v2: HashedVersion = HashedVersionFactory.create(applied, v, version - v.version);
      expect(v2.version).toBe(version);
      expect(hexEncode(v2.historyHash), `golden line ${line} (V)`).toBe(record[2]);
      expect(v2.historyHash.length).toBe(20);
      v = v2;
      applied = null;
    } else if (!SKIPPED.has(tag)) {
      throw new Error(`golden line ${line}: unexpected record ${tag}`);
    }
  }
  return counts;
}

test("matches original implementation (version records)", () => {
  const golden = Golden.load();
  expect(golden.records.length).toBe(RECORD_COUNT);
  // The id records come first; they are covered by the id tests.
  golden.next = golden.records.findIndex((r) => r[0] === "HVS");
  expect(golden.next).toBeGreaterThan(0);
  // HVS records with version Long.MAX_VALUE, checked for ordering only.
  expect(versionCases(golden)).toBe(11);
  const counts = historyHashes(golden);
  expect(counts.get("SCN")).toBe(12);
  expect(counts.get("V")).toBe(72);
  expect(counts.get("A")).toBe(72);
  expect(golden.next).toBe(RECORD_COUNT);
});
