// Random non-invertible operations, converted to document operations by the
// original and by `Nindo.toDocOp` (`testdata/NindoGoldenGenerator.java`;
// port of the Rust `model_golden_tests/nindo.rs`).

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

import { Attributes, normalize, toConciseString } from "../../docop";
import { Nindo, type NindoBuilder, StandaloneDocument } from "..";
import { escape, lines, unescape } from "./goldenHelpers";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../../../crates/resonator-model/testdata/nindo_golden.txt", import.meta.url),
);

function pairs(fields: readonly string[]): [string, string][] {
  const result: [string, string][] = [];
  for (let i = 0; i < fields.length; i += 2) {
    result.push([fields[i]!, fields[i + 1]!]);
  }
  return result;
}

const value = (field: string): string | null => (field.startsWith("=") ? field.slice(1) : null);

function component(b: NindoBuilder, fields: readonly string[]): void {
  const attributes = (rest: readonly string[]): Attributes => Attributes.fromPairs(pairs(rest));
  switch (fields[0]) {
    case "K":
      b.skip(Number(fields[1]));
      break;
    case "C":
      b.characters(fields[1]!);
      break;
    case "ES":
      b.elementStart(fields[1]!, attributes(fields.slice(2)));
      break;
    case "EE":
      b.elementEnd();
      break;
    case "DC":
      b.deleteCharacters(Number(fields[1]));
      break;
    case "DS":
      b.deleteElementStart();
      break;
    case "DE":
      b.deleteElementEnd();
      break;
    case "RA":
      b.replaceAttributes(attributes(fields.slice(1)));
      break;
    case "UA":
      b.updateAttributes(pairs(fields.slice(1)).map(([name, v]) => [name, value(v)] as const));
      break;
    case "SA":
      b.startAnnotation(fields[1]!, value(fields[2]!));
      break;
    case "EA":
      b.endAnnotation(fields[1]!);
      break;
    default:
      throw new Error(`unknown component ${fields[0]}`);
  }
}

test("random nindos convert as in the original", () => {
  const all = lines(readFileSync(GOLDEN_PATH, "utf8"));
  let doc = new StandaloneDocument();
  const builder = Nindo.builder();
  let run = "";
  const counts = { R: 0, n: 0, o: 0, d: 0 };
  all.forEach((line, number) => {
    const raw = line.split("\t");
    const fields = raw.map(unescape);
    const context = `run ${run}, line ${number + 1}`;
    switch (fields[0]) {
      case "R":
        run = fields[1]!;
        doc = new StandaloneDocument();
        counts.R++;
        break;
      case "n":
        component(builder, fields.slice(1));
        counts.n++;
        break;
      case "o": {
        const op = normalize(builder.build().toDocOp(doc.document()));
        const actual = escape(toConciseString(op));
        if (actual !== raw[1]) {
          expect(actual, context).toBe(raw[1]);
        }
        doc.consume(op);
        counts.o++;
        break;
      }
      case "d": {
        const actual = doc.document().toXmlString();
        if (actual !== fields[1]) {
          expect(actual, context).toBe(fields[1]);
        }
        counts.d++;
        break;
      }
      default:
        throw new Error(`unknown record ${fields[0]} at ${context}`);
    }
  });
  expect(counts.o).toBe(40 * 30);
  expect(counts).toEqual({ R: 40, n: 18795, o: 1200, d: 1200 });
  expect(counts.R + counts.n + counts.o + counts.d).toBe(all.length);
});
