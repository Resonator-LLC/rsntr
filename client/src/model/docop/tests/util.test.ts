// Port of `DocOpUtilTest`, the document test of
// `ModifiableDocumentTestBase` / `BootstrapDocumentTest`, and tests of the
// other helpers.

import { expect, test } from "vitest";

import {
  AnnotationBoundaryMap,
  Attributes,
  AttributesUpdate,
  BootstrapDocument,
  type DocOp,
  DocOpBuilder,
  NO_SCHEMA_CONSTRAINTS,
  ValidationResult,
  asInitialization,
  initialDocumentLength,
  normalizeInitialization,
  resultingDocumentLength,
  toConciseString,
  toXmlString,
  validateInitialization,
} from "..";
import {
  annotationEscape,
  annotationUnescape,
  xmlAttrEscape,
  xmlTextEscape,
  xmlTextUnescape,
} from "../util";
import {
  attrs,
  beginAnnotation,
  boundary,
  expectOperationError,
  finishAnnotation,
} from "./helpers";

/** The first part of the test document, shared by both Java tests. */
function testDocPrefix(text: string): DocOpBuilder {
  // Check the 3 things we need to escape.
  const link1 = (): AnnotationBoundaryMap => beginAnnotation("link", null, '12?"\\3');
  const link2 = beginAnnotation("link", null, "1");
  const ann = boundary(
    [],
    [
      ["x", null, "3"],
      ["y", null, "3"],
    ],
  );
  return (
    new DocOpBuilder()
      .elementStart("p", Attributes.EMPTY)
      .characters("hi ")
      .characters(text)
      .elementStart("q", attrs("a", "1"))
      // Check things we need to escape.
      .characters("<some>markup&")
      .elementEnd()
      // Check things we need to escape.
      .elementStart("r", attrs("a", "2", "b", "\\\"'"))
      .elementEnd()
      .elementStart("q", Attributes.EMPTY)
      .annotationBoundary(link1())
      .elementEnd()
      .elementStart("q", Attributes.EMPTY)
      .annotationBoundary(AnnotationBoundaryMap.empty())
      .elementEnd()
      .elementStart("q", Attributes.EMPTY)
      .annotationBoundary(link1())
      .elementEnd()
      .elementStart("r", Attributes.EMPTY)
      .annotationBoundary(link1())
      .elementEnd()
      .elementStart("r", Attributes.EMPTY)
      .annotationBoundary(link2)
      .elementEnd()
      .annotationBoundary(ann)
      .characters("abc")
  );
}

/** `DocOpUtilTest.TEST_DOC1`. */
function utilTestDoc(): DocOp {
  const linkNull = beginAnnotation("link", null, null);
  const change = boundary(
    ["x"],
    [
      ["xa", null, null],
      ["xb", null, "5"],
      ["xc", null, "6"],
      ["z", null, "4"],
      ["zz", null, "7"],
    ],
  );
  const finish = finishAnnotation("link", "xa", "xb", "xc", "y", "z", "zz");
  return testDocPrefix("there")
    .annotationBoundary(linkNull)
    .characters("def")
    .annotationBoundary(change)
    .characters("ghi")
    .annotationBoundary(finish)
    .elementEnd()
    .build();
}

/** `ModifiableDocumentTestBase.TEST_DOC1`. */
function documentTestDoc(): DocOp {
  const linkNull = finishAnnotation("link");
  const change = boundary(
    ["x"],
    [
      ["xb", null, "5"],
      ["xc", null, "6"],
      ["z", null, "4"],
      ["zz", null, "7"],
    ],
  );
  const finish = finishAnnotation("xb", "xc", "y", "z", "zz");
  return testDocPrefix("therW")
    .annotationBoundary(linkNull)
    .characters("def")
    .annotationBoundary(change)
    .characters("ghi")
    .annotationBoundary(finish)
    .elementEnd()
    .build();
}

function testDocXml(text: string): string {
  return (
    `<p>hi ${text}` +
    '<q a="1">&lt;some&gt;markup&amp;</q>' +
    '<r a="2" b="\\&quot;\'"/>' +
    '<q><?a "link"="12\\q\\"\\\\3"?></q>' +
    "<q/>" +
    "<q/>" +
    "<r/>" +
    '<r><?a "link"="1"?></r>' +
    '<?a "x"="3" "y"="3"?>abc' +
    '<?a "link"?>def' +
    '<?a "x" "xb"="5" "xc"="6" "z"="4" "zz"="7"?>ghi' +
    '<?a "xb" "xc" "y" "z" "zz"?>' +
    "</p>"
  );
}

test("xml string representation is minimal and normalised", () => {
  const init = asInitialization(utilTestDoc());
  expect(toXmlString(init)).toBe(testDocXml("there"));
});

/**
 * Checks the document's XML and that it survives a round trip through its
 * own initialization.
 */
function check(xml: string, doc: BootstrapDocument): void {
  expect(doc.toXmlString()).toBe(xml);
  const copy = new BootstrapDocument();
  copy.consume(doc.asOperation());
  expect(copy.toXmlString()).toBe(xml);
  expect(doc.equals(copy)).toBe(true);
}

