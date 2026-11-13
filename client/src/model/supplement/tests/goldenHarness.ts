// The harness of the differential tests of the adt, supplement and schema
// modules against the original Java implementation (port of the Rust
// `supplement/golden_tests.rs`).
//
// `testdata/supplement_golden.txt` is written by
// `testdata/SupplementGoldenGenerator.java`, which drives the reference
// classes through scripted scenarios. Each test runs the same script
// against the port and expects the same records; the record format is
// described in the generator. For the schema scenarios the golden records
// are themselves the script: each holds the question and the answer.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect } from "vitest";

import { WaveletData } from "../../data";
import { XmlFragment, type MutableDocument } from "../../doc";
import {
  ValidationResult,
  ViolationCollector,
  normalize,
  toConciseString,
  validateInitialization,
} from "../../docop";
import { ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import type { WaveletOperation } from "../../operation";
import { SchemaCollection } from "../../schema";
import { HashedVersion } from "../../version";
import { WaveView, type OpBasedWavelet } from "../../wavelet";

function lines(text: string): string[] {
  const result = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  if (result[result.length - 1] === "") {
    result.pop();
  }
  return result;
}

const GOLDEN = lines(
  readFileSync(
    fileURLToPath(
      new URL("../../../../../crates/resonator-model/testdata/supplement_golden.txt", import.meta.url),
    ),
    "utf8",
  ),
);

export const TIME = 1234567890123;
export const ROOT = "conv+root";
export const OTHER = "conv+other";
export const UDW = "user+alice@example.com";

export const alice = (): ParticipantId => ParticipantId.of("alice@example.com");
export const bob = (): ParticipantId => ParticipantId.of("bob@example.com");
export const wid = (id: string): WaveletId => WaveletId.of("example.com", id);

/** The escaping of the generator's `line()`: per UTF-16 code unit. */
function escape(field: string): string {
  let escaped = "";
  for (let i = 0; i < field.length; i++) {
    const unit = field.charCodeAt(i);
    if (unit >= 0x20 && unit < 0x7f && unit !== 0x5c) {
      escaped += field.charAt(i);
    } else {
      escaped += "\\u" + unit.toString(16).toUpperCase().padStart(4, "0");
    }
  }
  return escaped;
}

export function unescape(field: string): string {
  return field.replace(/\\u([0-9A-Fa-f]{4})/g, (_, hex: string) =>
    String.fromCharCode(Number.parseInt(hex, 16)),
  );
}

function describe(op: WaveletOperation): string {
  const kind = op.kind;
  switch (kind.type) {
    case "addParticipant":
      return `add ${kind.participant}`;
    case "removeParticipant":
      return `remove ${kind.participant}`;
    case "noOp":
      return "noop";
    case "blip":
      if (kind.op.kind.type === "content") {
        // Normalized, as in the generator.
        return `doc ${kind.blipId} ${toConciseString(normalize(kind.op.kind.op))}`;
      }
      break;
    default:
      break;
  }
  throw new Error(`unexpected op ${op}`);
}

/** The lines the generator wrote for a scenario, without the `N` line. */
export function goldenScenario(name: string): string[] {
  const start = GOLDEN.indexOf(`N\t${name}`);
  expect(start, `no golden scenario ${name}`).toBeGreaterThanOrEqual(0);
  const result: string[] = [];
  for (let i = start + 1; i < GOLDEN.length && !GOLDEN[i]!.startsWith("N\t"); i++) {
    result.push(GOLDEN[i]!);
  }
  expect(result.length).toBeGreaterThan(0);
  return result;
}

/** Java's `String.valueOf` of a nullable value. */
export function nullable(value: { toString(): string } | null): string {
  return value === null ? "null" : value.toString();
}

/** Java's `toString` of a collection. */
export function list(items: Iterable<{ toString(): string }>): string {
  return `[${Array.from(items, (item) => item.toString()).join(", ")}]`;
}

export function resultName(result: ValidationResult): string {
  switch (result) {
    case ValidationResult.IllFormed:
      return "ILL_FORMED";
    case ValidationResult.InvalidDocument:
      return "INVALID_DOCUMENT";
    case ValidationResult.InvalidSchema:
      return "INVALID_SCHEMA";
    case ValidationResult.Valid:
      return "VALID";
  }
}

const byString = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);

