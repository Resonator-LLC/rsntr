// Ports of the Rust tests of `resonator-model` (`adt/tests.rs`), themselves ports
// of the tests in `org.waveprotocol.wave.model.adt.docbased` of Apache Wave
// (Apache License 2.0), without the listener and event tests.

import { describe, expect, it } from "vitest";
import { StandaloneDocument, XmlFragment, type DocView, type NodeId } from "../../doc";
import { WaveletId } from "../../id";
import { HashedVersion } from "../../version";
import {
  AdtError,
  BOOLEAN,
  CompoundInitializer,
  DOUBLE,
  DocumentBasedBasicMap,
  DocumentBasedBasicSet,
  DocumentBasedBasicValue,
  DocumentBasedBoolean,
  DocumentBasedElementList,
  DocumentBasedMonotonicMap,
  DocumentBasedMonotonicValue,
  DocumentBasedSingleton,
  DocumentBasedStructuredValue,
  HASHED_VERSION,
  INTEGER,
  LONG,
  ROOT,
  STRING,
  WAVELET_ID,
  buildAttributes,
  firstTopLevel,
  initialiseAttribute,
  isJavaLong,
  type InitialAttributes,
  type Locator,
} from "..";

/** A document with the given content. */
function document(xml: string): StandaloneDocument {
  const doc = new StandaloneDocument();
  if (xml !== "") {
    doc.mutable().insertXml(0, XmlFragment.parse(xml));
  }
  return doc;
}

const xml = (doc: StandaloneDocument): string => doc.document().toXmlString();

function at(doc: StandaloneDocument, locator: Locator): { view: DocView; node: NodeId } {
  const view = doc.mutable().view();
  const node = locator(view);
  if (node === null) {
    throw new Error("no container");
  }
  return { view, node };
}

function adtError(run: () => unknown): AdtError {
  try {
    run();
  } catch (e) {
    if (e instanceof AdtError) {
      return e;
    }
    throw e;
  }
  throw new Error("expected an AdtError");
}

describe("serializer", () => {
  it("serializers follow java", () => {
    expect(INTEGER.fromAttribute("+12")).toBe(12);
    expect(Object.is(INTEGER.fromAttribute("-0"), 0)).toBe(true);
    expect(INTEGER.fromAttribute("2147483648")).toBeNull();
    expect(INTEGER.fromAttribute("-2147483648")).toBe(-2147483648);
    expect(INTEGER.fromAttribute(" 1")).toBeNull();
    expect(INTEGER.fromAttribute("")).toBeNull();
    expect(INTEGER.fromAttribute("1.0")).toBeNull();
    expect(LONG.fromAttribute("9007199254740991")).toBe(Number.MAX_SAFE_INTEGER);
    // A long that a number cannot hold is not a value here.
    expect(LONG.fromAttribute("9223372036854775807")).toBeNull();
    expect(isJavaLong("9223372036854775807")).toBe(true);
    expect(isJavaLong("9223372036854775808")).toBe(false);
    expect(BOOLEAN.fromAttribute("TRUE")).toBe(true);
    expect(BOOLEAN.fromAttribute("yes")).toBe(false);
    expect(BOOLEAN.toAttribute(true)).toBe("true");
    expect(DOUBLE.fromAttribute(" 1e1 ")).toBe(10);
    expect(DOUBLE.fromAttribute("0x1p3")).toBe(8);
    expect(DOUBLE.fromAttribute("0x.8p1")).toBe(1);
    expect(DOUBLE.fromAttribute("1.5f")).toBe(1.5);
    expect(DOUBLE.fromAttribute("-Infinity")).toBe(Number.NEGATIVE_INFINITY);
    expect(DOUBLE.fromAttribute("1e400")).toBe(Number.POSITIVE_INFINITY);
    expect(DOUBLE.fromAttribute("NaN")).toBeNaN();
    for (const bad of ["", "nan", "inf", "1e", ".", "0x10", "1L", "--1", "1,5"]) {
      expect(DOUBLE.fromAttribute(bad), bad).toBeNull();
    }
    expect(DOUBLE.toAttribute(0.5)).toBe("0.5");
    expect(DOUBLE.toAttribute(100)).toBe("100.0");
    expect(DOUBLE.toAttribute(1e7)).toBe("1.0E7");
    expect(DOUBLE.toAttribute(1.0e-4)).toBe("1.0E-4");
    expect(DOUBLE.toAttribute(-0)).toBe("-0.0");
    expect(DOUBLE.toAttribute(2.2250738585072014e-308)).toBe("2.2250738585072014E-308");
    expect(DOUBLE.toAttribute(5e-324)).toBe("4.9E-324");
    expect(DOUBLE.toAttribute(Number.NaN)).toBe("NaN");
    expect(DOUBLE.toAttribute(Number.POSITIVE_INFINITY)).toBe("Infinity");

    const id = WaveletId.of("example.com", "conv+root");
    expect(WAVELET_ID.toAttribute(id)).toBe("example.com/conv+root");
    expect(WAVELET_ID.fromAttribute("example.com/conv+root")?.equals(id)).toBe(true);
    expect(WAVELET_ID.fromAttribute("conv+root")).toBeNull();
    const legacy = WaveletId.ofLegacy("example.com", "conv/root");
    expect(adtError(() => WAVELET_ID.toAttribute(legacy)).kind).toBe("unserialisable");

    const version = HashedVersion.of(12, new TextEncoder().encode("hash"));
    expect(HASHED_VERSION.toAttribute(version)).toBe("12:aGFzaA==");
    expect(HASHED_VERSION.fromAttribute("12:aGFzaA==")?.equals(version)).toBe(true);
    expect(HASHED_VERSION.fromAttribute("7")?.equals(HashedVersion.unsigned(7))).toBe(true);
    expect(HASHED_VERSION.fromAttribute("x")).toBeNull();
  });
});

