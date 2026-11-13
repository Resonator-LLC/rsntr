// Reader for `wave_golden.txt` (in the Rust crate's testdata), produced by
// `WaveGoldenGenerator.java` running the Apache Wave code. Shared by the
// golden tests of the id and version modules.
//
// Records are lines of tab-separated fields, the first being a tag.
// Characters outside printable ASCII and the backslash are written as
// backslash-u escapes of their UTF-16 code units.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect } from "vitest";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../../../crates/resonator-model/testdata/wave_golden.txt", import.meta.url),
);

/** Number of records in the golden file. */
export const RECORD_COUNT = 2893;

function unescape(field: string): string {
  return field.replace(/\\u([0-9A-Fa-f]{4})/g, (_, hex: string) =>
    String.fromCharCode(parseInt(hex, 16)),
  );
}

/** The golden records, each a list of unescaped fields. */
export class Golden {
  next = 0;

  private constructor(readonly records: readonly (readonly string[])[]) {}

  static load(): Golden {
    const lines = readFileSync(GOLDEN_PATH, "utf8").split("\n");
    if (lines[lines.length - 1] === "") {
      lines.pop();
    }
    return new Golden(
      lines.map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line).split("\t").map(unescape)),
    );
  }

  /** The tag of the next record; `null` at the end. */
  peekTag(): string | null {
    return this.records[this.next]?.[0] ?? null;
  }

  /** The next record (1-based line and fields after the tag) if it has the given tag. */
  take(tag: string): { line: number; fields: string[] } | null {
    const record = this.records[this.next];
    if (record === undefined || record[0] !== tag) {
      return null;
    }
    this.next += 1;
    return { line: this.next, fields: record.slice(1) };
  }

  /** Checks that the next record has the given tag and fields. */
  expect(tag: string, fields: readonly string[]): void {
    const record = this.take(tag);
    if (record === null) {
      throw new Error(`expected ${tag} at golden line ${this.next + 1}`);
    }
    expect(fields, `golden line ${record.line} (${tag})`).toEqual(record.fields);
  }

  /**
   * Checks all consecutive records with `tag`: `check` maps the first
   * `inputs` fields to the expected remaining fields. Returns the count.
   */
  checkAll(tag: string, inputs: number, check: (f: string[]) => string[]): number {
    let count = 0;
    for (let r = this.take(tag); r !== null; r = this.take(tag)) {
      const input = r.fields.slice(0, inputs);
      expect(check(input), `golden line ${r.line} (${tag} ${JSON.stringify(input)})`).toEqual(
        r.fields.slice(inputs),
      );
      count += 1;
    }
    return count;
  }
}

/** `["OK", ...fields]`, or `["ERR"]` if `f` throws one of the given error types. */
export function result(
  f: () => string[],
  errors: readonly (abstract new (...args: never[]) => Error)[],
): string[] {
  try {
    return ["OK", ...f()];
  } catch (e) {
    if (errors.some((type) => e instanceof type)) {
      return ["ERR"];
    }
    throw e;
  }
}

export function hexEncode(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

export function hexDecode(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  }
  return out;
}
