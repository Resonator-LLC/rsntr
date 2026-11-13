// Scenarios of the document-based data types (`adtScenarios` in the
// generator; port of the Rust `supplement/golden_tests/adt.rs`).

import { describe, it } from "vitest";
import {
  DocumentBasedBasicMap,
  DocumentBasedBasicSet,
  DocumentBasedBasicValue,
  DocumentBasedBoolean,
  DocumentBasedElementList,
  DocumentBasedMonotonicMap,
  DocumentBasedMonotonicValue,
  DocumentBasedSingleton,
  DocumentBasedStructuredValue,
  INTEGER,
  ROOT as DOC_ROOT,
  STRING,
  buildAttributes,
  firstTopLevel,
  type InitialAttributes,
} from "../../adt";
import { MutableDocument, type DocView, type NodeId } from "../../doc";
import { ROOT, Wave, list, nullable } from "./goldenHarness";

const D = "d";

/** A step that edits the document `d` and may answer queries. */
function step(
  wave: Wave,
  label: string,
  edit: (doc: MutableDocument, answers: string[]) => void,
): void {
  wave.step(label, (w) => {
    const answers: string[] = [];
    try {
      edit(new MutableDocument(w.wavelet(ROOT).documentSink(D)), answers);
    } finally {
      for (const answer of answers) {
        w.q(answer);
      }
    }
  });
}

function append(wave: Wave, label: string, xml: string): void {
  wave.step(label, (w) => w.append(ROOT, D, xml));
}

const root = (doc: MutableDocument): NodeId => doc.view().root();

