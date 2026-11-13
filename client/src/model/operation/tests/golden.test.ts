// Differential test against the original Java implementation.
//
// `wave_golden.txt` (in the Rust crate's testdata) was produced by
// `WaveGoldenGenerator.java` running the Apache Wave code. The scenarios
// are replayed here with the same random number sequence, so the
// operations are the same, and transforms and wavelet state must match
// record by record.
//
// This file covers the scenario and transform records. Records that hold
// protobuf messages other than operations and deltas (applied deltas,
// stored deltas, snapshots) are read and skipped, as are the leading id
// and version records, which have their own tests.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

import { WaveletData } from "../../data";
import { BootstrapDocument, Component, DocOp } from "../../docop";
import { RandomProviderImpl, defaultParameters, generateRandomOp } from "../../docop/testing";
import type { Parameters } from "../../docop/testing";
import { ParticipantId, WaveletName } from "../../id";
import { HashedVersion, HashedVersionFactory } from "../../version";
import {
  DeltaPair,
  NO_TIMESTAMP,
  TransformedWaveletDelta,
  WaveTransformError,
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
  WaveletOperationError,
  applyWaveletDelta,
  transform,
} from "..";
import { encodeDelta, encodeOperation, fromHex, toHex } from "./protoEncoding";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../../../crates/resonator-model/testdata/wave_golden.txt", import.meta.url),
);

/** Tags of the id and version records that precede the scenarios. */
const ID_AND_VERSION_TAGS = new Set([
  "IDV", "DOM", "ENC", "DEC", "ESC", "ESP", "SPL", "PTH", "IDS", "WMOD", "WLEG", "WDUAL", "WLID",
  "NAME", "URI", "PID", "PIN", "PCMP", "WREF", "WREFE", "TOK", "HVS", "HVC",
]);

function unescape(field: string): string {
  return field.replace(/\\u([0-9a-fA-F]{4})/g, (_, hex: string) =>
    String.fromCharCode(parseInt(hex, 16)),
  );
}

/** The golden records, each a list of unescaped fields. */
class Golden {
  readonly records: string[][];
  next = 0;
  /** Number of records per tag that were compared with computed values. */
  readonly verified = new Map<string, number>();
  /** Number of records per tag that were read without being compared. */
  readonly skipped = new Map<string, number>();

  constructor(text: string) {
    const lines = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
    if (lines[lines.length - 1] === "") {
      lines.pop();
    }
    this.records = lines.map((line) => line.split("\t").map(unescape));
  }

  private count(counts: Map<string, number>, tag: string): void {
    counts.set(tag, (counts.get(tag) ?? 0) + 1);
  }

  peekTag(): string | null {
    return this.records[this.next]?.[0] ?? null;
  }

  /** Reads the next record, which must have the given tag, without checking it. */
  skip(tag: string): string[] {
    const record = this.records[this.next];
    if (record === undefined || record[0] !== tag) {
      throw new Error(`expected ${tag} at golden line ${this.next + 1}, found ${record?.[0]}`);
    }
    this.next += 1;
    this.count(this.skipped, tag);
    return record.slice(1);
  }

  /** Checks that the next record has the given tag and fields. */
  expect(tag: string, fields: readonly string[]): void {
    const line = this.next + 1;
    const record = this.records[this.next];
    if (record === undefined || record[0] !== tag) {
      throw new Error(`expected ${tag} at golden line ${line}, found ${record?.[0]}`);
    }
    this.next += 1;
    const expected = record.slice(1);
    if (expected.length !== fields.length || expected.some((f, i) => f !== fields[i])) {
      // Only build the (large) assertion message on failure.
      expect(fields, `golden line ${line} (${tag})`).toEqual(expected);
    }
    this.count(this.verified, tag);
  }
}

