// Tests for the deliberate deviations from the original: text is counted
// and ordered in UTF-16 code units, characters outside the BMP are allowed,
// and nothing may split a surrogate pair.

import { expect, test } from "vitest";

import {
  AnnotationBoundaryMap,
  Attributes,
  AttributesUpdate,
  BootstrapDocument,
  Component,
  DocOp,
  DocOpAutomaton,
  DocOpBuilder,
  type DocOpComponent,
  NO_SCHEMA_CONSTRAINTS,
  OperationError,
  ValidationResult,
  ViolationCollector,
  compose,
  initialDocumentLength,
  isWellFormed,
  normalize,
  resultingDocumentLength,
  transform,
  validate,
} from "..";
import { creator } from "../testing";
import { cmpUtf16, splitAtUtf16 } from "../utf16";
import {
  beginAnnotation,
  expectOperationError,
  expectTransformError,
  finishAnnotation,
  thrownBy,
} from "./helpers";

const { InvalidDocument, Valid } = ValidationResult;

function docWith(text: string): BootstrapDocument {
  const doc = new BootstrapDocument();
  doc.consume(new DocOpBuilder().characters(text).build());
  return doc;
}

function check(doc: BootstrapDocument, op: DocOp): ValidationResult {
  return validate(null, NO_SCHEMA_CONSTRAINTS, doc, op);
}

/** `assert_eq!` on two operations. */
function expectOpEquals(actual: DocOp, expected: DocOp): void {
  expect(actual.equals(expected), `expected ${expected}, got ${actual}`).toBe(true);
}

/** `assert_eq!` on two documents. */
function expectDocEquals(actual: BootstrapDocument, expected: BootstrapDocument): void {
  expect(actual.equals(expected), `expected ${expected}, got ${actual}`).toBe(true);
}

test("supplementary characters are well formed", () => {
  // The original rejects these as "characters component contains surrogate".
  expect(() =>
    new DocOpBuilder().characters("a😀").deleteCharacters("𝄞").build(),
  ).not.toThrow();
});

test("lengths count utf16 code units", () => {
  const doc = docWith("a😀b");
  expect(doc.length()).toBe(4);
  const op = new DocOpBuilder().retain(1).deleteCharacters("😀").characters("𝄞𝄞").retain(1).build();
  expect(initialDocumentLength(op)).toBe(4);
  expect(resultingDocumentLength(op)).toBe(6);
  expect(check(doc, op)).toBe(Valid);
  // Counting code points instead would make these apply.
  expect(check(doc, creator.identity(3))).toBe(InvalidDocument);
  expect(check(doc, creator.identity(4))).toBe(Valid);
});

test("document round trips supplementary characters", () => {
  const doc = docWith("a😀b");
  expect(doc.toXmlString()).toBe("a😀b");
  doc.consume(creator.deleteCharacters(4, 1, "😀"));
  expect(doc.toXmlString()).toBe("ab");
  doc.consume(creator.insertCharacters(2, 1, "𝄞\u{10FFFD}"));
  expect(doc.toXmlString()).toBe("a𝄞\u{10FFFD}b");
  const copy = new BootstrapDocument();
  copy.consume(doc.asOperation());
  expectDocEquals(doc, copy);
});

test("retain must not end inside a surrogate pair", () => {
  const doc = docWith("a😀b");
  // Between the two halves of the pair.
  expect(check(doc, creator.insertCharacters(4, 2, "x"))).toBe(InvalidDocument);
  expect(check(doc, new DocOpBuilder().retain(2).retain(2).build())).toBe(InvalidDocument);
  expect(
    check(
      doc,
      new DocOpBuilder().retain(2).elementStart("p", Attributes.EMPTY).elementEnd().retain(2).build(),
    ),
  ).toBe(InvalidDocument);
  expect(check(doc, creator.setAnnotation(4, 2, 4, "k", null, "v"))).toBe(InvalidDocument);
  expect(check(doc, creator.setAnnotation(4, 0, 2, "k", null, "v"))).toBe(InvalidDocument);
  // On either side of the pair is fine.
  expect(check(doc, creator.insertCharacters(4, 1, "x"))).toBe(Valid);
  expect(check(doc, creator.insertCharacters(4, 3, "x"))).toBe(Valid);
  expect(check(doc, creator.setAnnotation(4, 1, 3, "k", null, "v"))).toBe(Valid);

  const v = new ViolationCollector();
  validate(v, NO_SCHEMA_CONSTRAINTS, doc, creator.insertCharacters(4, 2, "x"));
  const description = v.firstDescription();
  expect(description).not.toBeNull();
  expect(description!).toContain("retain ends inside a surrogate pair");
});

