// The tags of a conversation, kept in the `tags` data document as a list of
// `<tag>name</tag>` elements (`TagsDocument`).
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::tags`), itself derived from
// `org.waveprotocol.wave.model.conversation.TagsDocument` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { DocView, type MutableDocument, type NodeId, XmlFragment } from "../doc";
import type { BootstrapDocument } from "../docop";

export const TAG_TAGNAME = "tag";

/** The text a top-level node starts with, if any. */
function leadingText(view: DocView, node: NodeId): string | null {
  const first = view.firstChild(node);
  return first === null ? null : view.text(first);
}

/**
 * The tags of a tags document, in document order: the text of the first
 * child of every top-level node that starts with text. Nodes without such
 * text are skipped, as `TagsDocument.processInitialState` does.
 */
export function tags(doc: BootstrapDocument): string[] {
  const view = new DocView(doc);
  const result: string[] = [];
  for (const node of view.children(view.root())) {
    const text = leadingText(view, node);
    if (text !== null) {
      result.push(text);
    }
  }
  return result;
}

function xmlFor(tagName: string): XmlFragment {
  return XmlFragment.text(tagName).wrap(TAG_TAGNAME);
}

/** Appends a tag (`TagsDocument.addTag(String)`). Duplicates are not checked. */
export function addTag(doc: MutableDocument, tagName: string): void {
  doc.appendXml(xmlFor(tagName));
}

/**
 * Inserts a tag before the `index`th top-level node, or at the end if there
 * are not that many (`TagsDocument.addTag(String, int)`).
 */
export function insertTag(doc: MutableDocument, tagName: string, index: number): void {
  const view = doc.view();
  const node = view.children(view.root())[index];
  // The original takes the node only if it is an element.
  const location = node !== undefined && view.isElement(node) ? view.location(node) : view.size();
  doc.insertXml(location, xmlFor(tagName));
}

/**
 * Deletes the first tag with a name, if any (`TagsDocument.deleteTag`).
 * Returns whether one was deleted.
 */
export function deleteTag(doc: MutableDocument, tagName: string): boolean {
  const view = doc.view();
  const found = view
    .childElements(view.root())
    .find((element) => leadingText(view, element) === tagName);
  if (found === undefined) {
    return false;
  }
  doc.deleteNode(found);
  return true;
}

/**
 * Deletes the top-level elements that do not start with text, first to last
 * (the cleanup of `TagsDocument.processInitialState`).
 */
export function deleteEmptyTags(doc: MutableDocument): void {
  for (;;) {
    const view = doc.view();
    const empty = view
      .childElements(view.root())
      .find((element) => leadingText(view, element) === null);
    if (empty === undefined) {
      return;
    }
    doc.deleteNode(empty);
  }
}
