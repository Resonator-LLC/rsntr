// Tests of the document layer (port of the Rust `doc/tests.rs`). The
// conversion of edits to operations is checked against the original in the
// golden tests; these cover the tree view and the edges of the editing API.

import { expect, test } from "vitest";

import { Attributes, BootstrapDocument, toConciseString } from "../../docop";
import {
  DocError,
  type DocErrorDetail,
  DocView,
  Nindo,
  type NindoComponent,
  type NodeId,
  Point,
  ROOT_TAG,
  StandaloneDocument,
  XmlFragment,
  annotationsOf,
  lineContainers,
} from "..";

function doc(xml: string): StandaloneDocument {
  const d = new StandaloneDocument();
  d.mutable().insertXml(0, XmlFragment.parse(xml));
  d.takeOps();
  return d;
}

function xml(d: StandaloneDocument): string {
  return d.document().toXmlString();
}

/** Expects `f` to throw a `DocError`, of the given kind if any. */
function expectDocError(f: () => unknown, kind?: DocErrorDetail["kind"]): DocError {
  let thrown: unknown = null;
  try {
    f();
  } catch (e) {
    thrown = e;
  }
  expect(thrown).toBeInstanceOf(DocError);
  const error = thrown as DocError;
  if (kind !== undefined) {
    expect(error.kind).toBe(kind);
  }
  return error;
}

test("view structure and locations", () => {
  const d = doc('<a x="1">one<b>two</b></a>t<c/>');
  const view = new DocView(d.document());
  const root = view.root();
  expect(view.size()).toBe(13);
  expect(view.tagName(root)).toBe(ROOT_TAG);
  expect([view.contentStart(root), view.contentEnd(root)]).toEqual([0, 13]);
  expect(view.children(root).length).toBe(3);
  const [a, t, c] = view.children(root) as [NodeId, NodeId, NodeId];
  expect(view.tagName(a)).toBe("a");
  expect(view.attribute(a, "x")).toBe("1");
  expect(view.attribute(a, "y")).toBeNull();
  expect(view.text(t)).toBe("t");
  expect(view.isText(t) && !view.isElement(t)).toBe(true);
  expect(view.tagName(t)).toBeNull();
  expect([
    view.location(a),
    view.contentStart(a),
    view.contentEnd(a),
    view.locationAfter(a),
  ]).toEqual([0, 1, 9, 10]);
  expect([view.location(t), view.locationAfter(t)]).toEqual([10, 11]);
  expect([view.location(c), view.itemSize(c)]).toEqual([11, 2]);
  const b = view.elementWithTagName("b", root)!;
  expect(b).not.toBeNull();
  expect(view.elementWithTagName("b")).toBe(b);
  expect(view.parent(b)).toBe(a);
  expect(view.location(b)).toBe(4);
  expect(view.textContent(b)).toBe("two");
  expect(view.textContent(a)).toBe("onetwo");
  expect(view.nextSibling(a)).toBe(t);
  expect(view.nextSiblingElement(a)).toBe(c);
  expect(view.previousSibling(t)).toBe(a);
  expect(view.previousSibling(a)).toBeNull();
  expect(view.nextSibling(c)).toBeNull();
  expect(view.firstChildElement(root)).toBe(a);
  expect(view.lastChild(root)).toBe(c);
  expect(view.enclosingElement(view.firstChild(a)!)).toBe(a);
  expect(view.descendantElements(root)).toEqual([a, b, c]);
  expect(view.childElements(root)).toEqual([a, c]);
  expect(view.descendants(root).length).toBe(6);
  expect(view.textBetween(2, 12)).toBe("netwot");
  expect(view.textBetween(5, 5)).toBe("");
  expect(view.elementStartingAt(4)).toBe(b);
  expect(view.elementStartingAt(5)).toBeNull();
  expect(view.elementStartingAt(10)).toBeNull();
  expect(view.nodeStartingAt(10)).toBe(t);
  expect(view.elementAt(6)).toBe(b);
  expect(view.elementAt(10)).toBe(root);
  expect(view.elementAt(14)).toBeNull();
});

