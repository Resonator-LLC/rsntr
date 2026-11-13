// The harness of the model golden tests (port of the Rust
// `model_golden_tests.rs`): a wave whose wavelets record their operations,
// and the records a scripted scenario produces, compared with those the
// original Java implementation wrote to `testdata/model_golden.txt`
// (`testdata/ModelGoldenGenerator.java` describes the record format).

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect } from "vitest";

import { WaveletData } from "../../data";
import type { MutableDocument } from "../../doc";
import { toConciseString } from "../../docop";
import { IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import type { WaveletOperation } from "../../operation";
import { HashedVersion } from "../../version";
import { type OpBasedWavelet, WaveView } from "..";

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
      new URL("../../../../../crates/resonator-model/testdata/model_golden.txt", import.meta.url),
    ),
    "utf8",
  ),
);

export const TIME = 1234567890123;

export const alice = (): ParticipantId => ParticipantId.of("alice@example.com");
export const bob = (): ParticipantId => ParticipantId.of("bob@example.com");

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
        return `doc ${kind.blipId} ${toConciseString(kind.op.kind.op)}`;
      }
      break;
    default:
      break;
  }
  throw new Error(`unexpected op ${op}`);
}

/** The names of all scenarios of the golden file, in order. */
export function goldenScenarioNames(): string[] {
  return GOLDEN.filter((line) => line.startsWith("N\t")).map((line) => line.slice(2));
}

/** The lines the generator wrote for a scenario, without the `N` line. */
function goldenScenario(name: string): string[] {
  const start = GOLDEN.indexOf(`N\t${name}`);
  expect(start, `no golden scenario ${name}`).toBeGreaterThanOrEqual(0);
  const result: string[] = [];
  for (let i = start + 1; i < GOLDEN.length && !GOLDEN[i]!.startsWith("N\t"); i++) {
    result.push(GOLDEN[i]!);
  }
  expect(result.length).toBeGreaterThan(0);
  return result;
}

/**
 * A wave whose wavelets record their operations, with the records produced
 * so far (the generator's `Wave`).
 */
export class Wave {
  readonly view: WaveView;
  readonly ids: IdGeneratorImpl;
  private readonly records: string[] = [];
  private unordered = false;

  constructor(author: ParticipantId) {
    this.view = new WaveView(WaveId.of("example.com", "w+wave"), author, {
      timestamp: TIME,
      bumpVersions: true,
    });
    this.ids = new IdGeneratorImpl("example.com", () => "seed");
  }

  private record(...fields: string[]): void {
    this.records.push(fields.map(escape).join("\t"));
  }

  q(answer: string): void {
    this.record("Q", answer);
  }

  static waveletId(id: string): WaveletId {
    return WaveletId.of("example.com", id);
  }

  /**
   * The wavelet with an id, created empty (without participants) at
   * version zero if missing.
   */
  wavelet(id: string): OpBasedWavelet {
    const waveletId = Wave.waveletId(id);
    const existing = this.view.wavelet(waveletId);
    if (existing !== null) {
      return existing;
    }
    const name = WaveletName.of(this.view.waveId(), waveletId);
    return this.view.addWavelet(
      WaveletData.empty(name, this.view.viewer(), HashedVersion.unsigned(0), this.view.timestamp()),
    );
  }

  edit(wavelet: string, doc: string, edit: (doc: MutableDocument) => void): void {
    this.wavelet(wavelet).editDocument(doc, edit);
  }

  /** A step of which only the number of operations is compared. */
  stepUnordered(label: string, step: (wave: Wave) => void): void {
    this.unordered = true;
    this.step(label, step);
    this.unordered = false;
  }

  step(label: string, step: (wave: Wave) => void): void {
    this.record("S", label);
    // The generator's clock: a second per step.
    const now = this.view.timestamp();
    this.view.setTimestamp(now - (now % 1000) + 1000 + (TIME % 1000));
    let failed = false;
    try {
      step(this);
    } catch {
      failed = true;
    }
    let ops: [string, string][] = [];
    for (const wavelet of this.view.wavelets()) {
      for (const op of wavelet.takeOps()) {
        ops.push([wavelet.waveletId().id, describe(op)]);
      }
    }
    if (this.unordered) {
      this.record("C", String(ops.length));
      ops = [];
    }
    for (const [id, op] of ops) {
      this.record("O", id, op);
    }
    if (failed) {
      this.record("E");
    }
    this.state();
  }

  private state(): void {
    const wavelets = [...this.view.wavelets()].sort((a, b) =>
      a.waveletId().id < b.waveletId().id ? -1 : a.waveletId().id > b.waveletId().id ? 1 : 0,
    );
    for (const wavelet of wavelets) {
      const id = wavelet.waveletId().id;
      const data = wavelet.data();
      for (const doc of data.documents()) {
        const xml = doc.content.toXmlString();
        if (xml !== "") {
          this.record("D", id, doc.id, String(doc.lastModifiedVersion), xml);
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
      const actual = this.records[index];
      if (actual !== expected) {
        expect(actual, `scenario ${scenario}, record ${index}, in step ${step}`).toBe(expected);
      }
    });
    expect(this.records.length, `scenario ${scenario}: extra records`).toBe(golden.length);
  }
}
