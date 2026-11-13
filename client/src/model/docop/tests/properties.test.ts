// Randomized property tests, ported from `DomainRandomTester` /
// `DocumentDomainLargeTest` and `OpComparatorsLargeTest.testRandomDocOps`.
//
// Every run is seeded; a failure message names the seed and iteration.

import { expect, test } from "vitest";

import {
  BootstrapDocument,
  DocOp,
  type DocOpComponent,
  NO_SCHEMA_CONSTRAINTS,
  OperationError,
  TransformError,
  ValidationResult,
  ViolationCollector,
  asInitialization,
  compose,
  composeAll,
  composeUnchecked,
  decompose,
  initialDocumentLength,
  invert,
  isWellFormed,
  normalize,
  resultingDocumentLength,
  toXmlString,
  transform,
  validate,
} from "..";
import {
  type Parameters,
  type RandomProvider,
  RandomProviderImpl,
  defaultParameters,
  generateRandomOp,
} from "../testing";

const NUM_ITERATIONS = 50;
const INITIAL_MUTATION_COUNT = 3;
const FEATURE_ITERATION_COUNT = 20;

/** A seeded generator (mulberry32) standing in for Rust's `StdRng`. */
class SeededRandom implements RandomProvider {
  private state: number;

  constructor(seed: number) {
    this.state = seed >>> 0;
  }

  nextInt(upperBound: number): number {
    this.state = (this.state + 0x6d2b79f5) >>> 0;
    let t = this.state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    const unit = ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    return Math.floor(unit * upperBound);
  }

  nextBoolean(): boolean {
    return this.nextInt(2) !== 0;
  }
}

/**
 * The two generator configurations every property is checked with: the
 * original's, and one that also inserts characters outside the BMP.
 */
function parameterSets(): [string, Parameters][] {
  return [
    ["default", defaultParameters()],
    ["supplementary", { ...defaultParameters(), supplementaryCharacters: true }],
  ];
}

class Run {
  constructor(
    readonly context: string,
    readonly r: SeededRandom,
    readonly p: Parameters,
  ) {}

  randomOp(doc: BootstrapDocument): DocOp {
    return generateRandomOp(this.r, this.p, doc);
  }

  /** A document after a few random mutations. */
  initialDocument(): BootstrapDocument {
    const doc = new BootstrapDocument();
    for (let i = 0; i < INITIAL_MUTATION_COUNT; i++) {
      this.apply(this.randomOp(doc), doc);
    }
    return doc;
  }

  apply(op: DocOp, doc: BootstrapDocument): void {
    try {
      doc.consume(op);
    } catch (e) {
      throw new Error(`${this.context}: ${String(e)}\nop: ${op.toString()}\ndoc: ${doc.toString()}`);
    }
  }

  assertEquivalent(what: string, a: BootstrapDocument, b: BootstrapDocument): void {
    const message = `${this.context}: ${what}`;
    expect(a.toXmlString(), message).toBe(b.toXmlString());
    expect(a.equals(b), message).toBe(true);
  }
}

/** Runs `body` once per parameter set and seed. */
function forEachRun(body: (run: Run) => void): void {
  for (const [name, p] of parameterSets()) {
    for (let seed = 0; seed < NUM_ITERATIONS; seed++) {
      body(new Run(`parameters=${name} seed=${seed}`, new SeededRandom(seed), p));
    }
  }
}

function copy(doc: BootstrapDocument): BootstrapDocument {
  const result = new BootstrapDocument();
  result.consume(doc.asOperation());
  return result;
}

test("data operation equivalence", () => {
  forEachRun((run) => {
    const d1 = new BootstrapDocument();
    for (let i = 0; i < INITIAL_MUTATION_COUNT; i++) {
      run.apply(run.randomOp(d1), d1);
      const d2 = copy(d1);
      run.assertEquivalent("document differs from its own initialization", d1, d2);
    }
  });
});

test("operation inversion", () => {
  forEachRun((run) => {
    const d1 = run.initialDocument();
    for (let i = 0; i < FEATURE_ITERATION_COUNT; i++) {
      const backup = d1.clone();
      const op = run.randomOp(d1);
      run.apply(op, d1);
      const reverse = invert(op);
      run.apply(reverse, d1);
      run.assertEquivalent("inverse does not restore the document", d1, backup);
      expect(op.equals(invert(reverse)), run.context).toBe(true);
    }
  });
});

function simpleComposition(emptyInitialState: boolean): void {
  forEachRun((run) => {
    const d1 = emptyInitialState ? new BootstrapDocument() : run.initialDocument();
    const d2 = d1.clone();
    for (let i = 0; i < FEATURE_ITERATION_COUNT; i++) {
      const op1 = run.randomOp(d1);
      run.apply(op1, d1);
      const op2 = run.randomOp(d1);
      run.apply(op2, d1);
      const composed = compose(op1, op2);
      run.apply(composed, d2);
      run.assertEquivalent("composed op differs from sequential application", d1, d2);
    }
  });
}

