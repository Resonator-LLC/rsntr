// Differential tests of the document layer against the original Java
// implementation (port of the doc scenarios of the Rust
// `model_golden_tests.rs` and `model_golden_tests/doc.rs`).
//
// `testdata/model_golden.txt` is written by
// `testdata/ModelGoldenGenerator.java`, which drives the reference classes
// through scripted scenarios. Each test here runs the same script against
// the port and expects the same records; the record format is described in
// the generator.
//
// The Rust harness edits documents of an `OpBasedWavelet`. The wavelet
// layer is outside this module, so here a `StandaloneDocument` per document
// stands in for it. That changes nothing in the operations or the document
// content; of the wavelet's own state in the records,
//
// * the wavelet version (`P` records) is checked as the number of
//   operations consumed so far, and the participant list as empty;
// * the document's last-modified version (third field of `D` records) is
//   NOT checked: it follows wavelet rules.
//
// The conversation, wavelet and digest scenarios of the same file are left
// to the modules they test; a test below pins down which ones those are.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

import {
  AnnotationBoundaryMap,
  Attributes,
  DocOpBuilder,
  toConciseString,
} from "../../docop";
import {
  type MutableDocument,
  Nindo,
  type NodeId,
  StandaloneDocument,
  XmlFragment,
  appendLine,
  appendToLastLine,
} from "..";
import { escape, lines } from "./goldenHelpers";

const GOLDEN = lines(
  readFileSync(
    fileURLToPath(
      new URL("../../../../../crates/resonator-model/testdata/model_golden.txt", import.meta.url),
    ),
    "utf8",
  ),
);

const DOC_SCENARIOS = ["doc-basic", "doc-delete-across-elements", "doc-nindo", "doc-unicode"];

/** The lines the generator wrote for a scenario, without the `N` line. */
function goldenScenario(name: string): string[] {
  const start = GOLDEN.indexOf(`N\t${name}`);
  expect(start, `no golden scenario ${name}`).toBeGreaterThanOrEqual(0);
  const result: string[] = [];
  for (let i = start + 1; i < GOLDEN.length && !GOLDEN[i]!.startsWith("N\t"); i++) {
    result.push(GOLDEN[i]!);
  }
  expect(result.length).toBeGreaterThan(0);
  return result;
}

/** Drops the last-modified version of a `D` record, see the header. */
function comparable(line: string): string {
  const fields = line.split("\t");
  if (fields[0] === "D") {
    fields[3] = "*";
  }
  return fields.join("\t");
}

/** The generator's `Wave`, reduced to documents that record their ops. */
class Wave {
  private readonly wavelets = new Map<string, Map<string, StandaloneDocument>>();
  private readonly versions = new Map<string, number>();
  private readonly records: string[] = [];
  readonly counts = { S: 0, O: 0, E: 0, D: 0, P: 0 };

  private record(...fields: string[]): void {
    this.counts[fields[0] as keyof Wave["counts"]]++;
    this.records.push(fields.map(escape).join("\t"));
  }

  private document(wavelet: string, doc: string): StandaloneDocument {
    let docs = this.wavelets.get(wavelet);
    if (docs === undefined) {
      docs = new Map();
      this.wavelets.set(wavelet, docs);
      this.versions.set(wavelet, 0);
    }
    let document = docs.get(doc);
    if (document === undefined) {
      document = new StandaloneDocument();
      docs.set(doc, document);
    }
    return document;
  }

  edit(wavelet: string, doc: string, edit: (doc: MutableDocument) => void): void {
    edit(this.document(wavelet, doc).mutable());
  }

  step(label: string, step: (wave: Wave) => void): void {
    this.record("S", label);
    let failed = false;
    try {
      step(this);
    } catch {
      failed = true;
    }
    for (const id of [...this.wavelets.keys()].sort()) {
      for (const [docId, document] of this.wavelets.get(id)!) {
        for (const op of document.takeOps()) {
          this.record("O", id, `doc ${docId} ${toConciseString(op)}`);
          this.versions.set(id, this.versions.get(id)! + 1);
        }
      }
    }
    if (failed) {
      this.record("E");
    }
    for (const id of [...this.wavelets.keys()].sort()) {
      for (const [docId, document] of this.wavelets.get(id)!) {
        const xml = document.document().toXmlString();
        if (xml !== "") {
          this.record("D", id, docId, "*", xml);
        }
      }
      this.record("P", id, String(this.versions.get(id)), "");
    }
  }

