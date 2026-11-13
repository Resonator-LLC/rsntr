// Ports of `RangeNormalizerTest` and `AnnotationsNormalizerTest`.

import { expect, test } from "vitest";

import {
  type AnnotationBoundaryMap,
  AnnotationsNormalizer,
  BufferSink,
  Component,
  DocOp,
  DocOpBuilder,
  type DocOpComponent,
  type OpSink,
  RangeNormalizer,
  normalize,
} from "..";
import { boundary } from "./helpers";

/** Rust's `assert_eq!` on operations. */
function expectOpEquals(expected: DocOp, actual: DocOp): void {
  expect(actual.toString()).toBe(expected.toString());
  expect(expected.equals(actual)).toBe(true);
}

function finish(sink: OpSink): DocOp {
  return DocOp.create(sink.finish());
}

function rangeNormalizer(): RangeNormalizer {
  return new RangeNormalizer(new BufferSink());
}

test("multiple retain normalization", () => {
  const normalizer = rangeNormalizer();
  normalizer.retain(1);
  normalizer.characters("a");
  normalizer.retain(1);
  normalizer.retain(1);
  normalizer.retain(1);
  normalizer.characters("b");
  normalizer.retain(1);
  const expected = new DocOpBuilder()
    .retain(1)
    .characters("a")
    .retain(3)
    .characters("b")
    .retain(1)
    .build();
  expectOpEquals(expected, finish(normalizer));
});

test("empty retain normalization", () => {
  const normalizer = rangeNormalizer();
  normalizer.retain(1);
  normalizer.characters("a");
  normalizer.retain(0);
  normalizer.characters("b");
  normalizer.retain(1);
  const expected = new DocOpBuilder().retain(1).characters("ab").retain(1).build();
  expectOpEquals(expected, finish(normalizer));
});

test("multiple characters normalization", () => {
  const normalizer = rangeNormalizer();
  normalizer.retain(1);
  normalizer.characters("a");
  normalizer.characters("b");
  normalizer.characters("c");
  normalizer.retain(1);
  const expected = new DocOpBuilder().retain(1).characters("abc").retain(1).build();
  expectOpEquals(expected, finish(normalizer));
});

test("empty characters normalization", () => {
  const normalizer = rangeNormalizer();
  normalizer.retain(1);
  normalizer.characters("");
  normalizer.retain(1);
  const expected = new DocOpBuilder().retain(2).build();
  expectOpEquals(expected, finish(normalizer));
});

test("multiple delete characters normalization", () => {
  const normalizer = rangeNormalizer();
  normalizer.retain(1);
  normalizer.deleteCharacters("a");
  normalizer.deleteCharacters("b");
  normalizer.deleteCharacters("c");
  normalizer.retain(1);
  const expected = new DocOpBuilder().retain(1).deleteCharacters("abc").retain(1).build();
  expectOpEquals(expected, finish(normalizer));
});

test("empty delete characters normalization", () => {
  const normalizer = rangeNormalizer();
  normalizer.retain(1);
  normalizer.deleteCharacters("");
  normalizer.retain(1);
  const expected = new DocOpBuilder().retain(2).build();
  expectOpEquals(expected, finish(normalizer));
});

function annotations1(): AnnotationBoundaryMap {
  return boundary(
    [],
    [
      ["x", "m", "a"],
      ["y", "n", "b"],
    ],
  );
}

function annotations2(): AnnotationBoundaryMap {
  return boundary(["y"], [["x", "m", "c"]]);
}

function annotations3(): AnnotationBoundaryMap {
  return boundary(["x"], [["y", "n", "f"]]);
}

function annotations4(): AnnotationBoundaryMap {
  return boundary(["y"], []);
}

function annotations12(): AnnotationBoundaryMap {
  return boundary([], [["x", "m", "c"]]);
}

function annotations23(): AnnotationBoundaryMap {
  return boundary(["x"], [["y", "n", "f"]]);
}

function annotations123(): AnnotationBoundaryMap {
  return boundary([], [["y", "n", "f"]]);
}