/**
 * A wave whose wavelets record their operations, with the records
 * produced so far (the generator's `Wave`).
 */
export class Wave {
  readonly view: WaveView;
  private readonly schemas = SchemaCollection.standard();
  private readonly lines: string[] = [];

  constructor() {
    this.view = new WaveView(WaveId.of("example.com", "w+wave"), alice(), {
      timestamp: TIME,
      bumpVersions: true,
    });
  }

  private record(...fields: string[]): void {
    this.lines.push(fields.map(escape).join("\t"));
  }

  q(answer: string): void {
    this.record("Q", answer);
  }

  /**
   * The wavelet with an id, created empty (without participants) at
   * version zero if missing.
   */
  wavelet(id: string): OpBasedWavelet {
    const waveletId = wid(id);
    const existing = this.view.wavelet(waveletId);
    if (existing !== null) {
      return existing;
    }
    const name = WaveletName.of(this.view.waveId(), waveletId);
    return this.view.addWavelet(
      WaveletData.empty(name, alice(), HashedVersion.unsigned(0), TIME),
    );
  }

  edit<R>(wavelet: string, doc: string, edit: (doc: MutableDocument) => R): R {
    return this.wavelet(wavelet).editDocument(doc, edit);
  }

  /** Appends content to a document. */
  append(wavelet: string, doc: string, xml: string): void {
    const fragment = XmlFragment.parse(xml);
    this.edit(wavelet, doc, (d) => void d.insertXml(d.size(), fragment));
  }

  step(label: string, step: (wave: Wave) => void): void {
    this.record("S", label);
    let failed = false;
    try {
      step(this);
    } catch {
      failed = true;
    }
    const ops: [string, string][] = [];
    for (const wavelet of this.view.wavelets()) {
      for (const op of wavelet.takeOps()) {
        ops.push([wavelet.waveletId().id, describe(op)]);
      }
    }
    // Stable: operations of one wavelet stay in emission order.
    ops.sort((a, b) => byString(a[0], b[0]));
    if (label.startsWith("~")) {
      this.record("U", String(ops.length));
    } else {
      for (const [id, op] of ops) {
        this.record("O", id, op);
      }
    }
    if (failed) {
      this.record("E");
    }
    this.state();
  }

  private state(): void {
    const wavelets = [...this.view.wavelets()].sort((a, b) =>
      byString(a.waveletId().id, b.waveletId().id),
    );
    for (const wavelet of wavelets) {
      const id = wavelet.waveletId().id;
      const data = wavelet.data();
      for (const doc of data.documents()) {
        const xml = doc.content.toXmlString();
        if (xml === "") {
          continue;
        }
        this.record("D", id, doc.id, String(doc.lastModifiedVersion), xml);
        const schema = this.schemas.schemaForId(data.waveletId, doc.id);
        if (schema !== null) {
          const violations = new ViolationCollector();
          const result = validateInitialization(violations, schema, doc.content.asOperation());
          this.record("V", id, doc.id, resultName(result), nullable(violations.firstDescription()));
        }
      }
      this.record(
        "P",
        id,
        String(data.version),
        data.participants.map((p) => p.address).join(" "),
      );
    }
  }

  /** Compares the records produced with the golden ones of a scenario. */
  check(scenario: string): void {
    const golden = goldenScenario(scenario);
    let step = "";
    golden.forEach((expected, index) => {
      if (expected.startsWith("S\t")) {
        step = expected;
      }
      const actual = this.lines[index];
      if (actual !== expected) {
        expect(actual, `scenario ${scenario}, record ${index}, in step ${step}`).toBe(expected);
      }
    });
    expect(this.lines.length, `scenario ${scenario}: extra records`).toBe(golden.length);
  }
}
