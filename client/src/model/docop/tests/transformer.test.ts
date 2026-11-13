// Port of `DocOpTransformerTest` and `TransformerTest`: expected results of
// transforming specific pairs of operations. The cases were translated
// mechanically from the Java source.

import { expect, test } from "vitest";

import {
  Attributes,
  AttributesUpdate,
  type DocOp,
  DocOpBuilder,
  toConciseString,
  transform,
} from "..";
import { creator } from "../testing";
import {
  attrs,
  beginAnnotation,
  expectTransformError,
  finishAnnotation,
} from "./helpers";

function singleTest(
  client: DocOp,
  server: DocOp,
  transformedClient: DocOp,
  transformedServer: DocOp,
): void {
  const pair = transform(client, server);
  expect(toConciseString(pair.clientOp)).toBe(
    toConciseString(transformedClient),
  );
  expect(toConciseString(pair.serverOp)).toBe(
    toConciseString(transformedServer),
  );
}

function single(
  client: DocOp,
  server: DocOp,
  transformedClient: DocOp,
  transformedServer: DocOp,
): void {
  singleTest(client, server, transformedClient, transformedServer);
}

/** Also checks the transformation with client and server swapped. */
function reversible(
  client: DocOp,
  server: DocOp,
  transformedClient: DocOp,
  transformedServer: DocOp,
): void {
  singleTest(client, server, transformedClient, transformedServer);
  singleTest(server, client, transformedServer, transformedClient);
}

function sampleStructural(size: number, location: number): DocOp {
  return new DocOpBuilder()
    .retain(location)
    .elementStart("sampleElement", Attributes.EMPTY)
    .characters("sample text")
    .elementEnd()
    .retain(size - location)
    .build();
}

// TransformerTest.testClientOpLongerThanServerOp
test("client op longer than server op", () => {
  const client = new DocOpBuilder().retain(1).build();
  const server = new DocOpBuilder().build();
  expectTransformError(() => transform(client, server), "ranOut");
});