test("view locate round trips", () => {
  const d = doc("<a>one<b>two</b></a>t<c/>");
  const view = new DocView(d.document());
  for (let location = 0; location <= view.size(); location++) {
    const point = view.locate(location);
    expect(point).not.toBeNull();
    expect(view.pointLocation(point!), String(location)).toBe(location);
  }
  expect(view.locate(view.size() + 1)).toBeNull();
  const a = view.firstChild(view.root())!;
  const text = view.firstChild(a)!;
  expect(view.locate(2)).toEqual(Point.inText(text, 1));
  expect(view.locate(1)).toEqual(Point.inElement(a, text));
  expect(view.locate(9)).toEqual(Point.inElement(a, null));
  expect(Point.equals(view.locate(9)!, Point.inElement(a))).toBe(true);
  expect(Point.equals(view.locate(2)!, Point.inElement(a))).toBe(false);
  // Invalid points.
  expect(view.pointLocation(Point.inText(a, 0))).toBeNull();
  expect(view.pointLocation(Point.inText(text, 4))).toBeNull();
  expect(view.pointLocation(Point.inElement(view.root(), text))).toBeNull();
});

test("view merges text split by annotations", () => {
  const d = doc("<p>abcdef</p>");
  d.mutable().setAnnotation(3, 5, "k", "v");
  const view = new DocView(d.document());
  const p = view.firstChild(view.root())!;
  expect(view.children(p).length).toBe(1);
  expect(view.textContent(p)).toBe("abcdef");
});

test("empty document view", () => {
  const view = new DocView(new BootstrapDocument());
  expect(view.size()).toBe(0);
  expect(view.children(view.root()).length).toBe(0);
  expect(view.locate(0)).toEqual(Point.inElement(view.root(), null));
  expect(view.textBetween(0, 10)).toBe("");
});

test("xml fragment", () => {
  const fragment = XmlFragment.text("a<b").wrap("x").append(XmlFragment.text("c")).appendText("d");
  expect(fragment.itemCount()).toBe(7);
  expect(fragment.items.length).toBe(4);
  expect(XmlFragment.empty().isEmpty()).toBe(true);
  expect(XmlFragment.text("").itemCount()).toBe(0);
  const parsed = XmlFragment.parse(
    "<a x='1' y=\"&lt;&amp;&#65;&#x42;\" >t&gt;&quot;&apos;<b/></a >tail",
  );
  const d = new StandaloneDocument();
  d.mutable().appendXml(parsed);
  expect(xml(d)).toBe('<a x="1" y="<&AB">t&gt;"\'<b/></a>tail');
  expect(XmlFragment.fromInitialization(d.document().asOperation()).equals(parsed)).toBe(true);
  expect(parsed.equals(fragment)).toBe(false);
  for (const bad of [
    "<a>",
    "</a>",
    "<a></b>",
    "<a x=1/>",
    "<a x/>",
    '<a x="1/>',
    "<!-- c -->",
    "<?pi?>",
    "<a",
    "&bogus;",
    '<a x="1" x="2"/>',
    "<1a/>",
    "&#xD800;",
    "&#x110000;",
  ]) {
    expectDocError(() => XmlFragment.parse(bad), "invalidArgument");
  }
});

test("edits check their arguments", () => {
  const d = doc("<a>one</a>");
  const m = d.mutable();
  const root = m.view().root();
  const a = m.view().firstChild(root)!;
  const text = m.view().firstChild(a)!;
  expectDocError(() => m.insertText(6, "x"), "invalidLocation");
  expectDocError(() => m.insertText(-1, "x"), "invalidLocation");
  expectDocError(() => m.insertText(1.5, "x"), "invalidLocation");
  expectDocError(() => m.deleteRange(3, 2));
  expectDocError(() => m.deleteRange(0, 6));
  expectDocError(() => m.deleteNode(root), "invalidNode");
  expectDocError(() => m.deleteNode(text), "invalidNode");
  expectDocError(() => m.setElementAttribute(root, "x", "1"), "invalidNode");
  expectDocError(() => m.emptyElement(text), "invalidNode");
  expectDocError(() => m.createChildElement(text, "b", []), "invalidNode");
  expectDocError(() => m.setAnnotation(0, 2, "bad?key", "v"));
  expectDocError(() => m.setAnnotation(0, 2, "@local", "v"));
  expectDocError(() => m.setAnnotation(2, 9, "k", "v"));
  expect(xml(d)).toBe("<a>one</a>");
  expect(d.ops().length).toBe(0);
  // An edit the document rejects leaves it unchanged.
  expectDocError(() => d.mutable().consume(Nindo.builder().skip(1).elementEnd().build()));
  expectDocError(() => d.mutable().consume(Nindo.builder().deleteCharacters(1).build()));
  expectDocError(() =>
    d.mutable().consume(Nindo.builder().startAnnotation("k", "v").skip(2).build()),
  );
  expectDocError(() => d.mutable().consume(Nindo.builder().endAnnotation("k").build()));
  // An unbalanced insertion.
  expectDocError(
    () => d.mutable().consume(Nindo.builder().skip(1).elementStart("b").build()),
    "invalidOperation",
  );
  expect(xml(d)).toBe("<a>one</a>");
  expect(d.ops().length).toBe(0);
});

