// Ports of `ComposerTest` and `DocOpCollectorTest`, plus direct tests of
// composition.

import { expect, test } from "vitest";

import {
  Attributes,
  AttributesUpdate,
  type DocOp,
  DocOpBuilder,
  DocOpCollector,
  compose,
  composeAll,
  composeUnchecked,
  decompose,
  initialDocumentLength,
  invert,
  resultingDocumentLength,
} from "..";
import { attrs, beginAnnotation, expectOperationError, finishAnnotation } from "./helpers";

/** Rust's `assert_eq!` on operations. */
function expectOpEquals(expected: DocOp, actual: DocOp): void {
  expect(actual.toString()).toBe(expected.toString());
  expect(expected.equals(actual)).toBe(true);
}

test("document length mismatch", () => {
  const empty = new DocOpBuilder().build();
  const retain = new DocOpBuilder().retain(1).build();
  const first = expectOperationError(() => compose(empty, retain), "illegalComposition");
  expect(first.detail).toEqual({ kind: "illegalComposition" });
  const second = expectOperationError(() => compose(retain, empty), "sizeMismatch");
  expect(second.detail).toEqual({ kind: "sizeMismatch", resulting: 1, initial: 0 });
});

test("composer checking", () => {
  const checked = new DocOpBuilder().build();
  const unchecked = new DocOpBuilder()
    .elementStart(".!$*.4!(,", Attributes.EMPTY)
    .elementEnd()
    .buildUnchecked();
  expectOperationError(() => compose(checked, unchecked), "illFormed");
  // Composing unchecked this time, nothing should fail.
  expectOpEquals(unchecked, composeUnchecked(checked, unchecked));
});

test("simple monotonic composition", () => {
  const collector = new DocOpCollector();
  expect(collector.isEmpty()).toBe(true);
  const a = new DocOpBuilder().characters("a").build();
  const b = new DocOpBuilder().retain(1).characters("b").build();
  const c = new DocOpBuilder().retain(2).characters("c").build();
  const d = new DocOpBuilder().retain(3).characters("d").build();
  const all = [a, b, c, d];
  collector.add(a);
  collector.add(b);
  collector.add(c);
  collector.add(d);
  expect(collector.isEmpty()).toBe(false);
  const expected = new DocOpBuilder().characters("abcd").build();
  const collected = collector.composeAll();
  expect(collected).not.toBeNull();
  expectOpEquals(expected, collected!);
  expect(collector.isEmpty()).toBe(true);
  const composed = composeAll(all);
  expect(composed).not.toBeNull();
  expectOpEquals(expected, composed!);
});

test("empty collection composes to none", () => {
  expect(new DocOpCollector().composeAll()).toBeNull();
  expect(composeAll([])).toBeNull();
});

test("collector reports composition errors", () => {
  const collector = new DocOpCollector();
  collector.add(new DocOpBuilder().characters("a").build());
  // Applies to the empty document, but the collected op produces one item.
  const mismatched = new DocOpBuilder().build();
  expectOperationError(() => collector.add(mismatched));
});

test("insertion then deletion cancel", () => {
  const insert = new DocOpBuilder().retain(1).characters("abc").retain(1).build();
  const del = new DocOpBuilder().retain(2).deleteCharacters("bc").retain(1).build();
  const expected = new DocOpBuilder().retain(1).characters("a").retain(1).build();
  expectOpEquals(expected, compose(insert, del));
});

test("attribute changes compose", () => {
  const insert = new DocOpBuilder().elementStart("p", attrs("a", "1")).elementEnd().build();
  const update = new DocOpBuilder()
    .updateAttributes(
      AttributesUpdate.fromTriples([
        ["a", "1", null],
        ["b", null, "2"],
      ]),
    )
    .retain(1)
    .build();
  const expected = new DocOpBuilder().elementStart("p", attrs("b", "2")).elementEnd().build();
  expectOpEquals(expected, compose(insert, update));

  // An update that does not match the inserted attributes is an error.
  const badUpdate = new DocOpBuilder()
    .updateAttributes(AttributesUpdate.single("a", "9", null))
    .retain(1)
    .build();
  expectOperationError(() => compose(insert, badUpdate), "mismatchedOldValue");

  const first = new DocOpBuilder()
    .updateAttributes(AttributesUpdate.single("a", "1", "2"))
    .retain(1)
    .build();
  const second = new DocOpBuilder()
    .replaceAttributes(attrs("a", "2"), attrs("c", "3"))
    .retain(1)
    .build();
  const expected2 = new DocOpBuilder()
    .replaceAttributes(attrs("a", "1"), attrs("c", "3"))
    .retain(1)
    .build();
  expectOpEquals(expected2, compose(first, second));
});