test("composition on initial state", () => {
  simpleComposition(true);
});

test("composition after mutations", () => {
  simpleComposition(false);
});

test("composition associativity", () => {
  forEachRun((run) => {
    const d1 = run.initialDocument();
    const d2 = d1.clone();
    const d3 = d1.clone();
    for (let i = 0; i < FEATURE_ITERATION_COUNT; i++) {
      const op1 = run.randomOp(d1);
      run.apply(op1, d1);
      const op2 = run.randomOp(d1);
      run.apply(op2, d1);
      const op3 = run.randomOp(d1);
      run.apply(op3, d1);
      const op12 = compose(op1, op2);
      const op23 = compose(op2, op3);
      run.apply(op1, d2);
      run.apply(op23, d2);
      run.apply(op12, d3);
      run.apply(op3, d3);
      run.assertEquivalent("op1;(op2;op3) differs from (op1;op2);op3", d2, d3);
      run.assertEquivalent("composition differs from sequential application", d1, d2);
      // Note that compose(op12, op3) and compose(op1, op23) are not always
      // syntactically equal: they can differ in annotation changes that have
      // no effect (also true of the original).
      const all = composeAll([op1, op2, op3]);
      expect(all, run.context).not.toBeNull();
      const d4 = d3.clone();
      // Rewind d4 to the state before the three ops and replay.
      run.apply(invert(all!), d4);
      run.apply(all!, d4);
      run.assertEquivalent("compose_all differs from sequential application", d1, d4);
    }
  });
});

test("transform diamond property", () => {
  forEachRun((run) => {
    const d1 = run.initialDocument();
    const d2 = d1.clone();
    for (let i = 0; i < FEATURE_ITERATION_COUNT; i++) {
      const original = d1.clone();
      const op1 = run.randomOp(original);
      const op2 = run.randomOp(original);
      run.apply(op1, d1);
      run.apply(op2, d2);
      let pair;
      try {
        pair = transform(op1, op2);
      } catch (e) {
        throw new Error(
          `${run.context}: ${String(e)}\nclient: ${op1.toString()}\nserver: ${op2.toString()}`,
        );
      }
      run.apply(pair.serverOp, d1);
      run.apply(pair.clientOp, d2);
      run.assertEquivalent("client and server diverge", d1, d2);
      // compose(c, s') and compose(s, c') have the same effect on the
      // original document.
      const d3 = original.clone();
      run.apply(compose(op1, pair.serverOp), d3);
      const d4 = original.clone();
      run.apply(compose(op2, pair.clientOp), d4);
      run.assertEquivalent("compose(c, s') differs from sequential", d1, d3);
      run.assertEquivalent("compose(s, c') differs from sequential", d1, d4);
    }
  });
});

// OpComparatorsLargeTest.testRandomDocOps. As the original notes, nothing
// guarantees that neighbouring seeds give different ops; it happens to hold
// for these seeds, here as in Java.
test("random doc ops equality", () => {
  const p = defaultParameters();
  for (let i = 0; i < 200; i++) {
    const doc = new BootstrapDocument();
    for (let j = 0; j < 20; j++) {
      const ra = RandomProviderImpl.ofSeed(i * 20 + j);
      const rb = RandomProviderImpl.ofSeed(i * 20 + j + 1);
      const a = generateRandomOp(ra, p, doc);
      const b = generateRandomOp(rb, p, doc);
      doc.consume(a);
      expect(a.equals(DocOp.unchecked([...a.components]))).toBe(true);
      expect(a.equals(b), `i=${i} j=${j}`).toBe(false);
    }
  }
});

test("random ops are normalized and have consistent lengths", () => {
  forEachRun((run) => {
    const doc = run.initialDocument();
    for (let i = 0; i < FEATURE_ITERATION_COUNT; i++) {
      const op = run.randomOp(doc);
      expect(initialDocumentLength(op), run.context).toBe(doc.length());
      expect(validate(null, NO_SCHEMA_CONSTRAINTS, doc, op), run.context).toBe(
        ValidationResult.Valid,
      );
      const [insertion, noninsertion] = decompose(op);
      const decomposed = doc.clone();
      run.apply(insertion, decomposed);
      run.apply(noninsertion, decomposed);
      run.apply(op, doc);
      expect(resultingDocumentLength(op), run.context).toBe(doc.length());
      run.assertEquivalent("decomposition differs from the op", doc, decomposed);
    }
  });
});

test("supplementary parameter set generates surrogate pairs", () => {
  const p = parameterSets()[1]![1];
  const r = new SeededRandom(1);
  const doc = new BootstrapDocument();
  let seen = false;
  for (let i = 0; i < 20; i++) {
    const op = generateRandomOp(r, p, doc);
    seen ||= op.components.some(
      (c) => c.type === "characters" && /[\uD800-\uDBFF][\uDC00-\uDFFF]/.test(c.characters),
    );
    doc.consume(op);
  }
  expect(seen).toBe(true);
});