test("insertion inside a surrogate pair is invalid", () => {
  const doc = docWith("😀");
  const automaton = new DocOpAutomaton(doc, NO_SCHEMA_CONSTRAINTS);
  expect(automaton.checkRetain(1, null)).toBe(InvalidDocument);
  expect(automaton.checkRetain(2, null)).toBe(Valid);
  expect(automaton.checkCharacters("x", null)).toBe(Valid);
  // Force the automaton into the pair, as a generator of invalid ops might.
  automaton.doRetain(1);
  expect(automaton.checkCharacters("x", null)).toBe(InvalidDocument);
  expect(automaton.checkElementStart("p", Attributes.EMPTY, null)).toBe(InvalidDocument);
});

test("deletion must match whole pairs", () => {
  const doc = docWith("a😀b");
  expect(check(doc, creator.deleteCharacters(4, 1, "😀"))).toBe(Valid);
  expect(check(doc, creator.deleteCharacters(4, 0, "a😀b"))).toBe(Valid);
  // Same high surrogate, different low surrogate.
  expect(check(doc, creator.deleteCharacters(4, 1, "😁"))).toBe(InvalidDocument);
  // A deletion cannot start or end in the middle of the pair: a Rust
  // string cannot hold half a pair, and the retain before it is invalid.
  // (A JavaScript string can hold half a pair; see the TS-only test at the
  // end of this file.)
  expect(check(doc, new DocOpBuilder().retain(2).deleteCharacters("b").retain(1).build())).toBe(
    InvalidDocument,
  );
  // Deleting only the character before the pair leaves the pair intact.
  expect(check(doc, creator.deleteCharacters(4, 0, "a"))).toBe(Valid);
});

test("compose splits text between pairs", () => {
  const insert = creator.insertCharacters(0, 0, "a😀b𝄞");
  // Deletes the first pair and annotates the second.
  const second = new DocOpBuilder()
    .retain(1)
    .deleteCharacters("😀")
    .retain(1)
    .annotationBoundary(beginAnnotation("k", null, "v"))
    .retain(2)
    .annotationBoundary(finishAnnotation("k"))
    .build();
  const expected = new DocOpBuilder()
    .characters("ab")
    .annotationBoundary(beginAnnotation("k", null, "v"))
    .characters("𝄞")
    .annotationBoundary(finishAnnotation("k"))
    .build();
  expectOpEquals(compose(insert, second), expected);
});

test("compose reports a split surrogate pair", () => {
  const insert = creator.insertCharacters(0, 0, "😀");
  // Not valid after `insert`: retains half of the pair.
  const split = creator.insertCharacters(2, 1, "x");
  expect(expectOperationError(() => compose(insert, split), "surrogateSplit").detail).toEqual({
    kind: "surrogateSplit",
    offset: 1,
  });
  const deleteHalf = new DocOpBuilder().deleteCharacters("a").retain(1).build();
  expect(expectOperationError(() => compose(insert, deleteHalf), "surrogateSplit").detail).toEqual({
    kind: "surrogateSplit",
    offset: 1,
  });
});

