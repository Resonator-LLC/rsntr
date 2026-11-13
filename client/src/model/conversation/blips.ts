// The structure of blip documents (`Blips`).
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::blips`), itself derived from
// `org.waveprotocol.wave.model.conversation.Blips` of Apache Wave, licensed
// under the Apache License, Version 2.0.

import { type DocView, LINE_TAGNAME, type NodeId, XmlFragment } from "../doc";

/** Prefix of the conversation model's annotation keys. */
export const ANNOTATION_PREFIX = "conv";
export const BODY_TAGNAME = "body";
export const HEAD_TAGNAME = "head";
/** The element in a blip that anchors an inline reply thread. */
export const THREAD_INLINE_ANCHOR_TAGNAME = "reply";
/** The attribute of an anchor holding the id of its thread. */
export const THREAD_INLINE_ANCHOR_ID_ATTR = "id";

/**
 * The content a new blip starts with: `<body><line/></body>`
 * (`Blips.INITIAL_CONTENT`).
 */
export function initialContent(): XmlFragment {
  return XmlFragment.empty().wrap(LINE_TAGNAME).wrap(BODY_TAGNAME);
}

/** The body element of a blip document (`Blips.getBody`). */
export function body(view: DocView): NodeId | null {
  return view.elementWithTagName(BODY_TAGNAME, view.root());
}