test("edits return new nodes", () => {
  const d = doc("<a>one</a>");
  const m = d.mutable();
  const b = m.createElement(2, "b", [
    ["k", "v"],
    ["j", "w"],
  ]);
  expect(m.view().tagName(b)).toBe("b");
  expect(m.view().location(b)).toBe(2);
  const a = m.view().firstChild(m.view().root())!;
  const c = m.createChildElement(a, "c");
  expect(m.view().location(c)).toBe(6);
  expect(m.insertXml(1, XmlFragment.text("t"))).toBeNull();
  const inserted = m.insertXml(0, XmlFragment.text("u").wrap("d"));
  expect(inserted).not.toBeNull();
  expect(m.view().tagName(inserted!)).toBe("d");
  expect(xml(d)).toBe('<d>u</d><a>to<b j="w" k="v"/>ne<c/></a>');
  expect(d.ops().length).toBe(4);
  expect(toConciseString(d.ops()[0]!)).toBe('__2; << b { j="w", k="v" }; >>; __3; ');
  expect(m.textContent()).toBe("utone");
  expect(m.textBetween(4, 6)).toBe("to");
});

test("guarded reset annotation", () => {
  const d = doc("<a>one</a>");
  expect(d.mutable().guardedResetAnnotation(1, 3, "k", "v")).toBe(true);
  expect(d.mutable().guardedResetAnnotation(1, 3, "k", "v")).toBe(false);
  expect(d.mutable().guardedResetAnnotation(2, 3, "k", "v")).toBe(true);
  expect(d.mutable().guardedResetAnnotation(2, 3, "k", "w")).toBe(true);
  expect(d.ops().length).toBe(3);
  expect(xml(d)).toBe('<a>o<?a "k"="w"?>n<?a "k"?>e</a>');
  // Resetting on an empty document does nothing.
  const empty = new StandaloneDocument();
  empty.mutable().resetAnnotation(0, 0, "k", "v");
  expect(empty.ops().length).toBe(0);
});

test("annotation reading", () => {
  const d = doc("<a>abcdefgh</a>");
  const m = d.mutable();
  m.setAnnotation(2, 6, "k", "v");
  m.setAnnotation(4, 8, "j", "w");
  expect(m.annotation(2, "k")).toBe("v");
  expect(m.annotation(6, "k")).toBeNull();
  expect(m.annotation(100, "k")).toBeNull();
  expect(m.findAnnotationChange(0, 10, "k", null)).toBe(2);
  expect(m.findAnnotationChange(2, 10, "k", "v")).toBe(6);
  expect(m.findAnnotationChange(2, 6, "k", "v")).toBeNull();
  expect(m.annotationKeys()).toEqual(["j", "k"]);
  expect(m.rangedAnnotations(3, 7).map((r) => [r.key, r.value, r.start, r.end])).toEqual([
    ["j", null, 0, 4],
    ["k", "v", 2, 6],
    ["j", "w", 4, 8],
    ["k", null, 6, 10],
  ]);
  expect(m.rangedAnnotations(2, 3, ["k"]).length).toBe(1);
  expect(m.rangedAnnotations(5, 5).length).toBe(0);
  // The reader over a plain document, and all annotations of an item.
  const reader = annotationsOf(d.document());
  expect(reader.annotationKeys()).toEqual(["j", "k"]);
  expect(reader.annotationsAt(5).entries).toEqual([
    { name: "j", value: "w" },
    { name: "k", value: "v" },
  ]);
  expect(reader.annotationsAt(100).isEmpty()).toBe(true);
});