// Counts no Rust `usize` can hold are included: in TypeScript they are
// malformed input like any other.
const EXTREME = [
  0,
  1,
  2,
  7,
  Math.floor(Number.MAX_SAFE_INTEGER / 2),
  Number.MAX_SAFE_INTEGER,
  Number.MAX_VALUE,
  -1,
  1.5,
  Number.NaN,
  Number.POSITIVE_INFINITY,
  Number.NEGATIVE_INFINITY,
];

// The last two hold unpaired surrogates, which a Rust string cannot.
const TEXTS = ["", "x", "😀", "a😀b", "￾", "\uD83D", "a\uDE00"];

/** Returns a copy of `op` with one random structural or numeric defect. */
function mutate(r: SeededRandom, op: DocOp, donor: DocOp): DocOp {
  const components: DocOpComponent[] = [...op.components];
  const pick = (len: number): number => r.nextInt(Math.max(len, 1));
  const extreme = (): number => EXTREME[r.nextInt(EXTREME.length)]!;
  const choice = r.nextInt(8);
  if (choice === 0 && components.length > 0) {
    components.splice(pick(components.length), 1);
  } else if (choice === 1 && components.length > 0) {
    const i = pick(components.length);
    components.splice(i, 0, components[i]!);
  } else if (choice === 2 && components.length > 1) {
    const i = pick(components.length);
    const j = pick(components.length);
    const tmp = components[i]!;
    components[i] = components[j]!;
    components[j] = tmp;
  } else if (choice === 3) {
    components.length = pick(components.length);
  } else if (choice === 4 && !donor.isEmpty()) {
    const component = donor.components[pick(donor.length)]!;
    components.splice(pick(components.length + 1), 0, component);
  } else if (choice === 5) {
    const itemCount = extreme();
    components.splice(pick(components.length + 1), 0, { type: "retain", itemCount });
  } else if (choice === 6) {
    const characters = TEXTS[r.nextInt(TEXTS.length)]!;
    const component: DocOpComponent = r.nextBoolean()
      ? { type: "characters", characters }
      : { type: "deleteCharacters", characters };
    components.splice(pick(components.length + 1), 0, component);
  } else {
    const i = components.findIndex((c) => c.type === "retain");
    if (i >= 0) {
      components[i] = { type: "retain", itemCount: extreme() };
    }
  }
  return DocOp.unchecked(components);
}

// Malformed and mismatched operations must produce typed errors, never any
// other failure.
test("malformed input never fails in other ways", () => {
  let errors = 0;
  let successes = 0;
  const kinds = new Set<string>();
  /** Runs `f`, counting success or a typed error; rethrows anything else. */
  const count = (f: () => unknown, allowed: "operation" | "transform" = "operation"): void => {
    try {
      const result = f();
      if (result === false) {
        errors += 1;
      } else {
        successes += 1;
      }
    } catch (e) {
      if (e instanceof OperationError) {
        kinds.add(e.kind);
      } else if (allowed === "transform" && e instanceof TransformError) {
        kinds.add(`transform:${e.kind}`);
      } else {
        throw e;
      }
      errors += 1;
    }
  };
  forEachRun((run) => {
    const doc = run.initialDocument();
    for (let i = 0; i < FEATURE_ITERATION_COUNT; i++) {
      const a = run.randomOp(doc);
      const b = run.randomOp(doc);
      const badA = mutate(run.r, a, b);
      const badB = mutate(run.r, b, a);
      const afterA = doc.clone();
      afterA.consume(a);
      const c = run.randomOp(afterA);
      const badC = mutate(run.r, c, a);

      for (const op of [badA, badB, badC]) {
        const v = new ViolationCollector();
        count(() => isWellFormed(v, op));
        v.toString();
        count(() => validate(v, NO_SCHEMA_CONSTRAINTS, doc, op) === ValidationResult.Valid);
        count(() => DocOp.create([...op.components]));
        count(() => invert(op));
        count(() => normalize(op));
        count(() => decompose(op));
        const before = doc.toXmlString();
        count(() => doc.clone().consume(op));
        count(() => {
          // The document is unchanged on error.
          const target = doc.clone();
          try {
            target.consume(op);
          } catch (e) {
            expect(target.toXmlString()).toBe(before);
            throw e;
          }
        });
        count(() => toXmlString(asInitialization(op)));
        op.toString();
        initialDocumentLength(op);
        resultingDocumentLength(op);
      }
      for (const [first, second] of [
        [a, badC],
        [badA, c],
        [badA, badC],
        [badA, badB],
      ] as const) {
        count(() => compose(first, second));
        count(() => composeUnchecked(first, second));
      }
      for (const [client, server] of [
        [a, badB],
        [badA, b],
        [badA, badB],
        [badA, badC],
      ] as const) {
        count(() => transform(client, server), "transform");
      }
      count(() => composeAll([a, badC, badB]));
    }
  });
  // The mutations must actually exercise the error paths (and not only
  // those).
  expect(errors).toBeGreaterThan(1000);
  expect(successes).toBeGreaterThan(1000);
  expect(kinds.size).toBeGreaterThan(5);
});