// ModifiableDocumentTestBase.testDoc1
test("document applies operations", () => {
  const doc = new BootstrapDocument();
  doc.consume(documentTestDoc());
  check(testDocXml("therW"), doc);

  const op = new DocOpBuilder()
    .updateAttributes(AttributesUpdate.single("a", null, "2"))
    .deleteCharacters("hi t")
    .characters("you ")
    .retain(4)
    .replaceAttributes(attrs("a", "1"), attrs("b", "2"))
    .retain("<some>markup&".length + 1)
    .updateAttributes(AttributesUpdate.single("a", "2", "3"))
    .retain(2)
    .annotationBoundary(beginAnnotation("link", '12?"\\3', "blah"))
    .retain(3)
    .annotationBoundary(finishAnnotation("link"))
    .retain(16)
    .build();
  doc.consume(op);
  check(
    '<p a="2">you herW' +
      '<q b="2">&lt;some&gt;markup&amp;</q>' +
      '<r a="3" b="\\&quot;\'"/><q><?a "link"="blah"?>' +
      '</q><q/><?a "link"="12\\q\\"\\\\3"?>' +
      "<q/>" +
      "<r/>" +
      '<r><?a "link"="1"?></r>' +
      '<?a "x"="3" "y"="3"?>abc' +
      '<?a "link"?>def' +
      '<?a "x" "xb"="5" "xc"="6" "z"="4" "zz"="7"?>ghi' +
      '<?a "xb" "xc" "y" "z" "zz"?>' +
      "</p>",
    doc,
  );
});

test("document rejects invalid operations and stays unchanged", () => {
  const doc = new BootstrapDocument();
  doc.consume(new DocOpBuilder().characters("abc").build());
  const before = doc.clone();
  for (const op of [
    new DocOpBuilder().retain(2).build(),
    new DocOpBuilder().retain(4).build(),
    new DocOpBuilder().deleteCharacters("abd").build(),
    new DocOpBuilder().retain(3).elementStart("p", Attributes.EMPTY).buildUnchecked(),
  ]) {
    expectOperationError(() => doc.consume(op), "validation");
    expect(before.equals(doc)).toBe(true);
  }
});

test("document as operation is normalized and valid", () => {
  const doc = new BootstrapDocument();
  doc.consume(documentTestDoc());
  const init = doc.asOperation();
  expect(validateInitialization(null, NO_SCHEMA_CONSTRAINTS, init)).toBe(ValidationResult.Valid);
  expect(init.equals(normalizeInitialization(init))).toBe(true);
  expect(resultingDocumentLength(init)).toBe(doc.length());
  expect(initialDocumentLength(init)).toBe(0);
});

test("concise string", () => {
  const op = new DocOpBuilder()
    .retain(3)
    .characters('a"b\\')
    .deleteCharacters("x")
    .elementStart("p", attrs("a", "1", "b", "2"))
    .elementEnd()
    .deleteElementStart("q", Attributes.EMPTY)
    .deleteElementEnd()
    .replaceAttributes(Attributes.EMPTY, attrs("a", "1"))
    .updateAttributes(
      AttributesUpdate.fromTriples([
        ["a", "1", null],
        ["b", null, "2"],
      ]),
    )
    .annotationBoundary(beginAnnotation("k", null, "v"))
    .retain(1)
    .annotationBoundary(boundary(["k"], [["l", "1", null]]))
    .retain(1)
    .annotationBoundary(finishAnnotation("l"))
    .annotationBoundary(AnnotationBoundaryMap.empty())
    .buildUnchecked();
  expect(toConciseString(op)).toBe(
    '__3; ++"a\\"b\\\\"; --"x"; << p { a="1", b="2" }; >>; x< q {}; x>; ' +
      'r@ {} { a="1" }; u@ { a: "1" -> null, b: null -> "2" }; ' +
      '|| { "k": null -> "v" }; __1; || { "k", "l": "1" -> null }; __1; ' +
      '|| { "l" }; || {}; ',
  );
  expect(op.toString()).toBe(toConciseString(op));
});

test("document lengths", () => {
  const op = new DocOpBuilder()
    .retain(3)
    .characters("ab")
    .deleteCharacters("xyz")
    .elementStart("p", Attributes.EMPTY)
    .elementEnd()
    .deleteElementStart("q", Attributes.EMPTY)
    .deleteElementEnd()
    .replaceAttributes(Attributes.EMPTY, Attributes.EMPTY)
    .updateAttributes(AttributesUpdate.EMPTY)
    .buildUnchecked();
  expect(initialDocumentLength(op)).toBe(3 + 3 + 2 + 2);
  expect(resultingDocumentLength(op)).toBe(3 + 2 + 2 + 2);
});

test("initialization rejects other components", () => {
  const op = new DocOpBuilder().retain(1).build();
  const error = expectOperationError(() => asInitialization(op), "notAnInitialization");
  expect(error.detail).toEqual({ kind: "notAnInitialization", component: "retain" });
  new DocOpBuilder().characters("a").buildInitialization();
});

test("escapes round trip", () => {
  expect(xmlTextEscape("a&b<c>")).toBe("a&amp;b&lt;c&gt;");
  expect(xmlTextUnescape("a&amp;b&lt;c&gt;")).toBe("a&b<c>");
  expect(annotationEscape('a\\b"c?')).toBe('a\\\\b\\"c\\q');
  expect(annotationUnescape('a\\\\b\\"c\\q')).toBe('a\\b"c?');
  expect(xmlAttrEscape('"')).toBe("&quot;");
});
