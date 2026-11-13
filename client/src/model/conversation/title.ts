// The title of a wave, kept as an annotation on its root blip
// (`TitleHelper`).
//
// The annotation `conv/title` either carries the title itself (an explicit
// title, set on the first item of the document) or is the empty string over
// the range of text that is the title (an implicit title: normally the
// first line or sentence).
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::title`), itself derived from
// `org.waveprotocol.wave.model.conversation.TitleHelper` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { DocError, DocView, LINE_TAGNAME, type MutableDocument } from "../doc";
import {
  AnnotationBoundaryMap,
  Attributes,
  type BootstrapDocument,
  type DocInitialization,
  DocOpBuilder,
} from "../docop";
import * as blips from "./blips";

/** The annotation key of the title. */
export const TITLE_KEY = "conv/title";

/** The annotation value that marks a range as an implicit title. */
export const AUTO_VALUE = "";

/**
 * A blip document with one empty line that is marked as implicit title:
 * `<body><line/></body>` with `conv/title=""` from inside the body to the
 * end of the document (`TitleHelper.emptyDocumentWithTitle`).
 */
export function emptyDocumentWithTitle(): DocInitialization {
  return new DocOpBuilder()
    .elementStart(blips.BODY_TAGNAME, Attributes.EMPTY)
    .annotationBoundary(AnnotationBoundaryMap.changesOnly([[TITLE_KEY, null, AUTO_VALUE]]))
    .elementStart(LINE_TAGNAME, Attributes.EMPTY)
    .elementEnd()
    .elementEnd()
    .annotationBoundary(AnnotationBoundaryMap.ends([TITLE_KEY]))
    .buildInitialization();
}

/**
 * The title of a document: the explicit title if there is one, else the
 * text of the implicit title range, else the empty string
 * (`TitleHelper.extractTitle`).
 */
export function extractTitle(doc: BootstrapDocument): string {
  const size = doc.length();
  const start = doc.firstAnnotationChange(0, size, TITLE_KEY, null);
  if (start === null) {
    return "";
  }
  const explicit = doc.getAnnotation(start, TITLE_KEY);
  if (explicit !== null && explicit !== "") {
    return explicit;
  }
  const end = doc.firstAnnotationChange(start, size, TITLE_KEY, AUTO_VALUE) ?? size;
  return new DocView(doc).textBetween(start, end);
}

/**
 * Whether the document has a non-empty explicit title
 * (`TitleHelper.hasExplicitTitle`).
 */
export function hasExplicitTitle(doc: BootstrapDocument): boolean {
  const start = doc.firstAnnotationChange(0, doc.length(), TITLE_KEY, null);
  if (start === null) {
    return false;
  }
  const value = doc.getAnnotation(start, TITLE_KEY);
  return value !== null && value !== "";
}

/**
 * Sets an explicit title, replacing any title there was
 * (`TitleHelper.setExplicitTitle`). The document must not be empty.
 */
export function setExplicitTitle(doc: MutableDocument, title: string): void {
  if (doc.size() === 0) {
    throw DocError.invalidArgument("Cannot set title on empty document");
  }
  doc.resetAnnotation(0, 1, TITLE_KEY, title);
}

/**
 * Marks a range as the implicit title, unless it already is
 * (`TitleHelper.setImplicitTitle`).
 */
export function setImplicitTitle(doc: MutableDocument, start: number, end: number): void {
  if (!(start < end)) {
    throw DocError.invalidArgument("Implicit title range is invalid");
  }
  doc.guardedResetAnnotation(start, end, TITLE_KEY, AUTO_VALUE);
}

const PERIOD = 0x2e;
const EXCLAMATION = 0x21;
const QUESTION = 0x3f;
const SPACE = 0x20;

/**
 * The range an implicit title should cover: from the first line element of
 * the body to the end of the first sentence (the whitespace after `.`, `!`
 * or `?`) or the start of the second line, whichever comes first, or else
 * almost to the end of the document (`TitleHelper.findImplicitTitle`).
 */
export function findImplicitTitle(view: DocView): [start: number, end: number] | null {
  const body = blips.body(view);
  if (body === null || view.size() === 0) {
    return null;
  }
  let afterPunctuation = false;
  let start: number | null = null;
  let end = view.size() - 1;
  // From the body to the end of the document, as the original's iteration
  // is not confined to the body.
  const nodes = view.descendants(view.root());
  outer: for (let i = nodes.indexOf(body); i >= 0 && i < nodes.length; i++) {
    const node = nodes[i]!;
    const text = view.text(node);
    if (text !== null) {
      if (start === null) {
        continue;
      }
      for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index);
        if (unit === PERIOD || unit === EXCLAMATION || unit === QUESTION) {
          afterPunctuation = true;
        } else if (afterPunctuation && unit === SPACE) {
          end = view.location(node) + index;
          break outer;
        } else {
          afterPunctuation = false;
        }
      }
    } else if (view.hasTag(node, LINE_TAGNAME)) {
      if (start !== null) {
        end = view.location(node);
        break;
      }
      start = view.location(node);
    }
  }
  if (start === null) {
    return null;
  }
  return start > 0 && end > start ? [start, end] : null;
}

/**
 * Sets the implicit title to the range {@link findImplicitTitle} finds,
 * unless the document has an explicit title
 * (`TitleHelper.maybeFindAndSetImplicitTitle`).
 */
export function maybeFindAndSetImplicitTitle(doc: MutableDocument): void {
  if (hasExplicitTitle(doc.document())) {
    return;
  }
  const range = findImplicitTitle(doc.view());
  if (range !== null) {
    setImplicitTitle(doc, range[0], range[1]);
  }
}
