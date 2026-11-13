// Pairing codes as people type them: the same reading as the node's
// (`crates/resonator-federation/src/pair_code.rs`).

import { describe, expect, test } from "vitest";

import {
  BIP39_ENGLISH,
} from "../bip39English";
import {
  codeGroups,
  codeHint,
  completeCodeWord,
  findCodeWord,
  looksLikePairLink,
  parsePairCode,
  suggestCodeWords,
} from "../pairCode";

describe("pairing codes", () => {
  test("the word list is BIP39 English, first four letters unique", () => {
    expect(BIP39_ENGLISH).toHaveLength(2048);
    expect(BIP39_ENGLISH[0]).toBe("abandon");
    expect(BIP39_ENGLISH[2047]).toBe("zoo");
    expect(new Set(BIP39_ENGLISH.map((w) => w.slice(0, 4))).size).toBe(2048);
  });

  test("codes are read as people type them", () => {
    for (const typed of [
      "482913-ocean-tiger",
      "482913-OCEAN-Tiger",
      "  482913 ocean tiger\n",
      "482913 - ocean - tiger",
      "482913--ocean  tiger",
      "482913-ocea-tige",
    ]) {
      const reading = parsePairCode(typed);
      expect(reading.ok ? reading.text : reading.error, typed).toBe("482913-ocean-tiger");
    }
    const zeros = parsePairCode("000042 abandon zoo");
    expect(zeros.ok && zeros.code).toEqual({ nameplate: "000042", words: ["abandon", "zoo"] });
  });

  test("malformed codes and unknown words are refused", () => {
    for (const bad of ["", "482913", "482913-ocean", "482913-ocean-tiger-apple", "48291-ocean-tiger", "48291x-ocean-tiger", "ocean-tiger-482913"]) {
      const reading = parsePairCode(bad);
      expect(reading.ok, bad).toBe(false);
      expect(!reading.ok && reading.error).toContain("6 digits and 2 words");
    }
    const word = parsePairCode("482913-ocean-tigr");
    expect(!word.ok && word.error).toBe('"tigr" is not one of the code words.');
    expect(findCodeWord("oce")).toBeNull();
    expect(findCodeWord("ocea")).toBe("ocean");
    expect(findCodeWord("ACT")).toBe("act");
    expect(looksLikePairLink(" resonator:pair/abc")).toBe(true);
    expect(looksLikePairLink("482913-ocean-tiger")).toBe(false);
  });

  test("hints speak only of what was finished", () => {
    expect(codeHint("")).toBeNull();
    expect(codeHint("4829")).toBeNull();
    expect(codeHint("4829 ")).toBe("The number has 6 digits.");
    expect(codeHint("ocean ")).toBe("A pairing code starts with its 6 digits.");
    expect(codeHint("482913-tig")).toBeNull();
    expect(codeHint("482913-tigr-")).toBe('"tigr" is not one of the code words.');
    expect(codeHint("482913-ocean-tiger-apple")).toContain("6 digits and 2 words");
    expect(codeHint("resonator:pair/abc")).toBeNull();
  });

  test("words are suggested and completed", () => {
    expect(suggestCodeWords("482913-oce")).toEqual(["ocean"]);
    expect(suggestCodeWords("482913-ocean-ti")).toEqual(["ticket", "tide", "tiger", "tilt"]);
    expect(suggestCodeWords("482913-ocean")).toEqual([]);
    expect(suggestCodeWords("482913-ocean ")).toEqual([]);
    expect(suggestCodeWords("4829")).toEqual([]);
    expect(completeCodeWord("482913 oce", "ocean")).toBe("482913-ocean-");
    expect(completeCodeWord("482913-ocean-ti", "tiger")).toBe("482913-ocean-tiger");
    expect(codeGroups("482913-ocean-tiger")).toEqual(["482913", "ocean", "tiger"]);
  });
});