describe("initializer", () => {
  it("initialise attribute fails on duplicate key", () => {
    const attrs: InitialAttributes = new Map([["k1", "v1"]]);
    const error = adtError(() => initialiseAttribute(attrs, "k1", "v2"));
    expect(error.detail).toEqual({ kind: "duplicateInitialiserKey", key: "k1" });
    expect(attrs.get("k1")).toBe("v1");
  });

  it("empty compound initialiser does nothing", () => {
    expect(buildAttributes(new CompoundInitializer()).size).toBe(0);
  });

  it("compound initialiser composes attributes", () => {
    const init = new CompoundInitializer()
      .with((target) => void target.set("k1", "v1"))
      .with((target) => void target.set("k2", "v2"));
    expect([...buildAttributes(init)]).toEqual([
      ["k1", "v1"],
      ["k2", "v2"],
    ]);
  });

  it("compound initialiser reports duplicate keys", () => {
    const value = new DocumentBasedBasicValue("k", STRING);
    const init = new CompoundInitializer()
      .with(value.createInitialiser("a"))
      .with(value.createInitialiser("b"));
    expect(adtError(() => buildAttributes(init)).detail).toEqual({
      kind: "duplicateInitialiserKey",
      key: "k",
    });
  });
});

const KEY1 = "bEefFaCe*2";
const KEY2 = "caFeBabE*9";
const SUPPLEMENT = firstTopLevel("supplement");

function readSubstrate(entries: readonly (readonly [string, number])[]): string {
  const body = entries.map(([k, v]) => `<read blipId="${k}" version="${v}"/>`).join("");
  return body === "" ? "<supplement/>" : `<supplement>${body}</supplement>`;
}