describe("adt golden", () => {
  it("adt-basic-map", () => {
    const MAP = new DocumentBasedBasicMap("e", "k", "v", STRING, STRING);
    const read = (doc: MutableDocument, keys: string[]): string =>
      [
        ...keys.map((key) => nullable(MAP.get(doc.view(), root(doc), key))),
        list(MAP.keySet(doc.view(), root(doc))),
      ].join(" ");
    const wave = new Wave();
    const put = (label: string, key: string, value: string): void =>
      step(wave, label, (doc, a) => void a.push(String(MAP.put(doc, DOC_ROOT, key, value))));
    put("put a", "a", "1");
    put("put b", "b", "2");
    put("put a same", "a", "1");
    put("put a other", "a", "3");
    step(wave, "read", (doc, a) => void a.push(read(doc, ["a", "b", "c"])));
    step(wave, "remove b", (doc) => MAP.remove(doc, DOC_ROOT, "b"));
    step(wave, "remove missing", (doc) => MAP.remove(doc, DOC_ROOT, "zz"));
    append(wave, "foreign content", '<e k="a" v="9"></e><x></x><e k="c" v="1"></e><e v="1"></e>');
    step(wave, "read again", (doc, a) => void a.push(read(doc, ["a", "c"])));
    put("~put c cleans up", "c", "2");
    step(wave, "read last", (doc, a) => void a.push(read(doc, ["a", "c"])));
    wave.check("adt-basic-map");
  });

  it("adt-monotonic-map", () => {
    const MAP = new DocumentBasedMonotonicMap("e", "k", "v", STRING, INTEGER);
    const read = (doc: MutableDocument, keys: string[]): string =>
      [
        ...keys.map((key) => nullable(MAP.get(doc.view(), root(doc), key))),
        list(MAP.keySet(doc.view(), root(doc))),
      ].join(" ");
    const wave = new Wave();
    const put = (label: string, key: string, value: number): void =>
      step(wave, label, (doc, a) => void a.push(String(MAP.put(doc, DOC_ROOT, key, value))));
    put("put a 5", "a", 5);
    put("put a 3", "a", 3);
    put("put a 5 again", "a", 5);
    put("put b 1", "b", 1);
    put("put a 7", "a", 7);
    step(wave, "read", (doc, a) => void a.push(read(doc, ["a", "b", "c"])));
    step(wave, "remove a", (doc) => MAP.remove(doc, DOC_ROOT, "a"));
    put("put a 2", "a", 2);
    append(wave, "foreign content", '<e k="b" v="4"></e><e k="b" v="4"></e>');
    step(wave, "read again", (doc, a) => void a.push(read(doc, ["a", "b"])));
    put("~put b 9 cleans up", "b", 9);
    wave.check("adt-monotonic-map");
  });

  it("adt-set", () => {
    const SET = new DocumentBasedBasicSet("f", "i", INTEGER);
    const wave = new Wave();
    step(wave, "add 5", (doc) => void SET.add(doc, DOC_ROOT, 5));
    step(wave, "add 7", (doc) => void SET.add(doc, DOC_ROOT, 7));
    step(wave, "add 5 again", (doc) => void SET.add(doc, DOC_ROOT, 5));
    step(wave, "remove 5", (doc) => void SET.remove(doc, DOC_ROOT, 5));
    step(wave, "remove missing", (doc) => void SET.remove(doc, DOC_ROOT, 6));
    append(wave, "foreign content", '<f i="7"></f><x></x><f i="2"></f>');
    step(wave, "read", (doc, a) => {
      const view = doc.view();
      const r = view.root();
      a.push(
        `${SET.contains(view, r, 7)} ${SET.contains(view, r, 2)} ${SET.contains(view, r, 5)} ` +
          list(SET.values(view, r)),
      );
    });
    step(wave, "add 9 cleans up", (doc) => void SET.add(doc, DOC_ROOT, 9));
    step(wave, "clear", (doc) => void SET.clear(doc, DOC_ROOT));
    step(wave, "clear empty", (doc) => void SET.clear(doc, DOC_ROOT));
    wave.check("adt-set");
  });

  it("adt-values", () => {
    const VALUE = new DocumentBasedMonotonicValue("m", "v", INTEGER);
    const FLAG = new DocumentBasedBoolean("b", "on");
    const read = (doc: MutableDocument, a: string[]): void => {
      const view = doc.view();
      a.push(`${nullable(VALUE.get(view, view.root()))} ${nullable(FLAG.get(view, view.root()))}`);
    };
    const wave = new Wave();
    const value = (label: string, v: number): void =>
      step(wave, label, (doc) => void VALUE.set(doc, DOC_ROOT, v));
    const flag = (label: string, v: boolean | null): void =>
      step(wave, label, (doc) => void FLAG.set(doc, DOC_ROOT, v));
    value("value 3", 3);
    value("value 2", 2);
    value("value 5", 5);
    flag("flag true", true);
    flag("flag true again", true);
    flag("flag false", false);
    step(wave, "read", read);
    flag("flag null", null);
    flag("flag null again", null);
    append(
      wave,
      "foreign content",
      '<b on="true"></b><b on="false"></b><b></b><m v="5"></m>',
    );
    step(wave, "read again", read);
    flag("flag false cleans up", false);
    value("~value 6 cleans up", 6);
    wave.check("adt-values");
  });

  /** The value the generator's factory adapts an element to. */
  const bar = (view: DocView, element: NodeId): string => nullable(view.attribute(element, "bar"));

  /** The attributes the generator's factory initialises an element with. */
  const barAttributes = (value: string | null): InitialAttributes =>
    buildAttributes(new DocumentBasedBasicValue("bar", STRING).createInitialiser(value));

  it("adt-list", () => {
    const LIST = new DocumentBasedElementList("foo");
    const container = firstTopLevel("c");
    const node = (doc: MutableDocument): NodeId => container(doc.view()) as NodeId;
    const wave = new Wave();
    append(wave, "container", "<x></x><c><y></y></c>");
    step(wave, "add x", (doc) => void LIST.add(doc, container, barAttributes("x")));
    step(wave, "add y", (doc) => void LIST.add(doc, container, barAttributes("y")));
    const addAt = (label: string, index: number, value: string | null): void =>
      step(wave, label, (doc) => void LIST.addAt(doc, container, index, barAttributes(value)));
    addAt("add z at 0", 0, "z");
    addAt("add null at 1", 1, null);
    addAt("add w at end", 4, "w");
    addAt("add beyond end", 6, "v");
    const indexOf = (doc: MutableDocument, value: string): number =>
      LIST.values(doc.view(), node(doc), bar).indexOf(value);
    step(wave, "read", (doc, a) => {
      const view = doc.view();
      const values = LIST.values(view, node(doc), bar);
      a.push(
        `${LIST.size(view, node(doc))} ${list(values)} ` +
          `${bar(view, LIST.get(view, node(doc), 2) as NodeId)} ${indexOf(doc, "y")}`,
      );
    });
    const remove = (label: string, value: string): void =>
      step(wave, label, (doc, a) => {
        const index = indexOf(doc, value);
        a.push(String(index >= 0 && LIST.remove(doc, container, index)));
      });
    remove("remove y", "y");
    remove("remove missing", "nope");
    step(wave, "clear", (doc) => LIST.clear(doc, container));
    wave.check("adt-list");
  });

  it("adt-singleton", () => {
    const SINGLE = new DocumentBasedSingleton("foo");
    const read = (doc: MutableDocument, a: string[]): void => {
      const view = doc.view();
      const element = SINGLE.get(view, view.root());
      a.push(
        `${SINGLE.hasValue(view, view.root())} ` +
          nullable(element === null ? null : bar(view, element)),
      );
    };
    const wave = new Wave();
    const set = (label: string, value: string): void =>
      step(wave, label, (doc, a) => {
        const element = SINGLE.set(doc, DOC_ROOT, barAttributes(value));
        a.push(bar(doc.view(), element));
      });
    step(wave, "read empty", read);
    set("set 1", "1");
    set("set 2", "2");
    append(wave, "foreign content", '<foo bar="7"></foo><w><foo bar="8"></foo></w>');
    step(wave, "read", read);
    set("set 3 cleans up", "3");
    append(wave, "more foreign content", '<foo bar="9"></foo>');
    step(wave, "clear", (doc) => SINGLE.clear(doc, DOC_ROOT));
    step(wave, "clear empty", (doc) => SINGLE.clear(doc, DOC_ROOT));
    wave.check("adt-singleton");
  });

  it("adt-structured", () => {
    const value = new DocumentBasedStructuredValue<"alpha" | "beta", number>(INTEGER);
    const BASIC = new DocumentBasedBasicValue("alpha", INTEGER);
    const element = (doc: MutableDocument): NodeId => firstTopLevel("s")(doc.view()) as NodeId;
    const read = (doc: MutableDocument, a: string[]): void => {
      const view = doc.view();
      const e = element(doc);
      a.push(
        `${nullable(value.get(view, e, "alpha"))} ${nullable(value.get(view, e, "beta"))} ` +
          nullable(BASIC.get(view, e)),
      );
    };
    const wave = new Wave();
    append(wave, "element", "<s></s>");
    step(wave, "set alpha", (doc) => value.set(doc, element(doc), "alpha", 5));
    step(wave, "set alpha same", (doc) => value.set(doc, element(doc), "alpha", 5));
    for (const label of ["set both", "set both same"]) {
      step(wave, label, (doc) =>
        value.setAll(doc, element(doc), [
          ["alpha", 6],
          ["beta", 7],
        ]),
      );
    }
    step(wave, "read", read);
    step(wave, "remove beta", (doc) => value.set(doc, element(doc), "beta", null));
    step(wave, "basic set", (doc) => BASIC.set(doc, element(doc), 9));
    step(wave, "basic set same", (doc) => BASIC.set(doc, element(doc), 9));
    step(wave, "basic remove", (doc) => BASIC.set(doc, element(doc), null));
    step(wave, "read again", read);
    wave.check("adt-structured");
  });
});