function annotationsNormalizer(): AnnotationsNormalizer {
  return new AnnotationsNormalizer(new BufferSink());
}

/** retain(1), then each boundary followed by retain(1). */
function expectedWithBoundaries(maps: AnnotationBoundaryMap[]): DocOp {
  const builder = new DocOpBuilder().retain(1);
  for (const map of maps) {
    builder.annotationBoundary(map).retain(1);
  }
  return builder.build();
}

test("annotation normalization 1", () => {
  const normalizer = annotationsNormalizer();
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations1());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations2());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations3());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations4());
  normalizer.retain(1);
  const expected = expectedWithBoundaries([
    annotations1(),
    annotations2(),
    annotations3(),
    annotations4(),
  ]);
  expectOpEquals(expected, finish(normalizer));
});

test("annotation normalization 2", () => {
  const normalizer = annotationsNormalizer();
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations1());
  normalizer.annotationBoundary(annotations2());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations3());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations4());
  normalizer.retain(1);
  const expected = expectedWithBoundaries([annotations12(), annotations3(), annotations4()]);
  expectOpEquals(expected, finish(normalizer));
});

test("annotation normalization 3", () => {
  const normalizer = annotationsNormalizer();
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations1());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations2());
  normalizer.annotationBoundary(annotations3());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations4());
  normalizer.retain(1);
  const expected = expectedWithBoundaries([annotations1(), annotations23(), annotations4()]);
  expectOpEquals(expected, finish(normalizer));
});

test("annotation normalization 4", () => {
  const normalizer = annotationsNormalizer();
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations1());
  normalizer.annotationBoundary(annotations2());
  normalizer.annotationBoundary(annotations3());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations4());
  normalizer.retain(1);
  const expected = expectedWithBoundaries([annotations123(), annotations4()]);
  expectOpEquals(expected, finish(normalizer));
});

/**
 * The annotation tests with an empty component between the second and third
 * boundary, which must not act as a separator.
 */
function emptyComponentNormalization(empty: DocOpComponent): void {
  const normalizer = annotationsNormalizer();
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations1());
  normalizer.annotationBoundary(annotations2());
  normalizer.push(empty);
  normalizer.annotationBoundary(annotations3());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations4());
  normalizer.retain(1);
  const expected = expectedWithBoundaries([annotations123(), annotations4()]);
  expectOpEquals(expected, finish(normalizer));
}

test("annotations empty retain normalization", () => {
  emptyComponentNormalization(Component.retain(0));
});

test("annotations empty characters normalization", () => {
  emptyComponentNormalization(Component.characters(""));
});

test("annotations empty delete characters normalization", () => {
  emptyComponentNormalization(Component.deleteCharacters(""));
});

// Not in the original suite: the normalizer drops a change that repeats the
// change already in effect.
test("redundant annotation change is dropped", () => {
  const normalizer = annotationsNormalizer();
  normalizer.annotationBoundary(annotations12());
  normalizer.retain(1);
  normalizer.annotationBoundary(annotations12());
  normalizer.retain(1);
  normalizer.annotationBoundary(boundary(["x"], []));
  const expected = new DocOpBuilder()
    .annotationBoundary(annotations12())
    .retain(2)
    .annotationBoundary(boundary(["x"], []))
    .build();
  const range = rangeNormalizer();
  for (const component of normalizer.finish()) {
    range.push(component);
  }
  expectOpEquals(expected, finish(range));
});

test("normalize combines both normalizers", () => {
  const op = new DocOpBuilder()
    .retain(1)
    .retain(2)
    .annotationBoundary(annotations12())
    .characters("a")
    .annotationBoundary(annotations12())
    .characters("b")
    .annotationBoundary(boundary(["x"], []))
    .retain(1)
    .buildUnchecked();
  const expected = new DocOpBuilder()
    .retain(3)
    .annotationBoundary(annotations12())
    .characters("ab")
    .annotationBoundary(boundary(["x"], []))
    .retain(1)
    .build();
  expectOpEquals(expected, normalize(op));
});
