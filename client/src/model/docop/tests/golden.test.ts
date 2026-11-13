// Differential test against the original Java implementation.
//
// `docop_golden.txt` (in the Rust crate's testdata) was produced by
// `GoldenGenerator.java` running the Apache Wave code. The random operation
// generator here consumes random numbers in the same order as the original,
// so replaying a scenario with the same seed must reproduce the same
// operations, and compose, transform, invert and document application must
// give the same results.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

import {
  BootstrapDocument,
  type DocOp,
  DocOpCollector,
  compose,
  invert,
  toConciseString,
  transform,
} from "..";
import { RandomProviderImpl, defaultParameters, generateRandomOp } from "../testing";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../../../crates/resonator-model/testdata/docop_golden.txt", import.meta.url),
);

/** Rust's `str::lines`. */
function lines(text: string): string[] {
  const result = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  if (result[result.length - 1] === "") {
    result.pop();
  }
  return result;
}

class Golden {
  private next = 0;
  checked = 0;

  constructor(readonly lines: readonly string[]) {}

  /** Checks that the next record has the given tag and payload. */
  expect(tag: string, actual: string): void {
    const number = this.next++;
    const line = this.lines[number];
    if (line === undefined) {
      throw new Error("golden file ended early");
    }
    const space = line.indexOf(" ");
    const lineTag = space < 0 ? line : line.slice(0, space);
    const payload = space < 0 ? "" : line.slice(space + 1);
    if (lineTag !== tag || actual !== payload) {
      // Only build the (large) assertion message on failure.
      expect(`${tag} ${actual}`, `golden line ${number + 1}`).toBe(`${lineTag} ${payload}`);
    }
    this.checked += 1;
  }

  expectOp(tag: string, op: DocOp): void {
    this.expect(tag, toConciseString(op));
  }

  atEnd(): boolean {
    return this.next === this.lines.length;
  }
}

test("matches original implementation", () => {
  const all = lines(readFileSync(GOLDEN_PATH, "utf8"));
  const golden = new Golden(all);
  const p = defaultParameters();
  const scenarios = all.filter((l) => l.startsWith("SEED ")).length;
  const groups = all.filter((l) => l.startsWith("C ")).length;
  expect(scenarios).toBe(30);
  expect(groups).toBe(30 * 6);
  for (let seed = 0; seed < scenarios; seed++) {
    golden.expect("SEED", String(seed));
    const r = RandomProviderImpl.ofSeed(seed);
    const doc = new BootstrapDocument();
    const collector = new DocOpCollector();
    for (let i = 0; i < 3; i++) {
      const op = generateRandomOp(r, p, doc);
      golden.expectOp("G", op);
      doc.consume(op);
      collector.add(op);
    }
    golden.expect("D", doc.toXmlString());
    for (let i = 0; i < groups / scenarios; i++) {
      const client = generateRandomOp(r, p, doc);
      const server = generateRandomOp(r, p, doc);
      golden.expectOp("C", client);
      golden.expectOp("S", server);
      const pair = transform(client, server);
      golden.expectOp("TC", pair.clientOp);
      golden.expectOp("TS", pair.serverOp);
      golden.expectOp("CS", compose(client, pair.serverOp));
      golden.expectOp("SC", compose(server, pair.clientOp));
      golden.expectOp("IC", invert(client));
      doc.consume(client);
      doc.consume(pair.serverOp);
      collector.add(client);
      collector.add(pair.serverOp);
      golden.expect("D", doc.toXmlString());
    }
    const all = collector.composeAll();
    expect(all).not.toBeNull();
    golden.expectOp("CA", all!);
  }
  expect(golden.atEnd(), "golden file has extra lines").toBe(true);
  expect(golden.checked).toBe(all.length);
  expect(golden.checked).toBe(1620);
});