const NAMES: readonly (readonly [string, string, string, string])[] = [
  ["example.com", "w+abc", "example.com", "conv+root"],
  ["example.com", "w+abc", "other.org", "user+a@other.org"],
  ["a.b.c", "w+café", "a.b.c", "conv+中文"],
  ["example.com", "prof+jörg@tūdaliņ.lv", "example.com", "conv+x*y"],
  ["x.org", "w+x\u{10000}y", "y.org", "conv+~~a"],
];
const POOL = ["a@example.com", "b@example.com", "c@other.org", "@example.com"] as const;
const DOCS = ["b+1", "b+2", "attach+x", "conversation", "tr+1"] as const;

function pid(address: string): ParticipantId {
  return ParticipantId.of(address);
}

function ctx(creator: ParticipantId): WaveletOperationContext {
  return new WaveletOperationContext(creator, NO_TIMESTAMP, 1);
}

function list(participants: readonly ParticipantId[]): string {
  return participants.map((p) => `${p},`).join("");
}

function expectDump(golden: Golden, w: WaveletData): void {
  golden.expect("W", [
    String(w.version),
    `${w.hashedVersion.version}:${toHex(w.hashedVersion.historyHash)}`,
    String(w.lastModifiedTime),
    w.creator.toString(),
    String(w.creationTime),
    list(w.participants),
  ]);
  for (const d of w.documents()) {
    golden.expect("WD", [
      d.id,
      d.author.toString(),
      list(d.contributors),
      String(d.lastModifiedTime),
      String(d.lastModifiedVersion),
      d.content.toXmlString(),
    ]);
  }
}

function expectFailingDelta(
  golden: Golden,
  wavelet: WaveletData,
  author: ParticipantId,
  v: HashedVersion,
  ops: WaveletOperation[],
): void {
  const delta = TransformedWaveletDelta.cloneOperations(
    author,
    HashedVersion.unsigned(v.version + ops.length),
    2_000_000,
    ops,
  );
  let outcome = "OK";
  try {
    applyWaveletDelta(delta, wavelet);
  } catch (e) {
    if (!(e instanceof WaveletOperationError) || e.kind !== "deltaFailed") {
      throw e;
    }
    outcome = "OperationException";
  }
  golden.expect("F", [outcome]);
  expectDump(golden, wavelet);
}

