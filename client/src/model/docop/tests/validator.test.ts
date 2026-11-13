// Port of `DocOpValidatorTest`, plus tests of well-formedness rules.

import { expect, test } from "vitest";

import {
  AnnotationBoundaryMap,
  Attributes,
  AttributesUpdate,
  BootstrapDocument,
  type ChangeTriple,
  DocOp,
  DocOpBuilder,
  type DocumentSchema,
  EMPTY_DOCUMENT,
  NO_SCHEMA_CONSTRAINTS,
  type PermittedCharacters,
  ValidationResult,
  ViolationCollector,
  coerceString,
  isWellFormed,
  mergeValidationResults,
  validate,
} from "..";
import {
  attrs,
  beginAnnotation,
  boundary,
  expectOperationError,
  finishAnnotation,
} from "./helpers";

/** `DocOpValidatorTest.TEST_CONSTRAINTS`. */
class TestConstraints implements DocumentSchema {
  permitsChild(parent: string | null, child: string): boolean {
    return (
      (parent === null && child === "body") ||
      (parent === "body" && child === "line") ||
      (parent === "body" && child === "otherchildofbody")
    );
  }

  permittedCharacters(element: string | null): PermittedCharacters {
    if (element === "body") {
      return "blipText";
    } else {
      return "none";
    }
  }

  permitsAttribute(_element: string, _name: string): boolean {
    return false;
  }

  permitsAttributeValue(_element: string, _name: string, _value: string): boolean {
    return false;
  }

  requiredInitialChildren(element: string | null): readonly string[] {
    if (element === "body") {
      return ["line"];
    } else {
      return [];
    }
  }
}

const TEST_CONSTRAINTS: DocumentSchema = new TestConstraints();

/**
 * Initializes a document with `d`, validates `m` against it and checks that
 * the outcome is `expected`.
 */
function doTestWithSchema(
  schema: DocumentSchema,
  d: DocOpBuilder,
  m: DocOpBuilder,
  expected: ValidationResult,
): void {
  const doc = new BootstrapDocument();
  doc.consume(d.buildUnchecked());
  const op = m.buildUnchecked();
  const v = new ViolationCollector();
  const result = validate(v, schema, doc, op);
  expect(result, `op: ${op}, violations: ${v}`).toBe(expected);
  expect(v.validationResult()).toBe(result);
  expect(v.isValid()).toBe(result === ValidationResult.Valid);
  expect(v.firstDescription() === null).toBe(result === ValidationResult.Valid);
  // The result must not depend on whether violations are collected.
  expect(validate(null, schema, doc, op)).toBe(result);
}

function doTest(d: DocOpBuilder, m: DocOpBuilder, expected: ValidationResult): void {
  doTestWithSchema(NO_SCHEMA_CONSTRAINTS, d, m, expected);
}

function doSchemaTest(d: DocOpBuilder, m: DocOpBuilder, expected: ValidationResult): void {
  doTestWithSchema(TEST_CONSTRAINTS, d, m, expected);
}

function b(): DocOpBuilder {
  return new DocOpBuilder();
}

function empty(): Attributes {
  return Attributes.EMPTY;
}

const { IllFormed, InvalidDocument, InvalidSchema, Valid } = ValidationResult;

test("test1", () => {
  doTest(b(), b(), Valid);
});

test("test2", () => {
  doTest(b(), b().elementStart("<", empty()).elementEnd(), IllFormed);
});

test("test3", () => {
  doTest(b(), b().elementStart("blip", empty()).elementEnd(), Valid);
});

test("test4", () => {
  doSchemaTest(b(), b().elementStart("p", empty()).elementEnd(), InvalidSchema);
});

test("max skip distance doesnt assert", () => {
  doTest(b().elementStart("blip", empty()).elementEnd(), b().retain(3).retain(1), InvalidDocument);
});

test("unsorted attributes", () => {
  const unchecked = (names: readonly string[]): Attributes =>
    Attributes.fromSortedUnchecked(names.map((name) => ({ name, value: "1" })));
  doTest(b(), b().elementStart("blip", unchecked(["a", "b"])).elementEnd(), Valid);
  doTest(b(), b().elementStart("blip", unchecked(["b", "a"])).elementEnd(), IllFormed);
  doTest(b(), b().elementStart("blip", unchecked(["a", "a"])).elementEnd(), IllFormed);
});

/**
 * A boundary built without the argument checks of the regular constructor
 * (`DumbAnnotationBoundaryMap`).
 */
function dumb(endKeys: readonly string[], changes: readonly ChangeTriple[]): AnnotationBoundaryMap {
  return AnnotationBoundaryMap.unchecked(
    [...endKeys],
    changes.map(([key, oldValue, newValue]) => ({ key, oldValue, newValue })),
  );
}