describe("basic map", () => {
  const MAP = new DocumentBasedBasicMap("read", "blipId", "version", STRING, INTEGER);
  const target = (entries: readonly (readonly [string, number])[]): StandaloneDocument =>
    document(readSubstrate(entries));
  const get = (doc: StandaloneDocument, key: string): number | null => {
    const { view, node } = at(doc, SUPPLEMENT);
    return MAP.get(view, node, key);
  };
  const keys = (doc: StandaloneDocument): string[] => {
    const { view, node } = at(doc, SUPPLEMENT);
    return MAP.keySet(view, node);
  };
  const put = (doc: StandaloneDocument, key: string, value: number): boolean =>
    MAP.put(doc.mutable(), SUPPLEMENT, key, value);
  const remove = (doc: StandaloneDocument, key: string): void =>
    MAP.remove(doc.mutable(), SUPPLEMENT, key);

  it("put on empty map is returned by get", () => {
    const doc = target([]);
    put(doc, KEY1, 10);
    expect(get(doc, KEY1)).toBe(10);
  });

  it("put on empty map inserts into substrate", () => {
    const doc = target([]);
    put(doc, KEY1, 10);
    expect(xml(doc)).toBe(readSubstrate([[KEY1, 10]]));
  });

  it("load leaves overridden entries in substrate but cleans on write", () => {
    const all = [
      [KEY1, 10],
      [KEY2, 20],
      [KEY1, 30],
    ] as const;
    const doc = target(all);
    expect(get(doc, KEY1)).toBe(30);
    expect(xml(doc)).toBe(readSubstrate(all));
    put(doc, KEY2, 50);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY2, 50],
        [KEY1, 30],
      ]),
    );
  });

  it("put of later values replace old", () => {
    const doc = target([
      [KEY1, 10],
      [KEY2, 20],
    ]);
    put(doc, KEY1, 5);
    expect(get(doc, KEY1)).toBe(5);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY1, 5],
        [KEY2, 20],
      ]),
    );
    put(doc, KEY1, 30);
    expect(get(doc, KEY1)).toBe(30);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY1, 30],
        [KEY2, 20],
      ]),
    );
  });

  it("remote added later value obviates old entry", () => {
    const doc = target([
      [KEY1, 10],
      [KEY2, 20],
    ]);
    expect(get(doc, KEY1)).toBe(10);
    put(doc, KEY2, 50);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY2, 50],
        [KEY1, 10],
      ]),
    );
  });

  it("get put", () => {
    const doc = target([]);
    expect(get(doc, "A")).toBeNull();
    expect(get(doc, "B")).toBeNull();
    expect(put(doc, "A", 1)).toBe(true);
    expect(put(doc, "B", 2)).toBe(true);
    expect(get(doc, "A")).toBe(1);
    expect(get(doc, "B")).toBe(2);
    expect(put(doc, "A", 3)).toBe(true);
    expect(get(doc, "A")).toBe(3);
    expect(put(doc, "A", 3)).toBe(false);
    expect(get(doc, "A")).toBe(3);
  });

  it("key set", () => {
    const doc = target([]);
    expect(keys(doc)).toEqual([]);
    put(doc, "A", 1);
    put(doc, "B", 2);
    expect(keys(doc)).toEqual(["A", "B"]);
    put(doc, "C", 3);
    expect(keys(doc)).toEqual(["A", "B", "C"]);
    remove(doc, "B");
    expect(keys(doc)).toEqual(["A", "C"]);
  });

  it("remove removes", () => {
    const doc = target([]);
    expect(get(doc, "A")).toBeNull();
    put(doc, "A", 1);
    expect(get(doc, "A")).toBe(1);
    remove(doc, "A");
    expect(get(doc, "A")).toBeNull();
  });

  it("malformed entries are ignored and kept", () => {
    const content =
      '<supplement><read blipId="a" version="x"/><read version="1"/>' +
      '<read blipId="a"/><other blipId="a" version="3"/>text</supplement>';
    const doc = document(content);
    expect(get(doc, "a")).toBeNull();
    expect(keys(doc)).toEqual([]);
    put(doc, "a", 2);
    expect(get(doc, "a")).toBe(2);
    expect(xml(doc)).toBe(
      content.replace("<supplement>", '<supplement><read blipId="a" version="2"/>'),
    );
  });

  it("missing container is an error", () => {
    const doc = document("<other/>");
    expect(adtError(() => MAP.put(doc.mutable(), SUPPLEMENT, "a", 1)).kind).toBe(
      "missingContainer",
    );
    expect(doc.ops().length).toBe(1);
  });
});

describe("monotonic map", () => {
  const MAP = new DocumentBasedMonotonicMap("read", "blipId", "version", STRING, INTEGER);
  const target = (entries: readonly (readonly [string, number])[]): StandaloneDocument =>
    document(readSubstrate(entries));
  const get = (doc: StandaloneDocument, key: string): number | null => {
    const { view, node } = at(doc, SUPPLEMENT);
    return MAP.get(view, node, key);
  };
  const put = (doc: StandaloneDocument, key: string, value: number): boolean =>
    MAP.put(doc.mutable(), SUPPLEMENT, key, value);
  const two = [
    [KEY1, 10],
    [KEY2, 20],
  ] as const;

  it("put on empty map is returned by get", () => {
    const doc = target([]);
    put(doc, KEY1, 10);
    expect(get(doc, KEY1)).toBe(10);
  });

  it("put on empty map inserts into substrate", () => {
    const doc = target([]);
    put(doc, KEY1, 10);
    expect(xml(doc)).toBe(readSubstrate([[KEY1, 10]]));
  });

  it("load leaves overridden entries in substrate but cleans on write", () => {
    const all = [
      [KEY1, 10],
      [KEY2, 20],
      [KEY1, 30],
    ] as const;
    const doc = target(all);
    expect(get(doc, KEY1)).toBe(30);
    expect(xml(doc)).toBe(readSubstrate(all));
    put(doc, KEY2, 50);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY2, 50],
        [KEY1, 30],
      ]),
    );
  });

  it("put of a lesser value does nothing", () => {
    const doc = target(two);
    expect(put(doc, KEY1, 5)).toBe(false);
    expect(get(doc, KEY1)).toBe(10);
    expect(xml(doc)).toBe(readSubstrate(two));
    expect(doc.ops().length).toBe(1);
  });

  it("put of an equal value does nothing", () => {
    const doc = target(two);
    expect(put(doc, KEY2, 20)).toBe(false);
    expect(xml(doc)).toBe(readSubstrate(two));
  });

  it("remove put with lesser values", () => {
    const doc = target(two);
    MAP.remove(doc.mutable(), SUPPLEMENT, KEY1);
    put(doc, KEY1, 5);
    expect(get(doc, KEY1)).toBe(5);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY1, 5],
        [KEY2, 20],
      ]),
    );
  });

  it("put of a greater value replaces old", () => {
    const doc = target(two);
    expect(put(doc, KEY1, 30)).toBe(true);
    expect(get(doc, KEY1)).toBe(30);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY1, 30],
        [KEY2, 20],
      ]),
    );
  });

  it("remote added lesser entries get scheduled for removal", () => {
    const doc = target([
      [KEY1, 30],
      [KEY2, 20],
      [KEY1, 10],
    ]);
    expect(get(doc, KEY1)).toBe(30);
    put(doc, KEY2, 50);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY2, 50],
        [KEY1, 30],
      ]),
    );
  });

  it("remote added greater entries obviate old entry", () => {
    const doc = target([
      [KEY1, 10],
      [KEY2, 20],
      [KEY1, 30],
    ]);
    expect(get(doc, KEY1)).toBe(30);
    put(doc, KEY1, 50);
    expect(xml(doc)).toBe(
      readSubstrate([
        [KEY1, 50],
        [KEY2, 20],
      ]),
    );
  });

  it("first of equal values counts", () => {
    const doc = target([
      [KEY1, 30],
      [KEY1, 30],
      [KEY2, 20],
    ]);
    MAP.remove(doc.mutable(), SUPPLEMENT, KEY2);
    expect(xml(doc)).toBe(readSubstrate([[KEY1, 30]]));
    // Two deletions, in document order.
    expect(doc.ops().length).toBe(3);
  });
});