function scenarios(golden: Golden): void {
  const p = defaultParameters();
  for (let seed = 0; seed < 12; seed++) {
    const r = RandomProviderImpl.ofSeed(seed);
    const name = WaveletName.ofParts(...NAMES[seed % NAMES.length]!);
    let v = HashedVersionFactory.createVersionZero(name);
    golden.expect("SCN", [String(seed), toHex(v.historyHash)]);
    const mirror = new Map<string, BootstrapDocument>();
    const participants: string[] = [];
    let wavelet: WaveletData | null = null;
    for (let i = 0; i < 6; i++) {
      const author = pid(POOL[r.nextInt(3)]!);
      const nops = 1 + r.nextInt(3);
      const ops: WaveletOperation[] = [];
      for (let k = 0; k < nops; k++) {
        const kind = r.nextInt(6);
        if (kind === 0) {
          const who = POOL[r.nextInt(4)]!;
          const index = participants.indexOf(who);
          if (index >= 0) {
            participants.splice(index, 1);
            ops.push(WaveletOperation.removeParticipant(ctx(author), pid(who)));
          } else {
            participants.push(who);
            ops.push(WaveletOperation.addParticipant(ctx(author), pid(who)));
          }
        } else if (kind === 1) {
          ops.push(WaveletOperation.noOp(ctx(author)));
        } else {
          const docId = DOCS[r.nextInt(5)]!;
          let doc = mirror.get(docId);
          if (doc === undefined) {
            doc = new BootstrapDocument();
            mirror.set(docId, doc);
          }
          const op = generateRandomOp(r, p, doc);
          doc.consume(op);
          ops.push(WaveletOperation.blipContent(ctx(author), docId, op));
        }
      }
      const ts = 1_000_000 + i * 1000;
      const delta = new WaveletDelta(author, v, ops);
      // The serialised client delta: checks the generated operations.
      golden.expect("D", [toHex(encodeDelta(delta.author, delta.targetVersion, delta.ops))]);

      // The applied delta is not built here; its bytes give the next version.
      const applied = fromHex(golden.skip("A")[0]!);
      const v2 = HashedVersionFactory.create(applied, v, nops);
      golden.expect("V", [String(v2.version), toHex(v2.historyHash)]);

      const td = TransformedWaveletDelta.fromDelta(v2, ts, delta);
      expect(td.resultingVersion.equals(v2)).toBe(true);
      expect(td.appliedAtVersion).toBe(v.version);
      // The stored delta record.
      golden.skip("T");
      // The delta as sent to clients: unsigned applied-at version, author, ops.
      golden.expect("U", [
        toHex(encodeDelta(td.author, HashedVersion.unsigned(td.appliedAtVersion), td.ops)),
      ]);

      wavelet ??= WaveletData.empty(name, author, HashedVersion.unsigned(0), ts);
      const before = wavelet.clone();
      applyWaveletDelta(td, wavelet);
      expectDump(golden, wavelet);
      // A copy made before the delta is unaffected and catches up.
      expect(before.version).toBe(v.version);
      applyWaveletDelta(td, before);
      expect(before.equals(wavelet)).toBe(true);
      // The snapshot record.
      golden.skip("S");
      v = v2;
    }
    if (wavelet === null) {
      throw new Error("no wavelet");
    }

    // Deltas whose last operation fails, to exercise the rollback.
    const author = pid(POOL[0]);
    const nobody = pid("nobody@nowhere.com");
    const empty = new BootstrapDocument();
    let op = generateRandomOp(r, p, mirror.get(DOCS[0]) ?? empty);
    expectFailingDelta(golden, wavelet, author, v, [
      WaveletOperation.blipContent(ctx(author), DOCS[0], op),
      WaveletOperation.noOp(ctx(author)),
      WaveletOperation.removeParticipant(ctx(author), nobody),
    ]);
    op = generateRandomOp(r, p, empty);
    expectFailingDelta(golden, wavelet, author, v, [
      WaveletOperation.blipContent(ctx(author), "b+new", op),
      WaveletOperation.addParticipant(ctx(author), nobody),
      WaveletOperation.removeParticipant(ctx(author), nobody),
      WaveletOperation.removeParticipant(ctx(author), nobody),
    ]);
    const retain = DocOp.create([Component.retain(5)]);
    expectFailingDelta(golden, wavelet, author, v, [
      WaveletOperation.blipContent(ctx(author), "b+bad", retain),
    ]);
  }
}

function describe(op: WaveletOperation): string {
  return op.isVersionUpdate() ? `VU:${op}` : toHex(encodeOperation(op));
}

function describeAll(ops: readonly WaveletOperation[]): string {
  return ops.map((op) => `${describe(op)},`).join("");
}

function opPool(
  who: ParticipantId,
  r: RandomProviderImpl,
  p: Parameters,
  base: BootstrapDocument,
): WaveletOperation[] {
  const ops = [
    WaveletOperation.addParticipant(ctx(who), pid("x@example.com")),
    WaveletOperation.addParticipant(ctx(who), pid("a@example.com")),
    WaveletOperation.removeParticipant(ctx(who), pid("a@example.com")),
    WaveletOperation.removeParticipant(ctx(who), pid("b@example.com")),
    WaveletOperation.removeParticipant(ctx(who), pid("x@example.com")),
    WaveletOperation.noOp(ctx(who)),
  ];
  for (const docId of ["b+1", "b+1", "b+2"]) {
    ops.push(WaveletOperation.blipContent(ctx(who), docId, generateRandomOp(r, p, base)));
  }
  return ops;
}

/**
 * Not in the original: applies `first` and then `second` to a wavelet whose
 * document "b+1" holds `base`, and returns the resulting participants and
 * document contents.
 */
