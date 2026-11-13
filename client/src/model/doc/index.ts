// Reading a wavelet document as an element tree and editing it through
// DOM-like calls that emit document operations.
//
// The original model never builds `DocOp`s by hand: it edits a
// `MutableDocument`, which turns each edit into a non-invertible operation
// (`Nindo`) that the document converts into the `DocOp` sent to the server.
// This module does the same in three layers:
//
// * `DocView` is the element tree of a document at one moment, with
//   locations, plus the `DocHelper` queries the model needs;
// * `Nindo` is the non-invertible operation, and `Nindo.toDocOp` the
//   conversion (`IndexedDocumentImpl.NonInvertibleCursor`);
// * `MutableDocument` offers the edits (`MutableDocumentImpl`) over a
//   `DocumentSink`, which applies each resulting operation and passes it on.
//   `StandaloneDocument` is a sink for a document on its own.
//
// `XmlFragment` replaces `XmlStringBuilder`, and `lineContainers` holds the
// `LineContainers` helpers for blip bodies.
//
// This is a TypeScript port of the Rust crate `resonator-model` (module `doc`),
// itself derived from `org.waveprotocol.wave.model.document`
// (`MutableDocumentImpl`, `indexed.IndexedDocumentImpl`, `operation.Nindo`,
// `util.DocHelper`, `util.LineContainers`, `util.XmlStringBuilder`,
// `util.Annotations`) of Apache Wave, licensed under the Apache License,
// Version 2.0.
//
// Differences from the original:
//
// * There are no persistent node objects: a `NodeId` is valid only until the
//   next edit. Not ported: `moveSiblings`, `resetAnnotationsInRange`, local
//   (`@`) annotations, document event listeners, persistent views.
// * A `Nindo` is not validated against a schema before conversion; the sink
//   validates the converted operation against the document.
// * The operations emitted are normalized. The original's are not: how it
//   splits retains and deletions depends on the history of its DOM (adjacent
//   text nodes are never merged again). After normalization they are equal.
//
// Quirks kept from the original, all visible in the emitted operations:
//
// * Deleting content re-annotates the deleted items: for every annotation
//   key in use in the document (and every key the same operation has set on
//   something before), the deletion is wrapped in a change of that key from
//   its value on the deleted items to its value on the item to the left,
//   even where both are absent (`"k": null -> null`).
// * Setting an annotation to the value it already has still emits a change
//   (`"k": "v" -> "v"`).
// * An edit that changes nothing still emits an operation, see
//   `MutableDocument`.
//
// Locations are UTF-16 code units, i.e. native string offsets. An insertion
// inside a surrogate pair is rejected by the sink's validation
// (`DocError` of kind "operation"); a deletion that cuts one is rejected by
// the conversion (kind "invalidOperation").

export { DocError, type DocErrorDetail } from "./error";
export * as lineContainers from "./lineContainers";
export {
  LINE_TAGNAME,
  TOP_LEVEL_CONTAINER_TAGNAME,
  appendLine,
  appendToLastLine,
  isLineContainerTagname,
} from "./lineContainers";
export {
  AnnotationReader,
  MutableDocument,
  StandaloneDocument,
  annotationsOf,
  type DocumentSink,
  type RangedAnnotation,
} from "./mutable";
export {
  Nindo,
  NindoBuilder,
  attributesFromPairs,
  type NindoAttributeUpdate,
  type NindoComponent,
} from "./nindo";
export { DocView, Point, ROOT_TAG, type NodeId } from "./view";
export { XmlFragment, type XmlItem } from "./xmlFragment";
