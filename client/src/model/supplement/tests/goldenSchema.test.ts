// Scenarios of the schemas (`schemaScenarios` in the generator; port of the
// Rust `supplement/golden_tests/schema.rs`). The golden records hold both
// the questions and the answers.

import { describe, expect, it } from "vitest";
import { XmlFragment } from "../../doc";
import {
  DocOpBuilder,
  ViolationCollector,
  validateInitialization,
  type DocInitialization,
  type PermittedCharacters,
} from "../../docop";
import { SchemaCollection } from "../../schema";
import { goldenScenario, list, nullable, resultName, unescape, wid } from "./goldenHarness";

/** The fields of the `Q` records of a scenario. */
function questions(scenario: string): string[][] {
  return goldenScenario(scenario).map((line) => {
    const fields = line.split("\t").map(unescape);
    expect(fields[0], line).toBe("Q");
    return fields.slice(1);
  });
}

const CHARACTERS: Record<PermittedCharacters, string> = {
  none: "NONE",
  blipText: "BLIP_TEXT",
  any: "ANY",
};

function initialization(xml: string): DocInitialization {
  const b = new DocOpBuilder();
  for (const item of XmlFragment.parse(xml).items) {
    switch (item.type) {
      case "text":
        b.characters(item.text);
        break;
      case "start":
        b.elementStart(item.tag, item.attributes);
        break;
      case "end":
        b.elementEnd();
        break;
    }
  }
  return b.buildInitialization();
}

describe("schema golden", () => {
  const schemas = SchemaCollection.standard();

  it("schema-lookup", () => {
    const records = questions("schema-lookup");
    expect(records.length).toBeGreaterThan(20);
    for (const record of records) {
      const id = wid(record[0]!);
      const schema = schemas.schemaForIdOrDefault(id, record[1]!);
      const actual = [schemas.schemaForId(id, record[1]!) !== null];
      for (const child of ["body", "conversation", "assign", "wavelet", "anything"]) {
        actual.push(schema.permitsChild(null, child));
      }
      expect(actual.map(String), record.join(" ")).toEqual(record.slice(2));
    }
  });

  // Each record is about a parent element type: the text it permits, its
  // required initial children, then each child type followed by whether it
  // is permitted.
  it("schema-structure", () => {
    const records = questions("schema-structure");
    expect(records.length).toBeGreaterThan(200);
    for (const record of records) {
      const schema = schemas.schemaForIdOrDefault(wid(record[0]!), record[1]!);
      const parent = record[2] === "null" ? null : record[2]!;
      const actual = [
        CHARACTERS[schema.permittedCharacters(parent)],
        list(schema.requiredInitialChildren(parent)),
      ];
      for (let i = 5; i < record.length; i += 2) {
        actual.push(record[i]!, String(schema.permitsChild(parent, record[i]!)));
      }
      expect(actual, record.join(" ")).toEqual(record.slice(3));
    }
  });

  // Each record is about an attribute of an element type: whether it is
  // permitted, then each value followed by whether it is permitted.
  it("schema-attributes", () => {
    const records = questions("schema-attributes");
    expect(records.length).toBeGreaterThan(300);
    let exceptions = 0;
    for (const record of records) {
      const schema = schemas.schemaForIdOrDefault(wid(record[0]!), record[1]!);
      const tag = record[2]!;
      const attr = record[3]!;
      expect(String(schema.permitsAttribute(tag, attr)), record.join(" ")).toBe(record[4]);
      for (let i = 5; i < record.length; i += 2) {
        // Where the original throws, the port answers false.
        let expected = record[i + 1]!;
        if (expected === "false (exception)") {
          exceptions += 1;
          expected = "false";
        }
        expect(
          String(schema.permitsAttributeValue(tag, attr, record[i]!)),
          `${tag} ${attr}=${JSON.stringify(record[i])} in ${record[1]} of ${record[0]}`,
        ).toBe(expected);
      }
    }
    expect(exceptions).toBeGreaterThan(0);
  });

  it("schema-documents", () => {
    const records = questions("schema-documents");
    expect(records.length).toBeGreaterThan(60);
    for (const record of records) {
      const schema = schemas.schemaForIdOrDefault(wid(record[0]!), record[1]!);
      const violations = new ViolationCollector();
      const result = validateInitialization(violations, schema, initialization(record[2]!));
      expect(
        [resultName(result), nullable(violations.firstDescription())],
        record.join(" "),
      ).toEqual(record.slice(3));
    }
  });
});