describe("basic set", () => {
  const INTS = new DocumentBasedBasicSet("folder", "id", INTEGER);
  const STRINGS = new DocumentBasedBasicSet("read", "blipId", STRING);
  const substrate = (values: readonly number[]): string => {
    const body = values.map((v) => `<folder id="${v}"/>`).join("");
    return body === "" ? "<supplement/>" : `<supplement>${body}</supplement>`;
  };
  const values = (doc: StandaloneDocument): number[] => {
    const { view, node } = at(doc, SUPPLEMENT);
    return INTS.values(view, node);
  };
  const strings = (doc: StandaloneDocument): string[] => {
    const { view, node } = at(doc, ROOT);
    return STRINGS.values(view, node);
  };
  const contains = (doc: StandaloneDocument, value: string): boolean => {
    const { view, node } = at(doc, ROOT);
    return STRINGS.contains(view, node, value);
  };

  it("add on empty state is returned by get", () => {
    const doc = document(substrate([]));
    INTS.add(doc.mutable(), SUPPLEMENT, 10);
    expect(values(doc)).toEqual([10]);
  });

  it("add on empty state inserts into substrate", () => {
    const doc = document(substrate([]));
    INTS.add(doc.mutable(), SUPPLEMENT, 10);
    expect(xml(doc)).toBe(substrate([10]));
  });

  it("substrate values are reflected", () => {
    expect(values(document(substrate([10])))).toEqual([10]);
    expect(values(document(substrate([30, 10])))).toEqual([10, 30]);
    // Duplicates are not exposed.
    expect(values(document(substrate([10, 10])))).toEqual([10]);
  });

  it("add remove contains", () => {
    const doc = document("");
    expect(strings(doc)).toEqual([]);
    for (const value of ["A", "B", "C"]) {
      expect(STRINGS.add(doc.mutable(), ROOT, value)).toBe(true);
    }
    expect(strings(doc)).toEqual(["A", "B", "C"]);
    expect(contains(doc, "B")).toBe(true);
    expect(contains(doc, "D")).toBe(false);
    expect(STRINGS.add(doc.mutable(), ROOT, "B")).toBe(false);
    expect(strings(doc)).toEqual(["A", "B", "C"]);
    expect(STRINGS.remove(doc.mutable(), ROOT, "B")).toBe(true);
    expect(strings(doc)).toEqual(["A", "C"]);
    expect(contains(doc, "B")).toBe(false);
    STRINGS.remove(doc.mutable(), ROOT, "C");
    STRINGS.remove(doc.mutable(), ROOT, "A");
    expect(strings(doc)).toEqual([]);
    expect(xml(doc)).toBe("");
  });

  it("clear", () => {
    const doc = document("");
    for (const value of ["A", "B", "C"]) {
      STRINGS.add(doc.mutable(), ROOT, value);
    }
    expect(strings(doc)).toEqual(["A", "B", "C"]);
    expect(STRINGS.clear(doc.mutable(), ROOT)).toBe(true);
    expect(strings(doc)).toEqual([]);
    expect(STRINGS.clear(doc.mutable(), ROOT)).toBe(false);
  });

  it("writes delete redundant elements only", () => {
    const doc = document(
      '<supplement><folder id="3"/><x/><folder id="3"/><folder/>' +
        '<folder id="y"/><folder id="4"/><folder id="3"/></supplement>',
    );
    expect(values(doc)).toEqual([3, 4]);
    INTS.remove(doc.mutable(), SUPPLEMENT, 4);
    expect(xml(doc)).toBe(
      '<supplement><folder id="3"/><x/><folder/><folder id="y"/></supplement>',
    );
    INTS.clear(doc.mutable(), SUPPLEMENT);
    expect(xml(doc)).toBe('<supplement><x/><folder/><folder id="y"/></supplement>');
  });
});