test("duplicate annotation keys", () => {
  doTest(
    b().characters("ab"),
    b()
      .annotationBoundary(beginAnnotation("a", null, "1"))
      .retain(1)
      .annotationBoundary(boundary(["a"], [["b", null, "2"]]))
      .retain(1)
      .annotationBoundary(finishAnnotation("b")),
    Valid,
  );
  doTest(
    b().characters("ab"),
    b()
      .annotationBoundary(beginAnnotation("a", null, "1"))
      .retain(1)
      .annotationBoundary(dumb(["a"], [["a", null, "2"]]))
      .retain(1)
      .annotationBoundary(finishAnnotation("a")),
    IllFormed,
  );
});

test("unsorted annotation keys", () => {
  const start = (): AnnotationBoundaryMap =>
    boundary(
      [],
      [
        ["a", null, "1"],
        ["b", null, "2"],
      ],
    );
  const middle = (): AnnotationBoundaryMap =>
    boundary(
      ["a"],
      [
        ["b", null, "2"],
        ["c", null, "3"],
      ],
    );
  // all ok
  doTest(
    b().characters("ab"),
    b()
      .annotationBoundary(start())
      .retain(1)
      .annotationBoundary(middle())
      .retain(1)
      .annotationBoundary(finishAnnotation("b", "c")),
    Valid,
  );
  // change keys in wrong order
  doTest(
    b().characters("ab"),
    b()
      .annotationBoundary(
        dumb(
          [],
          [
            ["b", null, "2"],
            ["a", null, "1"],
          ],
        ),
      )
      .retain(1)
      .annotationBoundary(middle())
      .retain(1)
      .annotationBoundary(finishAnnotation("b", "c")),
    IllFormed,
  );
  // end keys in wrong order
  doTest(
    b().characters("ab"),
    b()
      .annotationBoundary(start())
      .retain(1)
      .annotationBoundary(middle())
      .retain(1)
      .annotationBoundary(dumb(["c", "b"], [])),
    IllFormed,
  );
});

function annotatedAB(): DocOpBuilder {
  return b()
    .annotationBoundary(beginAnnotation("a", null, "1"))
    .characters("a")
    .characters("b")
    .annotationBoundary(finishAnnotation("a"));
}