// Performs tests for transforming text deletions against text deletions.
test("delete vs delete", () => {
  // A's deletion spatially before B's deletion
  reversible(
    creator.deleteCharacters(20, 1, "abcde"),
    creator.deleteCharacters(20, 7, "fg"),
    creator.deleteCharacters(18, 1, "abcde"),
    creator.deleteCharacters(15, 2, "fg"),
  );
  // A's deletion spatially adjacent to and before B's deletion
  reversible(
    creator.deleteCharacters(20, 1, "abcde"),
    creator.deleteCharacters(20, 6, "fg"),
    creator.deleteCharacters(18, 1, "abcde"),
    creator.deleteCharacters(15, 1, "fg"),
  );
  // A's deletion overlaps B's deletion
  reversible(
    creator.deleteCharacters(20, 1, "abcde"),
    creator.deleteCharacters(20, 3, "cdefghi"),
    creator.deleteCharacters(13, 1, "ab"),
    creator.deleteCharacters(15, 1, "fghi"),
  );
  // A's deletion a subset of B's deletion
  reversible(
    creator.deleteCharacters(20, 1, "abcdefg"),
    creator.deleteCharacters(20, 3, "cd"),
    creator.deleteCharacters(18, 1, "abefg"),
    creator.identity(13),
  );
  // A's deletion identical to B's deletion
  reversible(
    creator.deleteCharacters(20, 1, "abcdefg"),
    creator.deleteCharacters(20, 1, "abcdefg"),
    creator.identity(13),
    creator.identity(13),
  );
});
// Performs tests for transforming text insertions against text deletions.
test("insert vs delete", () => {
  // A's insertion spatially before B's deletion
  reversible(
    creator.insertCharacters(20, 1, "abc"),
    creator.deleteCharacters(20, 2, "de"),
    creator.insertCharacters(18, 1, "abc"),
    creator.deleteCharacters(23, 5, "de"),
  );
  // A's insertion spatially inside B's deletion
  reversible(
    creator.insertCharacters(20, 2, "abc"),
    creator.deleteCharacters(20, 1, "ce"),
    creator.insertCharacters(18, 1, "abc"),
    new DocOpBuilder()
      .retain(1)
      .deleteCharacters("c")
      .retain(3)
      .deleteCharacters("e")
      .retain(17)
      .build(),
  );
  // A's insertion spatially at the start of B's deletion
  reversible(
    creator.insertCharacters(20, 1, "abc"),
    creator.deleteCharacters(20, 1, "de"),
    creator.insertCharacters(18, 1, "abc"),
    creator.deleteCharacters(23, 4, "de"),
  );
  // A's insertion spatially at the end of B's deletion
  reversible(
    creator.insertCharacters(20, 3, "abc"),
    creator.deleteCharacters(20, 1, "de"),
    creator.insertCharacters(18, 1, "abc"),
    creator.deleteCharacters(23, 1, "de"),
  );
  // A's insertion spatially after B's deletion
  reversible(
    creator.insertCharacters(20, 4, "abc"),
    creator.deleteCharacters(20, 1, "de"),
    creator.insertCharacters(18, 2, "abc"),
    creator.deleteCharacters(23, 1, "de"),
  );
});
// Performs tests for transforming text insertions against text insertions.
test("insert vs insert", () => {
  // A's insertion spatially before B's insertion
  reversible(
    creator.insertCharacters(20, 1, "a"),
    creator.insertCharacters(20, 2, "1"),
    creator.insertCharacters(21, 1, "a"),
    creator.insertCharacters(21, 3, "1"),
  );
  // client's insertion spatially at the same location as server's insertion
  single(
    creator.insertCharacters(20, 2, "abc"),
    creator.insertCharacters(20, 2, "123"),
    creator.insertCharacters(23, 2, "abc"),
    creator.insertCharacters(23, 5, "123"),
  );
});
// Performs tests for transforming element insertions against text insertions.
test("structural vs insert", () => {
  // A's insertion spatially before B's insertion
  reversible(
    creator.insertCharacters(20, 1, "a"),
    sampleStructural(20, 2),
    creator.insertCharacters(33, 1, "a"),
    sampleStructural(21, 3),
  );
  // A's insertion spatially after B's insertion
  reversible(
    creator.insertCharacters(20, 2, "a"),
    sampleStructural(20, 1),
    creator.insertCharacters(33, 15, "a"),
    sampleStructural(21, 1),
  );
  // client's insertion spatially at the same location as server's insertion
  single(
    creator.insertCharacters(20, 2, "a"),
    sampleStructural(20, 2),
    creator.insertCharacters(33, 2, "a"),
    sampleStructural(21, 3),
  );
  // client's insertion spatially at the same location as server's insertion
  single(
    sampleStructural(20, 2),
    creator.insertCharacters(20, 2, "a"),
    sampleStructural(21, 2),
    creator.insertCharacters(33, 15, "a"),
  );
});
// Performs tests for transforming element insertions against element
// insertions.
test("structural vs structural", () => {
  // A's insertion spatially before B's insertion
  reversible(
    sampleStructural(20, 1),
    sampleStructural(20, 2),
    sampleStructural(33, 1),
    sampleStructural(33, 15),
  );
  // client's insertion spatially at the same location as server's insertion
  single(
    sampleStructural(20, 2),
    sampleStructural(20, 2),
    sampleStructural(33, 2),
    sampleStructural(33, 15),
  );
});
// Performs tests for transforming attribute settings against text deletions.
test("attributes vs delete", () => {
  // A's attributes replacement spatially before B's deletion
  reversible(
    creator.replaceAttributes(
      20,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
    creator.replaceAttributes(
      17,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
  );
  // A's attributes replacement spatially adjacent to and before B's deletion
  reversible(
    creator.replaceAttributes(
      20,
      4,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
    creator.replaceAttributes(
      17,
      4,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
  );
  // A's attributes replacement spatially after B's deletion
  reversible(
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
    creator.replaceAttributes(
      17,
      6,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
  );
  // A's attributes replacement adjacent to and after B's deletion
  reversible(
    creator.replaceAttributes(
      20,
      8,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
    creator.replaceAttributes(
      17,
      5,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteCharacters(20, 5, "abc"),
  );
});
// Performs tests for transforming attribute settings against text insertions.
test("attributes vs insert", () => {
  // A's attributes replacement spatially before B's insertion
  reversible(
    creator.replaceAttributes(
      20,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 6, "hello"),
    creator.replaceAttributes(
      25,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 6, "hello"),
  );
  // A's attributes replacement spatially after B's insertion
  reversible(
    creator.replaceAttributes(
      20,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 2, "hello"),
    creator.replaceAttributes(
      25,
      8,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 2, "hello"),
  );
  // A's attributes replacement spatially adjacent to and after B's insertion
  reversible(
    creator.replaceAttributes(
      20,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 3, "hello"),
    creator.replaceAttributes(
      25,
      8,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 3, "hello"),
  );
  // A's attributes replacement spatially adjacent to and before B's insertion
  reversible(
    creator.replaceAttributes(
      20,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 4, "hello"),
    creator.replaceAttributes(
      25,
      3,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.insertCharacters(20, 4, "hello"),
  );
});
// Performs tests for transforming attribute settings against attribute
// settings.
test("attributes vs attributes", () => {
  // A's attributes replacement spatially before B's attributes replacement
  reversible(
    creator.replaceAttributes(
      20,
      5,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
    creator.replaceAttributes(
      20,
      5,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
  );
  // client's attributes replacement coincides with server's attributes replacement
  single(
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("name", "Google!"),
    ),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "Google!"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.identity(20),
  );
  // client's attributes replacement coincides with server's attributes replacement
  single(
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
    creator.replaceAttributes(
      20,
      9,
      attrs("href", "http://www.yahoo.com/"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.identity(20),
  );
});
// Performs tests for transforming an attribute setting against an element
// deletion.
test("attributes vs element deletion", () => {
  // A's attributes replacement coincides with B's element deletion.
  reversible(
    creator.replaceAttributes(
      20,
      6,
      attrs("name", "value"),
      attrs("href", "http://www.google.com/"),
    ),
    creator.deleteElement(20, 6, "type", attrs("name", "value")),
    creator.identity(18),
    creator.deleteElement(
      20,
      6,
      "type",
      attrs("href", "http://www.google.com/"),
    ),
  );
});
// Performs tests for transforming attribute settings against text deletions.
test("attribute vs delete", () => {
  // A's attribute update spatially before B's deletion.
  reversible(
    creator.setAttribute(20, 3, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
    creator.setAttribute(17, 3, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
  );
  // A's attribute update spatially adjacent to and before B's deletion.
  reversible(
    creator.setAttribute(20, 4, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
    creator.setAttribute(17, 4, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
  );
  // A's attribute update spatially after B's deletion.
  reversible(
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
    creator.setAttribute(17, 6, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
  );
  // A's attribute update spatially adjacent to and after B's deletion.
  reversible(
    creator.setAttribute(20, 8, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
    creator.setAttribute(17, 5, "href", "initial", "http://www.google.com/"),
    creator.deleteCharacters(20, 5, "abc"),
  );
});
// Performs tests for transforming attribute settings against text insertions.
test("attribute vs insert", () => {
  // A's attribute update spatially before B's insertion.
  reversible(
    creator.setAttribute(20, 3, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 6, "hello"),
    creator.setAttribute(25, 3, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 6, "hello"),
  );
  // A's attribute update spatially adjacent to and before B's insertion.
  reversible(
    creator.setAttribute(20, 3, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 4, "hello"),
    creator.setAttribute(25, 3, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 4, "hello"),
  );
  // A's attribute update spatially after B's insertion.
  reversible(
    creator.setAttribute(20, 3, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 2, "hello"),
    creator.setAttribute(25, 8, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 2, "hello"),
  );
  // A's attribute update spatially adjacent to and after B's insertion.
  reversible(
    creator.setAttribute(20, 3, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 3, "hello"),
    creator.setAttribute(25, 8, "href", "initial", "http://www.google.com/"),
    creator.insertCharacters(20, 3, "hello"),
  );
});
// Performs tests for transforming attribute settings against attribute
// settings.
test("attribute vs attributes", () => {
  // A's attribute update spatially before B's attributes replacement.
  reversible(
    creator.setAttribute(20, 5, "href", "initial", "http://www.google.com/"),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
    creator.setAttribute(20, 5, "href", "initial", "http://www.google.com/"),
    creator.replaceAttributes(
      20,
      9,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
  );
  // A's attribute update spatially after B's attributes replacement.
  reversible(
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.replaceAttributes(
      20,
      5,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.replaceAttributes(
      20,
      5,
      attrs("name", "value"),
      attrs("href", "http://www.yahoo.com/"),
    ),
  );
  // A's attribute update coincides with B's attributes replacement.
  reversible(
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.replaceAttributes(
      20,
      9,
      attrs("href", "initial"),
      attrs("name", "Google!"),
    ),
    creator.identity(20),
    creator.replaceAttributes(
      20,
      9,
      attrs("href", "http://www.google.com/"),
      attrs("name", "Google!"),
    ),
  );
  // A's attribute update coincides with B's attributes replacement.
  reversible(
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.replaceAttributes(
      20,
      9,
      attrs("href", "initial"),
      attrs("href", "http://www.yahoo.com/"),
    ),
    creator.identity(20),
    creator.replaceAttributes(
      20,
      9,
      attrs("href", "http://www.google.com/"),
      attrs("href", "http://www.yahoo.com/"),
    ),
  );
});
// Performs tests for transforming attribute settings against attribute
// settings.
test("attribute vs attribute", () => {
  // A's attribute update spatially before B's attribute update.
  reversible(
    creator.setAttribute(20, 5, "href", "initial", "http://www.google.com/"),
    creator.setAttribute(20, 9, "href", "initial", "http://www.yahoo.com/"),
    creator.setAttribute(20, 5, "href", "initial", "http://www.google.com/"),
    creator.setAttribute(20, 9, "href", "initial", "http://www.yahoo.com/"),
  );
  // A's attribute update has different key than B's attribute update.
  reversible(
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.setAttribute(20, 9, "name", "initial", "Google!"),
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.setAttribute(20, 9, "name", "initial", "Google!"),
  );
  // client's attribute update has same key as server's attribute update.
  single(
    creator.setAttribute(20, 9, "href", "initial", "http://www.google.com/"),
    creator.setAttribute(20, 9, "href", "initial", "http://www.yahoo.com/"),
    creator.setAttribute(
      20,
      9,
      "href",
      "http://www.yahoo.com/",
      "http://www.google.com/",
    ),
    new DocOpBuilder()
      .retain(9)
      .updateAttributes(AttributesUpdate.EMPTY)
      .retain(10)
      .build(),
  );
});
// Performs tests for transforming an attribute setting against an element
// deletion.
test("attribute vs element deletion", () => {
  // A's attribute update coincides with B's element deletion
  reversible(
    creator.setAttribute(20, 6, "href", "initial", "http://www.google.com/"),
    creator.deleteElement(20, 6, "type", attrs("href", "initial")),
    creator.identity(18),
    creator.deleteElement(
      20,
      6,
      "type",
      attrs("href", "http://www.google.com/"),
    ),
  );
});
// Performs tests for transforming a structural deletion against a text
// insertion.
test("structural deletion vs insert", () => {
  // A's deletion engulfs B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.insertCharacters(20, 6, "hello"),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hellocdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
  // A's deletion engulfs B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.insertCharacters(20, 7, "hello"),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("chellodefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
  // A's deletion engulfs B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.insertCharacters(20, 16, "hello"),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteCharacters("hello")
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
  // A's deletion spatially adjacent to and before B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.insertCharacters(20, 17, "hello"),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(8)
      .build(),
    creator.insertCharacters(5, 2, "hello"),
  );
  // A's deletion spatially before B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.insertCharacters(20, 18, "hello"),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(8)
      .build(),
    creator.insertCharacters(5, 3, "hello"),
  );
});
// Performs tests for transforming a structural deletion against an element
// insertion.
test("structural deletion vs structural", () => {
  // A's deletion engulfs B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    sampleStructural(20, 6),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteElementStart("sampleElement", Attributes.EMPTY)
      .deleteCharacters("sample text")
      .deleteElementEnd()
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
  // A's deletion engulfs B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    sampleStructural(20, 7),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("c")
      .deleteElementStart("sampleElement", Attributes.EMPTY)
      .deleteCharacters("sample text")
      .deleteElementEnd()
      .deleteCharacters("defg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
  // A's deletion engulfs B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    sampleStructural(20, 16),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementStart("sampleElement", Attributes.EMPTY)
      .deleteCharacters("sample text")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
  // A's deletion spatially adjacent to and before B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    sampleStructural(20, 17),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(16)
      .build(),
    sampleStructural(5, 2),
  );
  // A's deletion spatially before B's insertion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    sampleStructural(20, 18),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(16)
      .build(),
    sampleStructural(5, 3),
  );
});
// Performs tests for transforming structural deletions.
test("structural deletion transformations", () => {
  // A's deletion engulfs B's deletion
  reversible(
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    new DocOpBuilder()
      .retain(5)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("cdefg")
      .deleteElementEnd()
      .retain(8)
      .build(),
    new DocOpBuilder()
      .retain(2)
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("ab")
      .deleteElementStart("type", Attributes.EMPTY)
      .deleteCharacters("hi")
      .deleteElementEnd()
      .deleteElementEnd()
      .retain(3)
      .build(),
    creator.identity(5),
  );
});
// Performs tests for transforming annotation modifications against deletions.
test("annotation vs delete", () => {
  // A's annotation spatially before B's deletion
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 7, "ab"),
    creator.setAnnotation(18, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 7, "ab"),
  );
  // A's annotation spatially after B's deletion
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 1, "ab"),
    creator.setAnnotation(18, 2, 4, "hello", null, "world"),
    creator.deleteCharacters(20, 1, "ab"),
  );
  // A's annotation spatially adjacent to and before B's deletion
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 6, "abc"),
    creator.setAnnotation(17, 4, 6, "hello", null, "world"),
    new DocOpBuilder()
      .retain(6)
      .annotationBoundary(beginAnnotation("hello", null, "world"))
      .deleteCharacters("abc")
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
  );
  // A's annotation spatially adjacent to and after B's deletion
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 1, "abc"),
    creator.setAnnotation(17, 1, 3, "hello", null, "world"),
    creator.deleteCharacters(20, 1, "abc"),
  );
  // A's annotation overlaps B's deletion
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 1, "abcd"),
    creator.setAnnotation(16, 1, 2, "hello", null, "world"),
    new DocOpBuilder()
      .retain(1)
      .deleteCharacters("abc")
      .annotationBoundary(beginAnnotation("hello", "world", null))
      .deleteCharacters("d")
      .annotationBoundary(finishAnnotation("hello"))
      .retain(15)
      .build(),
  );
  // A's annotation overlaps B's deletion
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 5, "abcd"),
    creator.setAnnotation(16, 4, 5, "hello", null, "world"),
    new DocOpBuilder()
      .retain(5)
      .deleteCharacters("a")
      .annotationBoundary(beginAnnotation("hello", null, "world"))
      .deleteCharacters("bcd")
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
  );
  // A's annotation encloses B's deletion
  reversible(
    creator.setAnnotation(20, 2, 8, "hello", null, "world"),
    creator.deleteCharacters(20, 5, "ab"),
    creator.setAnnotation(18, 2, 6, "hello", null, "world"),
    creator.deleteCharacters(20, 5, "ab"),
  );
  // A's annotation inside B's deletion
  reversible(
    creator.setAnnotation(20, 5, 7, "hello", null, "world"),
    creator.deleteCharacters(20, 2, "abcdef"),
    creator.identity(14),
    new DocOpBuilder()
      .retain(2)
      .deleteCharacters("abc")
      .annotationBoundary(beginAnnotation("hello", "world", null))
      .deleteCharacters("de")
      .annotationBoundary(finishAnnotation("hello"))
      .deleteCharacters("f")
      .retain(12)
      .build(),
  );
  // A's operation clears an annotation
  reversible(
    creator.setAnnotation(20, 4, 6, "hello", "world", null),
    creator.deleteCharacters(20, 6, "abc"),
    creator.setAnnotation(17, 4, 6, "hello", "world", null),
    new DocOpBuilder()
      .retain(6)
      .annotationBoundary(beginAnnotation("hello", "world", null))
      .deleteCharacters("abc")
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
  );
});
// Performs tests for transforming annotation modifications against
// insertions.
test("annotation vs insert", () => {
  // A's annotation spatially after B's insertion
  reversible(
    creator.setAnnotation(20, 3, 5, "hello", null, "world"),
    creator.insertCharacters(20, 2, "abcd"),
    creator.setAnnotation(24, 7, 9, "hello", null, "world"),
    creator.insertCharacters(20, 2, "abcd"),
  );
  // A's annotation spatially before B's insertion
  reversible(
    creator.setAnnotation(20, 3, 5, "hello", null, "world"),
    creator.insertCharacters(20, 6, "abcd"),
    creator.setAnnotation(24, 3, 5, "hello", null, "world"),
    creator.insertCharacters(20, 6, "abcd"),
  );
  // A's annotation encloses B's insertion
  reversible(
    creator.setAnnotation(20, 3, 5, "hello", null, "world"),
    creator.insertCharacters(20, 4, "abcd"),
    creator.setAnnotation(24, 3, 9, "hello", null, "world"),
    creator.insertCharacters(20, 4, "abcd"),
  );
  // A's annotation spatially adjacent to and after B's insertion
  reversible(
    creator.setAnnotation(20, 3, 5, "hello", null, "world"),
    creator.insertCharacters(20, 3, "abcd"),
    creator.setAnnotation(24, 7, 9, "hello", null, "world"),
    creator.insertCharacters(20, 3, "abcd"),
  );
  // A's annotation spatially adjacent to and before B's insertion
  reversible(
    creator.setAnnotation(20, 3, 5, "hello", null, "world"),
    creator.insertCharacters(20, 5, "abcd"),
    creator.setAnnotation(24, 3, 9, "hello", null, "world"),
    creator.insertCharacters(20, 5, "abcd"),
  );
  // A's operation clears an annotation
  reversible(
    creator.setAnnotation(20, 3, 5, "hello", "world", null),
    creator.insertCharacters(20, 4, "abcd"),
    creator.setAnnotation(24, 3, 9, "hello", "world", null),
    creator.insertCharacters(20, 4, "abcd"),
  );
});
// Performs tests for transforming annotation modifications against annotation
// modifications.
test("annotation vs annotation", () => {
  // A's annotation overlaps B's annotation and has different key
  reversible(
    creator.setAnnotation(20, 2, 6, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 9, "hi", "initial", "there"),
    creator.setAnnotation(20, 2, 6, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 9, "hi", "initial", "there"),
  );
  // A's annotation overlaps B's annotation and has different key
  reversible(
    creator.setAnnotation(20, 2, 9, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 7, "hi", "initial", "there"),
    creator.setAnnotation(20, 2, 9, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 7, "hi", "initial", "there"),
  );
  // A's annotation spatially before B's annotation
  reversible(
    creator.setAnnotation(20, 2, 5, "hello", "initial", "world"),
    creator.setAnnotation(20, 6, 9, "hello", "initial", "there"),
    creator.setAnnotation(20, 2, 5, "hello", "initial", "world"),
    creator.setAnnotation(20, 6, 9, "hello", "initial", "there"),
  );
  // A's annotation spatially adjacent to and before B's annotation
  reversible(
    creator.setAnnotation(20, 2, 5, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 9, "hello", "initial", "there"),
    creator.setAnnotation(20, 2, 5, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 9, "hello", "initial", "there"),
  );
  // client's annotation overlaps server's annotation
  single(
    creator.setAnnotation(20, 2, 6, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 9, "hello", "initial", "there"),
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(3)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(1)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(14)
      .build(),
    creator.setAnnotation(20, 6, 9, "hello", "initial", "there"),
  );
  // client's annotation overlaps server's annotation
  single(
    creator.setAnnotation(20, 5, 9, "hello", "initial", "world"),
    creator.setAnnotation(20, 2, 6, "hello", "initial", "there"),
    new DocOpBuilder()
      .retain(5)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(1)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(3)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
    creator.setAnnotation(20, 2, 5, "hello", "initial", "there"),
  );
  // client's annotation encloses server's annotation
  single(
    creator.setAnnotation(20, 2, 9, "hello", "initial", "world"),
    creator.setAnnotation(20, 5, 7, "hello", "initial", "there"),
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(3)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(2)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
    creator.identity(20),
  );
  // client's annotation inside server's annotation
  single(
    creator.setAnnotation(20, 5, 7, "hello", "initial", "world"),
    creator.setAnnotation(20, 2, 9, "hello", "initial", "there"),
    creator.setAnnotation(20, 5, 7, "hello", "there", "world"),
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "there"))
      .retain(3)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "there"))
      .retain(2)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
  );
  // client's annotation overlaps server's incontiguous annotation
  single(
    creator.setAnnotation(20, 4, 8, "hello", "initial", "world"),
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "there"))
      .retain(3)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "there"))
      .retain(2)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
    new DocOpBuilder()
      .retain(4)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(1)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(1)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(12)
      .build(),
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "there"))
      .retain(2)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(4)
      .annotationBoundary(beginAnnotation("hello", "initial", "there"))
      .retain(1)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
  );
  // client's incontiguous annotation overlaps server's annotation
  single(
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(3)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(2)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
    creator.setAnnotation(20, 4, 8, "hello", "initial", "there"),
    new DocOpBuilder()
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(1)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(2)
      .annotationBoundary(beginAnnotation("hello", "there", "world"))
      .retain(1)
      .annotationBoundary(beginAnnotation("hello", "initial", "world"))
      .retain(1)
      .annotationBoundary(finishAnnotation("hello"))
      .retain(11)
      .build(),
    creator.setAnnotation(20, 5, 7, "hello", "initial", "there"),
  );
});