describe("boolean", () => {
  const TARGET = new DocumentBasedBoolean("muted", "muted");
  const target = (state: readonly (boolean | null)[]): StandaloneDocument =>
    document(state.map((v) => (v === null ? "<muted/>" : `<muted muted="${v}"/>`)).join(""));
  const get = (doc: StandaloneDocument): boolean | null => {
    const { view, node } = at(doc, ROOT);
    return TARGET.get(view, node);
  };
  const set = (doc: StandaloneDocument, value: boolean | null): boolean =>
    TARGET.set(doc.mutable(), ROOT, value);

  it("initial state is null", () => {
    expect(get(target([]))).toBeNull();
  });

  it("set on initial state", () => {
    const yes = target([]);
    set(yes, true);
    expect(get(yes)).toBe(true);
    const no = target([]);
    set(no, false);
    expect(get(no)).toBe(false);
  });

  it("backwards compatible with existing and corrupt state", () => {
    expect(get(target([true]))).toBe(true);
    expect(get(target([true, true]))).toBe(true);
    expect(get(target([false]))).toBe(false);
    expect(get(target([false, false]))).toBe(false);
  });

  it("write true cleans up", () => {
    const doc = target([false, false]);
    set(doc, true);
    expect(xml(doc)).toBe(xml(target([true])));
  });

  it("write false cleans up", () => {
    const doc = target([false, true, null]);
    // An element without the attribute counts as true.
    expect(get(doc)).toBe(true);
    set(doc, false);
    expect(xml(doc)).toBe(xml(target([false])));
  });

  it("write null cleans up", () => {
    const doc = target([false, true, null]);
    set(doc, null);
    expect(xml(doc)).toBe("");
  });

  it("write does not cleanup foreign elements", () => {
    const doc = document('<muted muted="false"/><muted muted="true"/><foreign/>');
    set(doc, null);
    expect(xml(doc)).toBe("<foreign/>");
  });

  it("write of current value does nothing", () => {
    const doc = target([false, true]);
    expect(set(doc, true)).toBe(false);
    expect(xml(doc)).toBe(xml(target([false, true])));
  });
});

describe("monotonic value", () => {
  const VALUE = new DocumentBasedMonotonicValue("all", "read", INTEGER);
  const substrate = (values: readonly number[]): string => {
    const body = values.map((v) => `<all read="${v}"/>`).join("");
    return body === "" ? "<supplement/>" : `<supplement>${body}</supplement>`;
  };
  const get = (doc: StandaloneDocument): number | null => {
    const { view, node } = at(doc, SUPPLEMENT);
    return VALUE.get(view, node);
  };
  const set = (doc: StandaloneDocument, value: number): boolean =>
    VALUE.set(doc.mutable(), SUPPLEMENT, value);

  it("set on empty state is returned by get and inserts into substrate", () => {
    const doc = document(substrate([]));
    expect(get(doc)).toBeNull();
    expect(set(doc, 10)).toBe(true);
    expect(get(doc)).toBe(10);
    expect(xml(doc)).toBe(substrate([10]));
  });

  it("leaves obsolete entries on load but cleans on write", () => {
    const doc = document(substrate([10, 30, 20]));
    expect(get(doc)).toBe(30);
    expect(xml(doc)).toBe(substrate([10, 30, 20]));
    set(doc, 40);
    expect(get(doc)).toBe(40);
    expect(xml(doc)).toBe(substrate([40]));
  });

  it("set of a lesser value does nothing", () => {
    const doc = document(substrate([10]));
    expect(set(doc, 5)).toBe(false);
    expect(get(doc)).toBe(10);
    expect(xml(doc)).toBe(substrate([10]));
  });

  it("set of a greater value replaces old", () => {
    const doc = document(substrate([10]));
    set(doc, 20);
    expect(get(doc)).toBe(20);
    expect(xml(doc)).toBe(substrate([20]));
  });

  it("remote added lesser entries scheduled for removal", () => {
    const doc = document(substrate([20, 10]));
    expect(get(doc)).toBe(20);
    set(doc, 30);
    expect(get(doc)).toBe(30);
    expect(xml(doc)).toBe(substrate([30]));
  });

  it("remote added greater entries replace old entry but is not cleaned", () => {
    const doc = document(substrate([10, 20]));
    expect(get(doc)).toBe(20);
    expect(xml(doc)).toBe(substrate([10, 20]));
  });

  it("set of an equal value does nothing", () => {
    const doc = document(substrate([10, 20]));
    expect(set(doc, 20)).toBe(false);
    expect(xml(doc)).toBe(substrate([10, 20]));
  });
});