function converged(
  base: BootstrapDocument,
  first: readonly WaveletOperation[],
  second: readonly WaveletOperation[],
): string {
  const a = pid("a@example.com");
  const wavelet = WaveletData.empty(
    WaveletName.ofParts(...NAMES[0]!),
    a,
    HashedVersion.unsigned(0),
    0,
  );
  wavelet.createDocument("b+1", a, [], base.asOperation(), 0, 0);
  for (const op of [...first, ...second]) {
    op.apply(wavelet);
  }
  expect(wavelet.version).toBe(first.length + second.length);
  return [
    list(wavelet.participants),
    ...wavelet.documents().map((d) => `${d.id}=${d.content.toXmlString()}`),
  ].join("\n");
}

function transforms(golden: Golden): void {
  const p = defaultParameters();
  const a = pid("a@example.com");
  const b = pid("b@example.com");
  for (let seed = 100; seed < 106; seed++) {
    const r = RandomProviderImpl.ofSeed(seed);
    const base = new BootstrapDocument();
    for (let i = 0; i < 3; i++) {
      base.consume(generateRandomOp(r, p, base));
    }
    const client = opPool(a, r, p, base);
    const server = opPool(b, r, p, base);
    golden.expect("XS", [String(seed)]);
    client.forEach((c, i) => {
      server.forEach((s, j) => {
        const fields = [String(i), String(j)];
        try {
          const pair = transform(c, s);
          expect(pair.clientOp.context.equals(c.context)).toBe(true);
          expect(pair.serverOp.context.equals(s.context)).toBe(true);
          fields.push("OK", describe(pair.clientOp), describe(pair.serverOp));
        } catch (e) {
          if (!(e instanceof WaveTransformError)) {
            throw e;
          }
          fields.push(e.kind === "removedAuthor" ? "RA" : "TE");
        }
        golden.expect("X", fields);
      });
    });

    const cdoc = base.clone();
    const sdoc = base.clone();
    const cd: WaveletOperation[] = [];
    const sd: WaveletOperation[] = [];
    for (let i = 0; i < 2; i++) {
      const c = generateRandomOp(r, p, cdoc);
      cdoc.consume(c);
      cd.push(WaveletOperation.blipContent(ctx(a), "b+1", c));
      const s = generateRandomOp(r, p, sdoc);
      sdoc.consume(s);
      sd.push(WaveletOperation.blipContent(ctx(b), "b+1", s));
    }
    cd.push(client[0]!);
    sd.push(server[0]!);
    sd.push(server[5]!);
    let pair = new DeltaPair(cd, sd).transform();
    golden.expect("DP", [describeAll(pair.client), describeAll(pair.server)]);
    // Both orders of application converge.
    expect(converged(base, cd, pair.server)).toBe(converged(base, sd, pair.client));

    const same = cd.map((op) => op.cloneOp(new WaveletOperationContext(a, 5, 1)));
    pair = new DeltaPair(cd, same).transform();
    golden.expect("DP", [describeAll(pair.client), describeAll(pair.server)]);
  }
}

test("matches original implementation", () => {
  const golden = new Golden(readFileSync(GOLDEN_PATH, "utf8"));
  expect(golden.records.length).toBe(2893);
  // The id and version records come first.
  while (golden.peekTag() !== "SCN") {
    const tag = golden.peekTag();
    if (tag === null || !ID_AND_VERSION_TAGS.has(tag)) {
      throw new Error(`unexpected record ${tag} at golden line ${golden.next + 1}`);
    }
    golden.skip(tag);
  }
  scenarios(golden);
  transforms(golden);
  expect(golden.next, "unchecked golden records").toBe(golden.records.length);

  const counts = (m: Map<string, number>): Record<string, number> =>
    Object.fromEntries([...m].filter(([tag]) => !ID_AND_VERSION_TAGS.has(tag)));
  expect(counts(golden.verified)).toEqual({
    SCN: 12,
    D: 72,
    V: 72,
    U: 72,
    W: 108,
    WD: 389,
    F: 36,
    XS: 6,
    X: 486,
    DP: 12,
  });
  expect(counts(golden.skipped)).toEqual({ A: 72, T: 72, S: 72 });
});