test("structurally incompatible composition is an error", () => {
  const insertText = new DocOpBuilder().characters("ab").build();
  const deleteElement = new DocOpBuilder()
    .deleteElementStart("p", Attributes.EMPTY)
    .deleteElementEnd()
    .build();
  const error = expectOperationError(
    () => compose(insertText, deleteElement),
    "illegalComposition",
  );
  expect(error.detail).toEqual({ kind: "illegalComposition" });
});

test("annotations compose", () => {
  const first = new DocOpBuilder()
    .annotationBoundary(beginAnnotation("k", null, "1"))
    .retain(2)
    .annotationBoundary(finishAnnotation("k"))
    .retain(1)
    .build();
  const second = new DocOpBuilder()
    .retain(1)
    .annotationBoundary(beginAnnotation("k", "1", "2"))
    .retain(2)
    .annotationBoundary(finishAnnotation("k"))
    .build();
  const expected = new DocOpBuilder()
    .annotationBoundary(beginAnnotation("k", null, "1"))
    .retain(1)
    .annotationBoundary(beginAnnotation("k", null, "2"))
    .retain(1)
    .annotationBoundary(beginAnnotation("k", "1", "2"))
    .retain(1)
    .annotationBoundary(finishAnnotation("k"))
    .build();
  expectOpEquals(expected, compose(first, second));
});

// Quirk preserved from the original: a retain of the second operation that
// outlasts the first operation goes unreported.
test("trailing retain of second operation is not reported", () => {
  const one = new DocOpBuilder().retain(1).build();
  const five = new DocOpBuilder().retain(5).build();
  expectOpEquals(one, compose(one, five));
});

test("inverse composes to identity", () => {
  const op = new DocOpBuilder()
    .retain(1)
    .deleteCharacters("ab")
    .characters("xyz")
    .updateAttributes(AttributesUpdate.single("a", "1", "2"))
    .replaceAttributes(attrs("a", "1"), Attributes.EMPTY)
    .annotationBoundary(beginAnnotation("k", "1", null))
    .retain(2)
    .annotationBoundary(finishAnnotation("k"))
    .deleteElementStart("p", attrs("q", "r"))
    .deleteElementEnd()
    .elementStart("s", Attributes.EMPTY)
    .elementEnd()
    .build();
  const inverse = invert(op);
  const expectedInverse = new DocOpBuilder()
    .retain(1)
    .characters("ab")
    .deleteCharacters("xyz")
    .updateAttributes(AttributesUpdate.single("a", "2", "1"))
    .replaceAttributes(Attributes.EMPTY, attrs("a", "1"))
    .annotationBoundary(beginAnnotation("k", null, "1"))
    .retain(2)
    .annotationBoundary(finishAnnotation("k"))
    .elementStart("p", attrs("q", "r"))
    .elementEnd()
    .deleteElementStart("s", Attributes.EMPTY)
    .deleteElementEnd()
    .build();
  expectOpEquals(expectedInverse, inverse);
  expectOpEquals(op, invert(inverse));
  expect(resultingDocumentLength(inverse)).toBe(initialDocumentLength(op));
  expect(initialDocumentLength(inverse)).toBe(resultingDocumentLength(op));
});

test("decompose splits insertions from the rest", () => {
  const op = new DocOpBuilder()
    .retain(1)
    .characters("ab")
    .deleteCharacters("c")
    .elementStart("p", Attributes.EMPTY)
    .elementEnd()
    .annotationBoundary(beginAnnotation("k", null, "1"))
    .retain(1)
    .annotationBoundary(finishAnnotation("k"))
    .build();
  const [insertion, noninsertion] = decompose(op);
  const expectedInsertion = new DocOpBuilder()
    .retain(1)
    .characters("ab")
    .retain(1)
    .elementStart("p", Attributes.EMPTY)
    .elementEnd()
    .retain(1)
    .build();
  const expectedNoninsertion = new DocOpBuilder()
    .retain(3)
    .deleteCharacters("c")
    .retain(2)
    .annotationBoundary(beginAnnotation("k", null, "1"))
    .retain(1)
    .annotationBoundary(finishAnnotation("k"))
    .build();
  expectOpEquals(expectedInsertion, insertion);
  expectOpEquals(expectedNoninsertion, noninsertion);
});