describe("basic value", () => {
  const VALUE = new DocumentBasedBasicValue("state", INTEGER);
  const INBOXING = firstTopLevel("inboxing");
  const substrate = (value: number): string => `<inboxing state="${value}"/>`;
  const get = (doc: StandaloneDocument): number | null => {
    const { view, node } = at(doc, INBOXING);
    return VALUE.get(view, node);
  };
  const set = (doc: StandaloneDocument, value: number | null): void => {
    const mutable = doc.mutable();
    VALUE.set(mutable, INBOXING(mutable.view()) as NodeId, value);
  };

  it("initialiser encodes value and skips null", () => {
    const foo = new DocumentBasedBasicValue("foo", STRING);
    expect([...buildAttributes(foo.createInitialiser("bar"))]).toEqual([["foo", "bar"]]);
    expect(buildAttributes(foo.createInitialiser(null)).size).toBe(0);
  });

  it("set on empty state", () => {
    const doc = document(substrate(0));
    set(doc, 10);
    expect(get(doc)).toBe(10);
    expect(xml(doc)).toBe(substrate(10));
  });

  it("substrate value is reflected", () => {
    expect(get(document(substrate(10)))).toBe(10);
  });

  it("set replaces value", () => {
    const doc = document(substrate(10));
    set(doc, 5);
    expect(get(doc)).toBe(5);
    expect(xml(doc)).toBe(substrate(5));
  });

  it("remote set replaces value", () => {
    const doc = document(substrate(10));
    const mutable = doc.mutable();
    mutable.setElementAttribute(INBOXING(mutable.view()) as NodeId, "state", "5");
    expect(get(doc)).toBe(5);
    expect(xml(doc)).toBe(substrate(5));
  });

  it("set null removes the attribute and same value emits nothing", () => {
    const doc = document(substrate(10));
    set(doc, 10);
    expect(doc.ops().length).toBe(1);
    set(doc, null);
    expect(get(doc)).toBeNull();
    expect(xml(doc)).toBe("<inboxing/>");
  });
});

