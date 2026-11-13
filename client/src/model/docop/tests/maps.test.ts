// Ports of `ImmutableStateMapTest` and `ImmutableUpdateMapTest`, plus
// tests of the compatibility-check quirks of the original maps.

import { expect, test } from "vitest";

import {
  AnnotationBoundaryMap,
  type Attribute,
  type AttributeUpdate,
  Attributes,
  AttributesUpdate,
  type UpdateTriple,
} from "..";
import { checkAttributesSorted, checkUpdatesSorted } from "../attributes";
import { attrs, boundary, expectOperationError, finishAnnotation } from "./helpers";

function update(triples: readonly UpdateTriple[]): AttributesUpdate {
  return AttributesUpdate.fromTriples(triples);
}

/** Rust's `assert_eq!` on attributes. */
function expectAttributesEqual(expected: Attributes, actual: Attributes): void {
  expect(expected.equals(actual)).toBe(true);
}

/** Rust's `assert_eq!` on updates. */
function expectUpdatesEqual(expected: AttributesUpdate, actual: AttributesUpdate): void {
  expect(actual.toString()).toBe(expected.toString());
  expect(expected.equals(actual)).toBe(true);
}

test("removal in update with", () => {
  const updated = attrs("a", "1").updateWith(AttributesUpdate.single("a", "1", null));
  expect(updated.containsKey("a")).toBe(false);
});

test("varargs constructor", () => {
  const a = attrs("c", "0", "a", "1", "b", "2");
  const expected = Attributes.fromSorted([
    { name: "a", value: "1" },
    { name: "b", value: "2" },
    { name: "c", value: "0" },
  ]);
  expectAttributesEqual(expected, a);
  expectOperationError(() =>
    Attributes.fromPairs([
      ["a", "1"],
      ["a", "2"],
    ]),
  );
});

test("update adds attribute", () => {
  const a = attrs("a", "0").updateWith(AttributesUpdate.single("b", null, "1"));
  expect(a.get("a")).toBe("0");
  expect(a.get("b")).toBe("1");
  expect(a.size).toBe(2);
});

// The null-element cases of the Java tests have no counterpart here.
test("check attributes sorted cases", () => {
  const list = (names: readonly string[]): Attribute[] =>
    names.map((name) => ({ name, value: "1" }));
  checkAttributesSorted(list([]));
  checkAttributesSorted(list(["a"]));
  checkAttributesSorted(list(["aa", "ab"]));
  checkAttributesSorted(list(["a", "b", "c"]));
  expectOperationError(() => checkAttributesSorted(list(["asdfa", "asdfb", "asdfb"])));
  expectOperationError(() => checkAttributesSorted(list(["rar", "rar", "rbr"])));
  expectOperationError(() => checkAttributesSorted(list(["a", "a"])));
  expectOperationError(() => checkAttributesSorted(list(["a", "c", "b"])));
});

test("check updates sorted cases", () => {
  const list = (names: readonly string[]): AttributeUpdate[] =>
    names.map((name) => ({ name, oldValue: "0", newValue: null }));
  checkUpdatesSorted(list([]));
  checkUpdatesSorted(list(["a"]));
  checkUpdatesSorted(list(["aa", "ab"]));
  checkUpdatesSorted(list(["a", "b", "c"]));
  expectOperationError(() => checkUpdatesSorted(list(["asdfa", "asdfb", "asdfb"])));
  expectOperationError(() => checkUpdatesSorted(list(["rar", "rar", "rbr"])));
  expectOperationError(() => checkUpdatesSorted(list(["a", "a"])));
  expectOperationError(() => checkUpdatesSorted(list(["ard", "ard"])));
  expectOperationError(() => checkUpdatesSorted(list(["a", "c", "b"])));
});