  /** Compares the records produced with the golden ones of a scenario. */
  check(scenario: string): Wave["counts"] {
    const golden = goldenScenario(scenario);
    let step = "";
    golden.forEach((expected, index) => {
      if (expected.startsWith("S\t")) {
        step = expected;
      }
      const actual = this.records[index];
      if (actual !== comparable(expected)) {
        expect(actual, `scenario ${scenario}, record ${index}, in step ${step}`).toBe(
          comparable(expected),
        );
      }
    });
    expect(this.records.length, `scenario ${scenario}: extra records`).toBe(golden.length);
    return this.counts;
  }
}

const W = "conv+root";
const D = "b+doc";

function elementAt(doc: MutableDocument, location: number): NodeId {
  const element = doc.view().elementStartingAt(location);
  expect(element).not.toBeNull();
  return element!;
}

test("doc-basic", () => {
  const wave = new Wave();
  const body = XmlFragment.empty().wrap("line").wrap("body");
  const edit = (label: string, f: (doc: MutableDocument) => void): void =>
    wave.step(label, (w) => w.edit(W, D, f));
  edit("insert body", (doc) => doc.insertXml(doc.size(), body));
  edit("insert text", (doc) => doc.insertText(3, "Hello world"));
  edit("bold", (doc) => doc.setAnnotation(3, 8, "style/fontWeight", "bold"));
  edit("link", (doc) => doc.setAnnotation(5, 12, "link/manual", "http://x"));
  edit("insert in annotated", (doc) => doc.insertText(6, "XY"));
  edit("insert at annotation end", (doc) => doc.insertText(10, "Z"));
  edit("create reply anchor", (doc) => doc.createElement(9, "reply", [["id", "t1"]]));
  edit("set attribute", (doc) => doc.setElementAttribute(elementAt(doc, 9), "id", "t2"));
  edit("set same attribute", (doc) => doc.setElementAttribute(elementAt(doc, 9), "id", "t2"));
  edit("update attributes", (doc) =>
    doc.updateElementAttributes(elementAt(doc, 9), [
      ["a", "1"],
      ["id", null],
      ["zz", "z"],
    ]),
  );
  edit("replace attributes", (doc) =>
    doc.setElementAttributes(
      elementAt(doc, 9),
      Attributes.fromPairs([
        ["z", "9"],
        ["b", "2"],
      ]),
    ),
  );
  edit("unbold part", (doc) => doc.setAnnotation(4, 7, "style/fontWeight", null));
  edit("rebold overlapping", (doc) => doc.setAnnotation(2, 13, "style/fontWeight", "bold"));
  edit("set same annotation", (doc) => doc.setAnnotation(3, 6, "style/fontWeight", "bold"));
  edit("delete range over annotations", (doc) => doc.deleteRange(4, 13));
  edit("explicit title", (doc) => doc.resetAnnotation(0, 1, "conv/title", "Title"));
  edit("implicit title", (doc) => doc.resetAnnotation(1, 6, "conv/title", ""));
  edit("append line", (doc) =>
    appendLine(doc, XmlFragment.text("second < line & more"), Attributes.EMPTY),
  );
  edit("append to last line", (doc) => appendToLastLine(doc, XmlFragment.text(" tail")));
  edit("insert empty text", (doc) => doc.insertText(4, ""));
  edit("delete empty range", (doc) => doc.deleteRange(4, 4));
  edit("delete line element", (doc) => doc.deleteNode(elementAt(doc, 1)));
  edit("delete all text", (doc) => doc.deleteRange(1, doc.size() - 1));
  edit("empty document", (doc) => doc.emptyElement(doc.view().root()));
  edit("append line to empty", (doc) =>
    appendLine(doc, XmlFragment.text("fresh"), Attributes.EMPTY),
  );
  edit("append to last line of empty", (doc) => {
    doc.emptyElement(doc.view().root());
    appendToLastLine(doc, XmlFragment.text("x"));
  });
  edit("bad location", (doc) => doc.insertText(1000, "x"));
  expect(wave.check("doc-basic")).toEqual({ S: 27, O: 30, E: 1, D: 26, P: 27 });
});