describe("element list", () => {
  const WORD = ["lah", "dee", "dah"];
  const LIST = new DocumentBasedElementList("foo");
  const VALUE = new DocumentBasedBasicValue("bar", STRING);
  const FRESH = firstTopLevel("blank");
  const PREPOPULATED = firstTopLevel("preopopulated");
  const PREPOPULATED_SIZE = 3;
  const lists = (): StandaloneDocument =>
    document(
      '<blank/><preopopulated><random flubble="huh" foo="1"/>' +
        '<foo bar="0"/><foo bar="1"/><foo bar="2"/></preopopulated>',
    );
  const attributes = (value: string | null): InitialAttributes =>
    buildAttributes(VALUE.createInitialiser(value));
  const values = (doc: StandaloneDocument, list: Locator): (string | null)[] => {
    const { view, node } = at(doc, list);
    return LIST.values(view, node, (v, element) => VALUE.get(v, element));
  };
  const size = (doc: StandaloneDocument, list: Locator): number => {
    const { view, node } = at(doc, list);
    return LIST.size(view, node);
  };

  it("empty list", () => {
    expect(size(lists(), FRESH)).toBe(0);
  });

  it("add", () => {
    const doc = lists();
    WORD.forEach((word, index) => {
      const mutable = doc.mutable();
      const element = LIST.add(mutable, FRESH, attributes(null));
      VALUE.set(mutable, element, word);
      expect(size(doc, FRESH)).toBe(index + 1);
      expect(values(doc, FRESH)[index]).toBe(word);
    });
  });

  it("add with initial state", () => {
    const mutable = lists().mutable();
    const element = LIST.add(mutable, FRESH, attributes("baz"));
    expect(VALUE.get(mutable.view(), element)).toBe("baz");
  });

  it("remove", () => {
    const doc = lists();
    expect(size(doc, FRESH)).toBe(0);
    LIST.add(doc.mutable(), FRESH, attributes(null));
    expect(size(doc, FRESH)).toBe(1);
    expect(LIST.remove(doc.mutable(), FRESH, 0)).toBe(true);
    expect(size(doc, FRESH)).toBe(0);
    expect(LIST.remove(doc.mutable(), FRESH, 0)).toBe(false);
  });

  it("clear", () => {
    const doc = lists();
    LIST.clear(doc.mutable(), FRESH);
    expect(size(doc, FRESH)).toBe(0);
    expect(size(doc, PREPOPULATED)).not.toBe(0);
    LIST.clear(doc.mutable(), PREPOPULATED);
    expect(size(doc, PREPOPULATED)).toBe(0);
    expect(xml(doc)).toBe(
      '<blank/><preopopulated><random flubble="huh" foo="1"/></preopopulated>',
    );
  });

  it("can iterate over values", () => {
    const doc = lists();
    for (const word of WORD) {
      LIST.add(doc.mutable(), FRESH, attributes(word));
    }
    expect(values(doc, FRESH)).toEqual(WORD);
  });

  it("pre populated", () => {
    const doc = lists();
    expect(size(doc, PREPOPULATED)).toBe(PREPOPULATED_SIZE);
    expect(values(doc, PREPOPULATED)).toEqual(["0", "1", "2"]);
  });

  it("index of", () => {
    const mutable = lists().mutable();
    let container = PREPOPULATED(mutable.view()) as NodeId;
    for (let index = 0; index < PREPOPULATED_SIZE; index++) {
      const element = LIST.get(mutable.view(), container, index) as NodeId;
      expect(LIST.indexOf(mutable.view(), container, element)).toBe(index);
    }
    const last = LIST.add(mutable, PREPOPULATED, attributes(null));
    container = PREPOPULATED(mutable.view()) as NodeId;
    expect(LIST.indexOf(mutable.view(), container, last)).toBe(PREPOPULATED_SIZE);
    const fresh = FRESH(mutable.view()) as NodeId;
    expect(LIST.indexOf(mutable.view(), fresh, last)).toBeNull();
  });

  it("add at invalid index", () => {
    const doc = lists();
    const error = adtError(() =>
      LIST.addAt(doc.mutable(), PREPOPULATED, PREPOPULATED_SIZE + 1, attributes(null)),
    );
    expect(error.detail).toEqual({ kind: "indexOutOfBounds", index: 4, size: 3 });
    expect(doc.ops().length).toBe(1);
  });

  it("add at index", () => {
    const doc = lists();
    LIST.addAt(doc.mutable(), FRESH, 0, attributes("first"));
    expect(values(doc, FRESH)).toEqual(["first"]);
    LIST.addAt(doc.mutable(), PREPOPULATED, 1, attributes("new second"));
    expect(values(doc, PREPOPULATED)).toEqual(["0", "new second", "1", "2"]);
    LIST.addAt(doc.mutable(), PREPOPULATED, 0, attributes("new first"));
    expect(values(doc, PREPOPULATED)).toEqual(["new first", "0", "new second", "1", "2"]);
    // The first item goes before the first item's element, after the
    // foreign element.
    expect(xml(doc)).toContain('<random flubble="huh" foo="1"/><foo bar="new first"/>');
  });
});