test("deletion annotations are relative", () => {
  // annotations are not needed if previous character has the same annotations
  doTest(annotatedAB(), b().retain(1).deleteCharacters("b"), Valid);
  // but may be specified
  doTest(
    annotatedAB(),
    b()
      .retain(1)
      .annotationBoundary(beginAnnotation("a", "1", "1"))
      .deleteCharacters("b")
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
  // even for null values
  doTest(
    b().characters("a").characters("b"),
    b()
      .retain(1)
      .annotationBoundary(beginAnnotation("a", null, null))
      .deleteCharacters("b")
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
});

test("deletion annotations are relative2", () => {
  const annotated = (text: string): DocOpBuilder =>
    b()
      .annotationBoundary(beginAnnotation("a", null, "1"))
      .characters(text)
      .annotationBoundary(finishAnnotation("a"));
  // annotations are needed if previous character has different annotations
  doTest(annotated("a"), b().deleteCharacters("a"), InvalidDocument);
  // (positive case)
  doTest(
    annotated("a"),
    b()
      .annotationBoundary(beginAnnotation("a", "1", null))
      .deleteCharacters("a")
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
  // this is even true when deleting multiple items in sequence
  doTest(
    annotated("ab"),
    b()
      .annotationBoundary(beginAnnotation("a", "1", null))
      .deleteCharacters("a")
      .annotationBoundary(finishAnnotation("a"))
      .deleteCharacters("b"),
    InvalidDocument,
  );
  // (positive case)
  doTest(
    annotated("ab"),
    b()
      .annotationBoundary(beginAnnotation("a", "1", null))
      .deleteCharacters("a")
      .deleteCharacters("b")
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
});

test("deletion annotations are relative3", () => {
  const doc = (): DocOpBuilder =>
    b()
      .annotationBoundary(beginAnnotation("a", null, "1"))
      .characters("a")
      .annotationBoundary(finishAnnotation("a"))
      .characters("b");
  // annotations are needed if previous character has more annotations
  doTest(doc(), b().retain(1).deleteCharacters("b"), InvalidDocument);
  doTest(
    doc(),
    b()
      .retain(1)
      .annotationBoundary(beginAnnotation("a", null, "1"))
      .deleteCharacters("b")
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
});

test("required tag", () => {
  // ok
  doSchemaTest(
    b(),
    b().elementStart("body", empty()).elementStart("line", empty()).elementEnd().elementEnd(),
    Valid,
  );
  // missing required element
  doSchemaTest(b(), b().elementStart("body", empty()).elementEnd(), InvalidSchema);
  // characters before required element
  doSchemaTest(
    b(),
    b()
      .elementStart("body", empty())
      .characters("a")
      .elementStart("line", empty())
      .elementEnd()
      .elementEnd(),
    InvalidSchema,
  );
  // different element before required element
  doSchemaTest(
    b(),
    b()
      .elementStart("body", empty())
      .elementStart("otherchildofbody", empty())
      .elementEnd()
      .elementStart("line", empty())
      .elementEnd()
      .elementEnd(),
    InvalidSchema,
  );
});

function bodyWithLine(): DocOpBuilder {
  return b()
    .elementStart("body", empty())
    .elementStart("line", empty())
    .elementEnd()
    .elementEnd();
}

test("deleting required tag", () => {
  // ok
  doSchemaTest(
    bodyWithLine(),
    b()
      .deleteElementStart("body", empty())
      .deleteElementStart("line", empty())
      .deleteElementEnd()
      .deleteElementEnd(),
    Valid,
  );
  // missing required element
  doSchemaTest(
    bodyWithLine(),
    b().retain(1).deleteElementStart("line", empty()).deleteElementEnd().retain(1),
    InvalidSchema,
  );
});

test("inserting around required tag", () => {
  // ok to insert after
  doSchemaTest(
    bodyWithLine(),
    b().retain(3).elementStart("line", empty()).elementEnd().retain(1),
    Valid,
  );
  // not ok to insert before
  doSchemaTest(bodyWithLine(), b().retain(1).characters("a").retain(3), InvalidSchema);
});

test("char at past end", () => {
  // this should not crash
  doTest(b(), b().retain(1).deleteCharacters("ab"), InvalidDocument);
});

// The remaining tests are not in the original suite.

test("schema restricts text and attributes", () => {
  // text is allowed in body after the required line, but only blip text
  doSchemaTest(bodyWithLine(), b().retain(3).characters("ok").retain(1), Valid);
  doSchemaTest(bodyWithLine(), b().retain(3).characters("a\u{7}").retain(1), InvalidSchema);
  // no text in line or at top level
  doSchemaTest(bodyWithLine(), b().retain(2).characters("a").retain(2), InvalidSchema);
  doSchemaTest(bodyWithLine(), b().retain(4).characters("a"), InvalidSchema);
  // no attributes at all
  doSchemaTest(
    bodyWithLine(),
    b().retain(1).updateAttributes(AttributesUpdate.single("a", null, "1")).retain(2),
    InvalidSchema,
  );
  doSchemaTest(
    bodyWithLine(),
    b().retain(1).replaceAttributes(empty(), attrs("a", "1")).retain(2),
    InvalidSchema,
  );
  // removing an attribute is never a schema violation
  doSchemaTest(
    bodyWithLine(),
    b().retain(1).updateAttributes(AttributesUpdate.single("a", null, null)).retain(2),
    Valid,
  );
});

test("attribute changes must match document", () => {
  const doc = (): DocOpBuilder => b().elementStart("p", attrs("a", "1")).elementEnd();
  doTest(doc(), b().updateAttributes(AttributesUpdate.single("a", "1", "2")).retain(1), Valid);
  doTest(
    doc(),
    b().updateAttributes(AttributesUpdate.single("a", "x", "2")).retain(1),
    InvalidDocument,
  );
  doTest(doc(), b().replaceAttributes(attrs("a", "1"), empty()).retain(1), Valid);
  doTest(doc(), b().replaceAttributes(empty(), empty()).retain(1), InvalidDocument);
  doTest(doc(), b().retain(1).replaceAttributes(attrs("a", "1"), empty()), InvalidDocument);
  doTest(doc(), b().deleteElementStart("p", empty()).deleteElementEnd(), InvalidDocument);
  doTest(
    doc(),
    b().deleteElementStart("q", attrs("a", "1")).deleteElementEnd(),
    InvalidDocument,
  );
  doTest(doc(), b().deleteElementStart("p", attrs("a", "1")).deleteElementEnd(), Valid);
});

test("annotation old values must match document", () => {
  doTest(
    annotatedAB(),
    b()
      .annotationBoundary(beginAnnotation("a", "1", "2"))
      .retain(2)
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
  doTest(
    annotatedAB(),
    b()
      .annotationBoundary(beginAnnotation("a", null, "2"))
      .retain(2)
      .annotationBoundary(finishAnnotation("a")),
    InvalidDocument,
  );
  // an insertion inherits from the item on its left
  doTest(
    annotatedAB(),
    b()
      .retain(2)
      .annotationBoundary(beginAnnotation("a", "1", null))
      .characters("c")
      .annotationBoundary(finishAnnotation("a")),
    Valid,
  );
  doTest(
    annotatedAB(),
    b()
      .annotationBoundary(beginAnnotation("a", "1", null))
      .characters("c")
      .annotationBoundary(finishAnnotation("a"))
      .retain(2),
    InvalidDocument,
  );
});

function assertIllFormed(m: DocOpBuilder, expectedDescription: string): void {
  const op = m.buildUnchecked();
  const v = new ViolationCollector();
  expect(isWellFormed(v, op), `${op}`).toBe(false);
  expect(v.isIllFormed()).toBe(true);
  const description = v.firstDescription();
  expect(description).not.toBeNull();
  expect(description!.startsWith(`ill-formed: ${expectedDescription}`), description!).toBe(true);
  expectOperationError(() => DocOp.create(op.components), "illFormed");
}

test("well formedness rules", () => {
  assertIllFormed(b().retain(0), "retain item count not positive");
  assertIllFormed(b().characters(""), "characters is empty");
  assertIllFormed(b().deleteCharacters(""), "characters is empty");
  assertIllFormed(b().characters("\u{FFFE}"), "characters component contains invalid unicode");
  assertIllFormed(
    b().deleteCharacters("\u{FDD0}"),
    "delete characters component contains invalid unicode",
  );
  assertIllFormed(b().elementStart("p", empty()), "elementStart with no matching elementEnd");
  assertIllFormed(b().elementEnd(), "elementEnd with no matching elementStart");
  assertIllFormed(
    b().deleteElementStart("p", empty()),
    "deleteElementStart with no matching deleteElementEnd",
  );
  assertIllFormed(b().deleteElementEnd(), "deleteElementEnd with no matching deleteElementStart");
  assertIllFormed(
    b().elementStart("p", empty()).retain(1).elementEnd(),
    "retain inside insert or delete",
  );
  assertIllFormed(
    b().deleteElementStart("p", empty()).retain(1).deleteElementEnd(),
    "retain inside insert or delete",
  );
  assertIllFormed(
    b().elementStart("p", empty()).deleteCharacters("a").elementEnd(),
    "deletion inside insertion",
  );
  assertIllFormed(
    b().deleteElementStart("p", empty()).characters("a").deleteElementEnd(),
    "insertion inside deletion",
  );
  assertIllFormed(
    b().elementStart("p", empty()).updateAttributes(AttributesUpdate.EMPTY).elementEnd(),
    "attribute change inside insert or delete",
  );
  assertIllFormed(
    b().elementStart("p", attrs("9", "1")).elementEnd(),
    "attribute name is not an XML Name",
  );
  assertIllFormed(
    b().elementStart("p", attrs("a", "\u{FFFF}")).elementEnd(),
    "attribute value is not valid UTF-16",
  );
  assertIllFormed(
    b().updateAttributes(
      AttributesUpdate.fromSortedUnchecked([
        { name: "b", oldValue: null, newValue: null },
        { name: "a", oldValue: null, newValue: null },
      ]),
    ),
    "attribute keys not strictly monotonic",
  );
  assertIllFormed(
    b().annotationBoundary(beginAnnotation("a", null, "1")),
    "annotation of key a starts but never ends",
  );
  assertIllFormed(
    b().annotationBoundary(finishAnnotation("a")),
    "annotation of key a ends without having started",
  );
  assertIllFormed(
    b()
      .annotationBoundary(beginAnnotation("a", null, "1"))
      .annotationBoundary(finishAnnotation("a")),
    "adjacent annotation boundaries",
  );
  assertIllFormed(
    b().annotationBoundary(dumb([], [["a?", null, null]])),
    "invalid character in annotation key",
  );
  assertIllFormed(
    b().annotationBoundary(dumb([], [["a", null, "\u{FFFE}"]])),
    "annotation value is not valid UTF-16",
  );
});

test("well formed but not applicable to empty document", () => {
  const op = b()
    .retain(3)
    .deleteCharacters("abc")
    .annotationBoundary(beginAnnotation("a", "1", null))
    .retain(1)
    .annotationBoundary(finishAnnotation("a"))
    .build();
  expect(isWellFormed(null, op)).toBe(true);
  expect(validate(null, NO_SCHEMA_CONSTRAINTS, EMPTY_DOCUMENT, op)).toBe(InvalidDocument);
});

test("validation result merging", () => {
  expect(mergeValidationResults(Valid, IllFormed)).toBe(IllFormed);
  expect(mergeValidationResults(InvalidSchema, InvalidDocument)).toBe(InvalidDocument);
  expect(mergeValidationResults(InvalidSchema, Valid)).toBe(InvalidSchema);
  expect(mergeValidationResults(Valid, Valid)).toBe(Valid);
});

test("permitted characters coerce string", () => {
  expect(coerceString("none", "a")).toBeNull();
  expect(coerceString("blipText", "a\tb\nc\u{7}😀")).toBe("a    b c\u{FFFD}😀");
  expect(coerceString("any", "a\tb\u{FFFE}")).toBe("a\tb\u{FFFD}");
});