test("doc-delete-across-elements", () => {
  const wave = new Wave();
  const edit = (label: string, f: (doc: MutableDocument) => void): void =>
    wave.step(label, (w) => w.edit(W, D, f));
  edit("build", (doc) =>
    doc.insertXml(
      doc.size(),
      XmlFragment.parse(
        "<a>one<b>two<c>three</c>four</b>five</a><d>six<e>seven</e>eight</d><f>nine</f>",
      ),
    ),
  );
  edit("annotate", (doc) => doc.setAnnotation(6, 30, "k", "v"));
  edit("annotate 2", (doc) => doc.setAnnotation(10, 20, "j", "w"));
  edit("delete c to e", (doc) => doc.deleteRange(11, 36));
  edit("delete a to f", (doc) => doc.deleteRange(2, doc.size() - 3));
  edit("delete into child", (doc) => doc.deleteRange(1, 5));
  expect(wave.check("doc-delete-across-elements")).toEqual({ S: 6, O: 14, E: 0, D: 6, P: 6 });
});

/** `TitleHelper.emptyDocumentWithTitle` (conversation layer), inlined. */
function emptyDocumentWithTitle() {
  return new DocOpBuilder()
    .elementStart("body", Attributes.EMPTY)
    .annotationBoundary(AnnotationBoundaryMap.changesOnly([["conv/title", null, ""]]))
    .elementStart("line", Attributes.EMPTY)
    .elementEnd()
    .elementEnd()
    .annotationBoundary(AnnotationBoundaryMap.ends(["conv/title"]))
    .buildInitialization();
}

test("doc-nindo", () => {
  const wave = new Wave();
  const edit = (label: string, f: (doc: MutableDocument) => void): void =>
    wave.step(label, (w) => w.edit(W, D, f));
  edit("initial content with annotations", (doc) =>
    doc.consume(Nindo.fromDocOp(emptyDocumentWithTitle().asOp(), false)),
  );
  edit("text", (doc) => doc.insertText(3, "abcdefghij"));
  edit("mixed", (doc) => {
    const b = Nindo.builder();
    b.skip(4);
    b.startAnnotation("x", "1");
    b.skip(2);
    b.characters("NEW");
    b.skip(1);
    b.endAnnotation("x");
    b.deleteCharacters(2);
    b.startAnnotation("y", "2");
    b.skip(2);
    b.endAnnotation("y");
    doc.consume(b.build());
  });
  edit("delete over three annotations", (doc) => doc.deleteRange(2, 12));
  edit("remove annotation everywhere", (doc) => doc.setAnnotation(0, doc.size(), "x", null));
  edit("delete after key removed", (doc) => doc.deleteRange(3, 4));
  expect(wave.check("doc-nindo")).toEqual({ S: 6, O: 7, E: 0, D: 6, P: 6 });
});

test("doc-unicode", () => {
  const wave = new Wave();
  const body = XmlFragment.empty().wrap("line").wrap("body");
  const edit = (label: string, f: (doc: MutableDocument) => void): void =>
    wave.step(label, (w) => w.edit(W, D, f));
  edit("build", (doc) => doc.insertXml(doc.size(), body));
  edit("text", (doc) => doc.insertText(3, "héllo 世界 \"q\" 'a' <&>"));
  edit("annotate", (doc) => doc.setAnnotation(4, 9, "kéy", 'väl"ue'));
  edit("delete", (doc) => doc.deleteRange(5, 11));
  expect(wave.check("doc-unicode")).toEqual({ S: 4, O: 4, E: 0, D: 4, P: 4 });
});

test("the remaining scenarios are the ones left to other modules", () => {
  const names = GOLDEN.filter((line) => line.startsWith("N\t")).map((line) => line.slice(2));
  expect(names).toEqual([
    ...DOC_SCENARIOS,
    "conv-basic",
    "conv-delete-many",
    "conv-anchor",
    "wavelet-authorise",
    "digest",
    "digest-conversation",
    "digest-other-conversation",
  ]);
  const docRecords = DOC_SCENARIOS.reduce((sum, name) => sum + goldenScenario(name).length, 0);
  expect(docRecords).toBe(184);
});
