// The conversation model: the threads and blips of a wave's conversations,
// read from and written to wavelets, with change notifications.
//
// A conversation lives in a conversational wavelet (`conv+root` or
// `conv+<token>`). Its structure is in the manifest document `conversation`
// (see `manifest`); each blip's content is a document of its own, named by
// the blip id; a blip's inline replies are anchored by `reply` elements in
// its content; tags are in the `tags` document; the title is an annotation
// on the first blip of the root thread (see `title`).
//
// * `Conversation` is a read-only snapshot over a `WaveletData`: root
//   thread, blips, reply threads, participants, blip metadata, traversal,
//   title, tags.
// * `WaveletBasedConversation` mutates a conversation through an
//   `OpBasedWavelet`, by thread and blip id.
// * `ObservableConversation` reports the changes of a conversation, local
//   and remote, as id-based events.
// * `WaveBasedConversationView` is the set of conversations of a wave: it
//   creates new ones, hands out the above, and tells when conversations
//   come and go.
//
// A TypeScript port of the Rust crate `resonator-model` (module `conversation`),
// itself derived from `org.waveprotocol.wave.model.conversation` of Apache
// Wave (`WaveBasedConversationView`, `WaveletBasedConversation`,
// `WaveletBasedConversationThread`, `WaveletBasedConversationBlip`,
// `DocumentBasedManifest`, `DocumentBasedManifestThread`,
// `DocumentBasedManifestBlip`, `AnchorData`, `BlipIterators`, `Blips`,
// `TitleHelper`, `TagsDocument`, `AnnotationConstants`, and for the events
// `ObservableConversation`, `ObservableConversationView`), licensed under
// the Apache License, Version 2.0.
//
// Differences from the original:
//
// * No object graph: reads are snapshots, mutations and events name
//   threads and blips by id. A deleted thread or blip is "not found" rather
//   than an unusable object.
// * Deleting a blip or thread walks children in manifest order where the
//   original walks a hash map; the operations are the same, their order can
//   differ when a blip has several reply threads or a thread several blips.
// * Structure events are derived by comparing the structure before and
//   after a change, see `ObservableConversation`.
// * Not ported: `ConversationCopier`, `AnchorManager`, `BlipMappers`,
//   `ConversationStructure` (client-side helpers).

export * as annotations from "./annotations";
export * as blips from "./blips";
export { ConversationError, type ConversationErrorDetail } from "./error";
export * as manifest from "./manifest";
export {
  Manifest,
  type AnchorData,
  type ManifestBlip,
  type ManifestThread,
} from "./manifest";
export {
  Conversation,
  ConversationBlip,
  ConversationThread,
  conversationIdFor,
  findAnchors,
  waveletIdFor,
  type LocatedReplyThread,
} from "./model";
export {
  ObservableConversation,
  type ConversationEvents,
  type ObservableConversationOptions,
} from "./observable";
export * as tags from "./tags";
export * as title from "./title";
export {
  WaveBasedConversationView,
  type ConversationViewEvents,
  type ConversationViewOptions,
} from "./view";
export {
  WaveletBasedConversation,
  makeWaveletConversational,
  readManifest,
  waveletHasConversation,
  type BlipIdGenerator,
} from "./waveletBased";