describe("structured value", () => {
  type Name = "NAME1" | "NAME2";
  const value = new DocumentBasedStructuredValue<Name, number>(INTEGER);
  const CONTAINER = firstTopLevel("container");
  const substrate = (values: readonly (readonly [Name, number])[]): string =>
    `<container${values.map(([n, v]) => ` ${n}="${v}"`).join("")}/>`;
  const get = (doc: StandaloneDocument, name: Name): number | null => {
    const { view, node } = at(doc, CONTAINER);
    return value.get(view, node, name);
  };
  const set = (doc: StandaloneDocument, name: Name, v: number | null): void => {
    const mutable = doc.mutable();
    value.set(mutable, CONTAINER(mutable.view()) as NodeId, name, v);
  };
  const setAll = (doc: StandaloneDocument, values: [Name, number | null][]): void => {
    const mutable = doc.mutable();
    value.setAll(mutable, CONTAINER(mutable.view()) as NodeId, values);
  };

  it("initialiser", () => {
    const init = value.createInitialiser([
      ["NAME1", 23],
      ["NAME2", 42],
    ]);
    expect([...buildAttributes(init)]).toEqual([
      ["NAME1", "23"],
      ["NAME2", "42"],
    ]);
    expect(buildAttributes(value.createInitialiser([["NAME1", null]])).size).toBe(0);
  });

  it("empty attribute is null", () => {
    expect(get(document(substrate([])), "NAME1")).toBeNull();
  });

  it("set on empty state", () => {
    let doc = document(substrate([]));
    set(doc, "NAME1", 42);
    expect(get(doc, "NAME1")).toBe(42);
    expect(get(doc, "NAME2")).toBeNull();
    doc = document(substrate([]));
    setAll(doc, [["NAME1", 42]]);
    expect(get(doc, "NAME1")).toBe(42);
    expect(get(doc, "NAME2")).toBeNull();
  });

  it("set on empty state inserts into substrate", () => {
    const both = [
      ["NAME1", 10],
      ["NAME2", 20],
    ] as const;
    let doc = document(substrate([]));
    set(doc, "NAME1", 10);
    expect(xml(doc)).toBe(substrate([["NAME1", 10]]));
    set(doc, "NAME2", 20);
    expect(xml(doc)).toBe(substrate(both));
    doc = document(substrate([]));
    setAll(doc, [["NAME1", 10]]);
    expect(xml(doc)).toBe(substrate([["NAME1", 10]]));
    setAll(doc, [["NAME2", 20]]);
    expect(xml(doc)).toBe(substrate(both));
  });

  it("substrate value is reflected", () => {
    const doc = document(substrate([["NAME1", 10]]));
    expect(get(doc, "NAME1")).toBe(10);
    expect(get(doc, "NAME2")).toBeNull();
  });

  it("set replaces value", () => {
    const before = [
      ["NAME1", 10],
      ["NAME2", 20],
    ] as const;
    const after = [
      ["NAME1", 5],
      ["NAME2", 20],
    ] as const;
    let doc = document(substrate(before));
    set(doc, "NAME1", 5);
    expect(get(doc, "NAME1")).toBe(5);
    expect(get(doc, "NAME2")).toBe(20);
    expect(xml(doc)).toBe(substrate(after));
    doc = document(substrate(before));
    setAll(doc, [["NAME1", 5]]);
    expect(xml(doc)).toBe(substrate(after));
  });

  it("set null clears value", () => {
    let doc = document(substrate([["NAME1", 10]]));
    set(doc, "NAME1", null);
    expect(get(doc, "NAME1")).toBeNull();
    expect(xml(doc)).toBe(substrate([]));
    doc = document(substrate([["NAME1", 10]]));
    setAll(doc, [["NAME1", null]]);
    expect(get(doc, "NAME1")).toBeNull();
    expect(xml(doc)).toBe(substrate([]));
  });

  it("remote set replaces value", () => {
    const doc = document(
      substrate([
        ["NAME1", 10],
        ["NAME2", 20],
      ]),
    );
    const mutable = doc.mutable();
    mutable.setElementAttribute(CONTAINER(mutable.view()) as NodeId, "NAME1", "5");
    expect(get(doc, "NAME1")).toBe(5);
    expect(get(doc, "NAME2")).toBe(20);
  });
});

describe("singleton", () => {
  const TARGET = new DocumentBasedSingleton("int");
  /** The value the original's integer factory adapts the element to. */
  const get = (doc: StandaloneDocument): number | null => {
    const { view, node } = at(doc, ROOT);
    const element = TARGET.get(view, node);
    expect(TARGET.hasValue(view, node)).toBe(element !== null);
    const attribute = element === null ? null : view.attribute(element, "v");
    return attribute === null ? null : INTEGER.fromAttribute(attribute);
  };
  const set = (doc: StandaloneDocument, value: string): void => {
    const mutable = doc.mutable();
    const element = TARGET.set(mutable, ROOT, new Map([["v", value]]));
    expect(mutable.view().attribute(element, "v")).toBe(value);
  };
  const clear = (doc: StandaloneDocument): void => TARGET.clear(doc.mutable(), ROOT);

  it("empty singleton has no value", () => {
    expect(get(document(""))).toBeNull();
  });

  it("set sets and replaces value", () => {
    const doc = document("");
    set(doc, "42");
    expect(get(doc)).toBe(42);
    set(doc, "43");
    expect(get(doc)).toBe(43);
    expect(xml(doc)).toBe('<int v="43"/>');
  });

  it("clear removes value, set after clear sets it", () => {
    const doc = document("");
    set(doc, "42");
    clear(doc);
    expect(get(doc)).toBeNull();
    set(doc, "43");
    expect(get(doc)).toBe(43);
  });

  it("clear empty does nothing", () => {
    const doc = document("");
    clear(doc);
    expect(doc.ops().length).toBe(0);
  });

  it("initial state is readable", () => {
    expect(get(document('<int v="42"/>'))).toBe(42);
  });

  it("the first element counts", () => {
    // A value inserted before the present one becomes the value.
    expect(get(document('<int v="43"/><int v="42"/>'))).toBe(43);
    expect(get(document('<int v="42"/><int v="43"/>'))).toBe(42);
  });

  it("clear removes redundant values", () => {
    const doc = document('<int v="42"/><other/><int v="43"/>');
    clear(doc);
    expect(get(doc)).toBeNull();
    expect(xml(doc)).toBe("<other/>");
  });

  it("set clears redundant values", () => {
    const doc = document('<int v="42"/><other><int v="45"/></other><int v="43"/>');
    set(doc, "44");
    expect(get(doc)).toBe(44);
    expect(xml(doc)).toBe('<int v="44"/><other/>');
  });
});