test("update with checks old values", () => {
  const a = attrs("b", "1", "d", "2");
  // Wrong old value of an existing key.
  expectOperationError(() => a.updateWith(update([["b", "x", "3"]])));
  expectOperationError(() => a.updateWith(update([["b", null, "3"]])));
  // Non-null old value of a key that sorts before an existing key.
  expectOperationError(() => a.updateWith(update([["a", "x", "3"]])));
  expectOperationError(() => a.updateWith(update([["c", "x", null]])));
  // Without the check the new values simply win.
  expectAttributesEqual(
    attrs("a", "3", "b", "1", "d", "2"),
    a.updateWithNoCompatibilityCheck(update([["a", "x", "3"]])),
  );
  expectAttributesEqual(
    attrs("d", "2"),
    a.updateWithNoCompatibilityCheck(update([["b", "x", null]])),
  );
});

// Quirk of ImmutableStateMap.updateWith preserved from the original: the old
// value of an update is not checked once the existing entries are exhausted.
test("update with does not check keys past the last entry", () => {
  const a = attrs("b", "1");
  expectAttributesEqual(
    attrs("b", "1", "c", "3"),
    a.updateWith(update([["c", "bogus", "3"]])),
  );
  expectAttributesEqual(
    attrs("z", "3"),
    Attributes.EMPTY.updateWith(update([["z", "bogus", "3"]])),
  );
});

test("compose with merges and checks shared keys", () => {
  const first = update([
    ["a", null, "1"],
    ["c", "0", "1"],
  ]);
  const second = update([
    ["b", "x", null],
    ["c", "1", "2"],
  ]);
  expectUpdatesEqual(
    update([
      ["a", null, "1"],
      ["b", "x", null],
      ["c", "0", "2"],
    ]),
    first.composeWith(second),
  );
  // The second update's old value must equal the first's new value.
  const mismatched = update([["c", "9", "2"]]);
  expectOperationError(() => first.composeWith(mismatched), "mismatchedOldValue");
  expectUpdatesEqual(first, first.composeWith(AttributesUpdate.EMPTY));
  expectUpdatesEqual(first, AttributesUpdate.EMPTY.composeWith(first));
});

test("update invert and exclude", () => {
  const u = update([
    ["a", null, "1"],
    ["b", "x", null],
  ]);
  expectUpdatesEqual(
    update([
      ["a", "1", null],
      ["b", null, "x"],
    ]),
    u.invert(),
  );
  expectUpdatesEqual(
    update([["b", "x", null]]),
    u.exclude((key) => key === "a"),
  );
  expectOperationError(() =>
    AttributesUpdate.fromTriples([
      ["a", null, null],
      ["a", null, null],
    ]),
  );
});

test("compose with boundary", () => {
  const open = update([
    ["b", null, "1"],
    ["d", "x", "y"],
  ]);
  const map = boundary(
    ["b"],
    [
      ["a", null, "2"],
      ["d", "p", "q"],
    ],
  );
  expectUpdatesEqual(
    update([
      ["a", null, "2"],
      ["d", "p", "q"],
    ]),
    open.composeWithBoundary(map),
  );
  // Ending a key that is not open.
  expectOperationError(() => open.composeWithBoundary(finishAnnotation("c")));
  expectOperationError(() => open.composeWithBoundary(finishAnnotation("z")));
  // A key both ended and changed.
  const both = AnnotationBoundaryMap.unchecked(
    ["b"],
    [{ key: "b", oldValue: null, newValue: "2" }],
  );
  expectOperationError(() => open.composeWithBoundary(both));
});

test("annotation boundary map argument checks", () => {
  const ends = (keys: readonly string[]): AnnotationBoundaryMap => AnnotationBoundaryMap.ends(keys);
  ends(["a", "b"]);
  // Order is not checked at construction.
  ends(["b", "a"]);
  expectOperationError(() => ends(["a", "a"]));
  expectOperationError(() => ends([""]));
  expectOperationError(() => ends(["a?"]));
  expectOperationError(() => ends(["a@b"]));
  expectOperationError(() =>
    AnnotationBoundaryMap.create(["a"], [{ key: "a", oldValue: null, newValue: "1" }]),
  );
  expectOperationError(() =>
    AnnotationBoundaryMap.changesOnly([
      ["a", null, "1"],
      ["a", null, "2"],
    ]),
  );
});