test("transform counts utf16 code units", () => {
  // Document "a😀b"; client inserts after the pair, server before it.
  {
    const client = creator.insertCharacters(4, 3, "𝄞");
    const server = creator.insertCharacters(4, 1, "🚀🚀");
    const pair = transform(client, server);
    expectOpEquals(pair.clientOp, creator.insertCharacters(8, 7, "𝄞"));
    expectOpEquals(pair.serverOp, creator.insertCharacters(6, 1, "🚀🚀"));
  }

  // Concurrent overlapping deletions of text with pairs.
  const client = creator.deleteCharacters(6, 0, "a😀");
  const server = creator.deleteCharacters(6, 1, "😀b𝄞");
  const pair = transform(client, server);
  expectOpEquals(pair.clientOp, creator.deleteCharacters(1, 0, "a"));
  expectOpEquals(pair.serverOp, creator.deleteCharacters(3, 0, "b𝄞"));

  const clientDoc = docWith("a😀b𝄞");
  const serverDoc = clientDoc.clone();
  clientDoc.consume(client);
  clientDoc.consume(pair.serverOp);
  serverDoc.consume(server);
  serverDoc.consume(pair.clientOp);
  expectDocEquals(clientDoc, serverDoc);
  expect(clientDoc.toXmlString()).toBe("");
});

test("transform reports a split surrogate pair", () => {
  // Document "😀"; the server op is invalid: it inserts inside the pair.
  const client = creator.deleteCharacters(2, 0, "😀");
  const server = creator.insertCharacters(2, 1, "x");
  const error = expectTransformError(() => transform(client, server), "operation");
  expect(error.detail.kind).toBe("operation");
  if (error.detail.kind === "operation") {
    expect(error.detail.error).toBeInstanceOf(OperationError);
    expect(error.detail.error.detail).toEqual({ kind: "surrogateSplit", offset: 1 });
  }
  expect(thrownBy(() => transform(server, client))).toBeInstanceOf(Error);
});

test("keys are ordered by utf16 code units", () => {
  // U+FF5E sorts after U+10000 in UTF-16 (0xFF5E > 0xD800) but before it
  // in code point (and UTF-8 byte) order. Both are XML name characters.
  const bmp = "\u{FF5E}";
  const astral = "\u{10000}";
  // Rust: `assert!(bmp < astral)`, which compares in code point order. The
  // JavaScript `<` on strings compares UTF-16 code units, so the code points
  // are compared explicitly here.
  expect(bmp.codePointAt(0)! < astral.codePointAt(0)!).toBe(true);
  // TS-only: and the UTF-16 order is the opposite.
  expect(cmpUtf16(bmp, astral)).toBe(1);

  const attributes = Attributes.fromPairs([
    [bmp, "1"],
    [astral, "2"],
  ]);
  const names = attributes.entries.map((a) => a.name);
  expect(names).toEqual([astral, bmp]);

  const utf16Order = Attributes.fromSortedUnchecked([
    { name: astral, value: "2" },
    { name: bmp, value: "1" },
  ]);
  const codePointOrder = Attributes.fromSortedUnchecked([
    { name: bmp, value: "1" },
    { name: astral, value: "2" },
  ]);
  Attributes.fromSorted([...utf16Order.entries]);
  expectOperationError(() => Attributes.fromSorted([...codePointOrder.entries]));
  const element = (attributes: Attributes): DocOp =>
    new DocOpBuilder().elementStart("p", attributes).elementEnd().buildUnchecked();
  expect(isWellFormed(null, element(utf16Order))).toBe(true);
  expect(isWellFormed(null, element(codePointOrder))).toBe(false);

  // Annotation keys: validation and the normalizer use the same order.
  const annotate = (keys: readonly [string, string]): DocOp =>
    new DocOpBuilder()
      .annotationBoundary(
        AnnotationBoundaryMap.unchecked(
          [],
          keys.map((key) => ({ key, oldValue: null, newValue: "1" })),
        ),
      )
      .characters("x")
      .annotationBoundary(AnnotationBoundaryMap.unchecked([...keys], []))
      .buildUnchecked();
  expect(isWellFormed(null, annotate([astral, bmp]))).toBe(true);
  expect(isWellFormed(null, annotate([bmp, astral]))).toBe(false);
  expectOpEquals(normalize(annotate([bmp, astral])), annotate([astral, bmp]));

  // Updates sort the same way.
  const update = AttributesUpdate.fromTriples([
    [bmp, null, null],
    [astral, null, null],
  ]);
  expect(update.updates[0]!.name).toBe(astral);

  // And so does the document's XML form.
  const doc = new BootstrapDocument();
  doc.consume(annotate([astral, bmp]));
  expect(doc.toXmlString()).toBe(
    `<?a "${astral}"="1" "${bmp}"="1"?>x<?a "${astral}" "${bmp}"?>`,
  );
});