test("nindo from doc op", () => {
  const d = doc("<a>one</a>");
  d.mutable().setAnnotation(1, 3, "k", "v");
  const op = d.ops()[0]!;
  const nindo = Nindo.fromDocOp(op, true);
  const expected: NindoComponent[] = [
    { type: "skip", count: 1 },
    { type: "startAnnotation", key: "k", value: "v" },
    { type: "skip", count: 2 },
    { type: "endAnnotation", key: "k" },
  ];
  expect(nindo.components).toEqual(expected);
  expect(nindo.equals(new Nindo(expected))).toBe(true);
  expect(nindo.equals(Nindo.fromDocOp(op, false))).toBe(false);
  expect(Nindo.fromDocOp(op, false).components.length).toBe(5);
  expect(Nindo.setAnnotation(3, 3, "k", null).components.length).toBe(0);
  // Replaying the non-invertible form on the changed document shows the
  // old values as they now are.
  d.mutable().consume(nindo);
  expect(toConciseString(d.ops()[1]!)).toBe('__1; || { "k": "v" -> "v" }; __2; || { "k" }; __2; ');
});

test("line container helpers", () => {
  const { appendLine, appendToLastLine, isLineContainerTagname } = lineContainers;
  const d = new StandaloneDocument();
  const m = d.mutable();
  const attributes = Attributes.fromPairs([["t", "h1"]]);
  let line = appendLine(m, XmlFragment.text("first"), attributes);
  expect(m.view().attribute(line, "t")).toBe("h1");
  line = appendLine(m, null, null);
  expect(m.view().location(line)).toBe(8);
  appendToLastLine(m, XmlFragment.text("second"));
  appendLine(m, XmlFragment.empty(), attributes);
  expect(xml(d)).toBe('<body><line t="h1"/>first<line/>second<line t="h1"/></body>');
  expect(isLineContainerTagname("body") && !isLineContainerTagname("line")).toBe(true);
  // Creating the container: content, then a replace of the attributes.
  expect(d.ops().map(toConciseString)).toEqual([
    "<< body {}; << line {}; >>; >>; ",
    '__3; ++"first"; __1; ',
    '__1; r@ {} { t="h1" }; __7; ',
    "__8; << line {}; >>; __1; ",
    '__10; ++"second"; __1; ',
    '__16; << line { t="h1" }; >>; __1; ',
  ]);
});

// Beyond the Rust tests: behaviour that only a UTF-16 string type exposes.

test("surrogate pairs are never split", () => {
  const d = doc("<a>x\u{1F600}y</a>");
  const m = d.mutable();
  expect(m.size()).toBe(6);
  // Inserting inside the pair: rejected by the sink's validation.
  expectDocError(() => m.insertText(3, "z"), "operation");
  // Deleting half of the pair: rejected by the conversion.
  expectDocError(() => m.deleteRange(2, 3), "invalidOperation");
  expectDocError(() => m.deleteRange(3, 5), "invalidOperation");
  // Annotating half of the pair.
  expectDocError(() => m.setAnnotation(1, 3, "k", "v"), "operation");
  // Inserting an unpaired surrogate.
  expectDocError(() => m.insertText(1, "\uD83D"), "operation");
  expect(xml(d)).toBe("<a>x\u{1F600}y</a>");
  expect(d.ops().length).toBe(0);
  // A bound inside the pair does not leak half of it.
  expect(m.textBetween(1, 3)).toBe("x�");
  expect(m.textBetween(1, 4)).toBe("x\u{1F600}");
  m.deleteRange(2, 4);
  expect(xml(d)).toBe("<a>xy</a>");
  expect(toConciseString(d.ops()[0]!)).toBe('__2; --"\u{1F600}"; __2; ');
});

test("view is a snapshot until refreshed", () => {
  const d = doc("<a>one</a>");
  const m = d.mutable();
  d.mutable().insertText(1, "X");
  expect(m.size()).toBe(5);
  m.refresh();
  expect(m.size()).toBe(6);
  expect(m.textContent()).toBe("Xone");
});
