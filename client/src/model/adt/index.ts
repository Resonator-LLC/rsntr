// Data types stored in the elements and attributes of a document.
//
// The supplement and the conversation manifest keep their state in small
// XML documents through a handful of embeddings: a map is a list of
// `<entry key=".." value=".."/>` elements, a set a list of
// `<entry value=".."/>` elements, and so on. Each embedding tolerates the
// redundant elements that concurrent writers leave behind, and removes them
// the next time it writes.
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased`
// (`AbstractDocumentBasedMap`, `DocumentBasedBasicMap`,
// `DocumentBasedMonotonicMap`, `DocumentBasedBasicSet`,
// `DocumentBasedBasicValue`, `DocumentBasedMonotonicValue`,
// `DocumentBasedBoolean`, `DocumentBasedElementList`,
// `DocumentBasedSingleton`, `DocumentBasedStructuredValue`, `Initializer`,
// `CompoundInitializer`) and `org.waveprotocol.wave.model.util.Serializer`
// of Apache Wave, licensed under the Apache License, Version 2.0.
//
// Differences from the original:
//
// * The original types are live objects: each one caches what it found in
//   the document and keeps the cache current by listening to document
//   events. Here a type is only a description of the embedding (tag and
//   attribute names, serializers). Every read interprets the document as it
//   is, exactly as a freshly created original object would, and every write
//   does what that fresh object would do. Listeners and events are not
//   ported, nor are the in-memory implementations: use the standard
//   collections.
// * A node of a `DocView` does not survive an edit, so the element that
//   holds an embedding is named by a `Locator` wherever a write needs
//   several edits.
// * Where the original deletes redundant elements in the iteration order
//   of a hash set of document nodes, which is arbitrary, they are deleted
//   here in document order. `DocumentBasedBasicSet.clear` deletes the value
//   elements in ascending order of value. Key and value listings are
//   sorted.
// * An entry whose key or value attribute is missing or does not parse is
//   ignored: it is neither a value nor a redundant element, and is never
//   deleted. The original throws when it meets a value that does not parse.
// * Numbers are parsed with ASCII digits only; longs must be safe integers.
//
// Writes throw `AdtError`, or `DocError` when the document rejects an edit.

export { AdtError, type AdtErrorDetail } from "./error";
export {
  CompoundInitializer,
  buildAttributes,
  initialiseAttribute,
  type InitialAttributes,
  type Initializer,
} from "./initializer";
export { DocumentBasedElementList } from "./list";
export { ROOT, deleteNodes, firstTopLevel, locate, type Locator } from "./locator";
export { DocumentBasedBasicMap, DocumentBasedMonotonicMap } from "./map";
export {
  BOOLEAN,
  DOUBLE,
  HASHED_VERSION,
  INTEGER,
  LONG,
  STRING,
  WAVELET_ID,
  enumSerializer,
  isJavaLong,
  javaDoubleToString,
  parseJavaDouble,
  parseJavaInt,
  parseJavaLong,
  type Serializer,
} from "./serializer";
export { DocumentBasedBasicSet } from "./set";
export { DocumentBasedSingleton } from "./singleton";
export {
  DocumentBasedBasicValue,
  DocumentBasedBoolean,
  DocumentBasedMonotonicValue,
  DocumentBasedStructuredValue,
} from "./value";