// TS-only (not in unicode.rs): a JavaScript string, unlike a Rust `String`,
// can contain unpaired surrogates. They are invalid text, as in the Java
// original, so every place that carries text must reject them as ill-formed.
test("TS-only: unpaired surrogates are ill formed", () => {
  const loneSurrogates = ["\uD800", "a\uDC00"];
  const cases: [string, (s: string) => DocOpComponent[]][] = [
    ["characters", (s) => [Component.characters(s)]],
    ["deleteCharacters", (s) => [Component.deleteCharacters(s)]],
    [
      "attribute value in elementStart",
      (s) => [
        Component.elementStart("p", Attributes.fromSortedUnchecked([{ name: "a", value: s }])),
        Component.elementEnd(),
      ],
    ],
    [
      "attribute value in deleteElementStart",
      (s) => [
        Component.deleteElementStart("p", Attributes.fromSortedUnchecked([{ name: "a", value: s }])),
        Component.deleteElementEnd(),
      ],
    ],
    [
      "new attribute value in replaceAttributes",
      (s) => [
        Component.replaceAttributes(
          Attributes.EMPTY,
          Attributes.fromSortedUnchecked([{ name: "a", value: s }]),
        ),
      ],
    ],
    [
      "old attribute value in replaceAttributes",
      (s) => [
        Component.replaceAttributes(
          Attributes.fromSortedUnchecked([{ name: "a", value: s }]),
          Attributes.EMPTY,
        ),
      ],
    ],
    [
      "new attribute value in updateAttributes",
      (s) => [
        Component.updateAttributes(
          AttributesUpdate.fromSortedUnchecked([{ name: "a", oldValue: null, newValue: s }]),
        ),
      ],
    ],
    [
      "old attribute value in updateAttributes",
      (s) => [
        Component.updateAttributes(
          AttributesUpdate.fromSortedUnchecked([{ name: "a", oldValue: s, newValue: null }]),
        ),
      ],
    ],
    [
      "annotation key",
      (s) => [
        Component.annotationBoundary(
          AnnotationBoundaryMap.unchecked([], [{ key: s, oldValue: null, newValue: "1" }]),
        ),
        Component.characters("x"),
        Component.annotationBoundary(AnnotationBoundaryMap.unchecked([s], [])),
      ],
    ],
    [
      "annotation new value",
      (s) => [
        Component.annotationBoundary(
          AnnotationBoundaryMap.unchecked([], [{ key: "k", oldValue: null, newValue: s }]),
        ),
        Component.characters("x"),
        Component.annotationBoundary(AnnotationBoundaryMap.unchecked(["k"], [])),
      ],
    ],
    [
      "annotation old value",
      (s) => [
        Component.annotationBoundary(
          AnnotationBoundaryMap.unchecked([], [{ key: "k", oldValue: s, newValue: null }]),
        ),
        Component.characters("x"),
        Component.annotationBoundary(AnnotationBoundaryMap.unchecked(["k"], [])),
      ],
    ],
  ];
  for (const [name, make] of cases) {
    // Sanity check of the case itself: with well-formed text it is accepted.
    expect(isWellFormed(null, DocOp.unchecked(make("ok"))), `${name} with "ok"`).toBe(true);
    for (const s of loneSurrogates) {
      const label = `${name} with ${JSON.stringify(s)}`;
      const components = make(s);
      const e = thrownBy(() => DocOp.create(components));
      expect(e, label).toBeInstanceOf(OperationError);
      expect((e as OperationError).kind, label).toBe("illFormed");
      expect(isWellFormed(null, DocOp.unchecked(components)), label).toBe(false);
    }
  }

  // Splitting between the halves of a pair is reported, not performed.
  const error = expectOperationError(() => splitAtUtf16("a😀b", 2), "surrogateSplit");
  expect(error.detail).toEqual({ kind: "surrogateSplit", offset: 2 });
  // On either side of the pair is fine.
  expect(splitAtUtf16("a😀b", 1)).toEqual(["a", "😀b"]);
  expect(splitAtUtf16("a😀b", 3)).toEqual(["a😀", "b"]);
});
