// Ports of the Rust tests of `resonator-model` (`schema/tests.rs`): the port of
// `org.waveprotocol.wave.model.conversation.SchemaConstraintsTest` of Apache
// Wave (Apache License 2.0) and tests of the lookup. The schemas themselves
// are compared with the original's in the supplement golden tests.

import { describe, expect, it } from "vitest";
import {
  Attributes,
  DocOpBuilder,
  ValidationResult,
  validateInitialization,
  type DocInitialization,
  type DocumentSchema,
} from "../../docop";
import { WaveletId } from "../../id";
import {
  ConversationSchemas,
  SchemaCollection,
  XmlSchemaConstraints,
  account,
  conversation,
  isDouble,
  isLong,
  isNonNegativeInteger,
  isPositiveInteger,
  isValidInteger,
  isWaveletId,
  userData,
  type SchemaProvider,
} from "..";

const wid = (id: string): WaveletId => WaveletId.of("example.com", id);

describe("schema", () => {
  it("abstract helper class", () => {
    const schema = conversation.BLIP_SCHEMA_CONSTRAINTS;
    expect(schema.permitsAttribute("body", "x")).toBe(false);
    expect(schema.requiredInitialChildren("body")).toEqual(["line"]);
    expect(schema.requiredInitialChildren("textarea")).toEqual(["line"]);
    expect(schema.requiredInitialChildren("line")).toEqual([]);
    expect(schema.permitsAttributeValue("reply", "id", "...")).toBe(true);
    expect(schema.permitsAttributeValue("reply", "id", "b+sdf")).toBe(true);
    expect(schema.permitsAttributeValue("line", "t", "h3")).toBe(true);
    expect(schema.permitsAttributeValue("line", "t", "z")).toBe(false);
    expect(schema.permitsAttribute("image", "attachment")).toBe(true);
    expect(schema.permitsAttributeValue("image", "attachment", "blahblah")).toBe(true);
    expect(schema.permitsAttribute("image", "something")).toBe(false);
    expect(schema.permitsAttributeValue("image", "something", "stuff")).toBe(false);
    expect(schema.permitsAttribute("caption", "something")).toBe(false);
    expect(schema.permitsAttributeValue("caption", "something", "stuff")).toBe(false);
    expect(schema.permitsChild("body", "line")).toBe(true);
    expect(schema.permitsChild("textarea", "line")).toBe(true);
    expect(schema.permitsChild(null, "body")).toBe(true);
    expect(schema.permitsChild(null, "blah")).toBe(false);
    expect(schema.permittedCharacters("body")).toBe("blipText");
    expect(schema.permittedCharacters("line")).toBe("none");
    expect(schema.permittedCharacters(null)).toBe("none");
  });

  it("rules", () => {
    const schema = new XmlSchemaConstraints();
    expect(schema.permitsChild(null, "a")).toBe(false);
    schema.addChildren(null, ["a"]);
    schema.addChildren("a", ["b", "c"]);
    schema.addChildren("a", ["d"]);
    expect(schema.permitsChild(null, "a")).toBe(true);
    expect(schema.permitsChild(null, "b")).toBe(false);
    for (const child of ["b", "c", "d"]) {
      expect(schema.permitsChild("a", child)).toBe(true);
    }
    expect(schema.permitsChild("b", "a")).toBe(false);
    // The empty string is not an element type.
    schema.addChildren("", ["a"]);
    expect(schema.permitsChild("", "a")).toBe(false);
    expect(schema.permitsChild(null, "")).toBe(false);

    schema.addAttrWithValues("a", "x", ["1"]);
    schema.addAttrWithValues("a", "x", ["2"]);
    expect(schema.permitsAttribute("a", "x")).toBe(true);
    expect(schema.permitsAttribute("a", "y")).toBe(false);
    expect(schema.permitsAttribute("b", "x")).toBe(false);
    expect(schema.permitsAttributeValue("a", "x", "1")).toBe(true);
    expect(schema.permitsAttributeValue("a", "x", "2")).toBe(true);
    expect(schema.permitsAttributeValue("a", "x", "3")).toBe(false);
    expect(schema.permitsAttributeValue("a", "y", "1")).toBe(false);
    // Adding the attribute without values lifts the restriction, and
    // values added afterwards restrict it anew.
    schema.addAttrs("a", ["x"]);
    expect(schema.permitsAttributeValue("a", "x", "3")).toBe(true);
    schema.addAttrWithValues("a", "x", ["4"]);
    expect(schema.permitsAttributeValue("a", "x", "1")).toBe(false);
    expect(schema.permitsAttributeValue("a", "x", "4")).toBe(true);

    schema.setValueCheck((tag, attr, value) =>
      tag === "a" && attr === "n" ? value === "ok" : null,
    );
    expect(schema.permitsAttributeValue("a", "n", "ok")).toBe(true);
    expect(schema.permitsAttributeValue("a", "n", "no")).toBe(false);
    // The check answers for the value only: the attribute is not permitted.
    expect(schema.permitsAttribute("a", "n")).toBe(false);
    expect(schema.permitsAttributeValue("a", "x", "4")).toBe(true);

    expect(schema.permittedCharacters("a")).toBe("none");
    schema.containsBlipText(["a"]);
    schema.containsAnyText(["b"]);
    expect(schema.permittedCharacters("a")).toBe("blipText");
    expect(schema.permittedCharacters("b")).toBe("any");
    expect(schema.permittedCharacters(null)).toBe("none");
    expect(schema.permittedCharacters("")).toBe("none");

    expect(schema.requiredInitialChildren("a")).toEqual([]);
    schema.addRequiredInitial("a", ["b"]);
    expect(schema.requiredInitialChildren("a")).toEqual(["b"]);
    expect(schema.requiredInitialChildren(null)).toEqual([]);
  });

  it("value predicates", () => {
    expect(isValidInteger("-5", -5)).toBe(true);
    expect(isValidInteger("-6", -5)).toBe(false);
    expect(isValidInteger("+7", 0)).toBe(true);
    expect(isValidInteger("7.0", 0)).toBe(false);
    expect(isValidInteger("2147483648", 0)).toBe(false);
    expect(isNonNegativeInteger("0")).toBe(true);
    expect(isNonNegativeInteger("-1")).toBe(false);
    expect(isPositiveInteger("1")).toBe(true);
    expect(isPositiveInteger("0")).toBe(false);
    expect(isWaveletId("anything at all")).toBe(true);
    expect(isLong("-9223372036854775808")).toBe(true);
    expect(isLong("9223372036854775808")).toBe(false);
    expect(isLong("1.5")).toBe(false);
    for (const good of [
      "1",
      "-1.5",
      " 2e3 ",
      ".5",
      "5.",
      "0x1p-2",
      "NaN",
      "Infinity",
      "1d",
      "1e999",
    ]) {
      expect(isDouble(good), good).toBe(true);
    }
    for (const bad of ["", " ", "one", "1e", "0x1", "infinity", "1 2", "++1"]) {
      expect(isDouble(bad), bad).toBe(false);
    }
  });

  it("lookup", () => {
    const schemas = SchemaCollection.standard();
    const root = wid("conv+root");
    const udw = wid("user+alice@example.com");
    expect(schemas.schemaForId(root, "b+abc")).toBe(conversation.BLIP_SCHEMA_CONSTRAINTS);
    expect(schemas.schemaForId(root, "conversation")).toBe(
      conversation.MANIFEST_SCHEMA_CONSTRAINTS,
    );
    expect(schemas.schemaForId(root, "roles")).toBe(account.ROLES_SCHEMA_CONSTRAINTS);
    expect(schemas.schemaForId(root, "tags")).toBeNull();
    expect(schemas.schemaForId(root, "m/read")).toBeNull();
    // A legacy wavelet id without prefix is conversational.
    expect(schemas.schemaForId(wid("legacy"), "b+abc")).toBe(
      conversation.BLIP_SCHEMA_CONSTRAINTS,
    );
    const documents: [string, XmlSchemaConstraints][] = [
      ["m/read", userData.UDW_READ],
      ["m/presentation", userData.UDW_PRESENTATION],
      ["m/folder", userData.UDW_FOLDER],
      ["m/archiving", userData.UDW_ARCHIVE],
      ["m/muted", userData.UDW_MUTE],
      ["m/cleared", userData.UDW_CLEARED],
      ["m/abuse", userData.UDW_ABUSE],
      ["m/seen", userData.UDW_SEEN],
      ["m/gadgets", userData.UDW_GADGET],
    ];
    for (const [doc, schema] of documents) {
      expect(schemas.schemaForId(udw, doc), doc).toBe(schema);
      expect(schemas.schemaForId(root, doc), doc).toBeNull();
    }
    expect(schemas.schemaForId(udw, "b+abc")).toBeNull();
    expect(schemas.schemaForId(udw, "conversation")).toBeNull();
    expect(schemas.schemaForId(wid("other+x"), "b+abc")).toBeNull();

    // Without a schema, anything goes.
    const none = schemas.schemaForIdOrDefault(udw, "m/other");
    expect(none.permitsChild(null, "anything")).toBe(true);
    expect(none.permitsAttributeValue("anything", "x", "y")).toBe(true);
    expect(none.permittedCharacters(null)).toBe("any");

    expect(new SchemaCollection().schemaForId(root, "b+abc")).toBeNull();

    // The first provider with a schema wins.
    const everything: SchemaProvider = {
      schemaForId: (): DocumentSchema => conversation.MANIFEST_SCHEMA_CONSTRAINTS,
    };
    const both = new SchemaCollection();
    both.add(ConversationSchemas);
    both.add(everything);
    expect(both.schemaForId(root, "b+abc")).toBe(conversation.BLIP_SCHEMA_CONSTRAINTS);
    expect(both.schemaForId(udw, "m/read")).toBe(conversation.MANIFEST_SCHEMA_CONSTRAINTS);
  });

  // The server's use: validate a document against the schema found for it.
  it("validation", () => {
    const schemas = SchemaCollection.standard();
    const udw = wid("user+alice@example.com");
    const schema = schemas.schemaForIdOrDefault(udw, "m/read");
    const readState = (version: string): DocInitialization =>
      new DocOpBuilder()
        .elementStart("wavelet", Attributes.fromPairs([["i", "example.com/conv+root"]]))
        .elementStart("all", Attributes.fromPairs([["v", version]]))
        .elementEnd()
        .elementEnd()
        .buildInitialization();
    expect(validateInitialization(null, schema, readState("12"))).toBe(ValidationResult.Valid);
    expect(validateInitialization(null, schema, readState("-12"))).toBe(
      ValidationResult.InvalidSchema,
    );
    // The same content is not a blip.
    const blip = schemas.schemaForIdOrDefault(wid("conv+root"), "b+abc");
    expect(validateInitialization(null, blip, readState("12"))).toBe(
      ValidationResult.InvalidSchema,
    );
  });
});
