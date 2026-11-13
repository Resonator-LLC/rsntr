// Names of the documents, elements and attributes of a user-data wavelet
// (the constants of the Rust `supplement/wavelet_based.rs`; a separate
// module here so that the schemas can use them without the supplement).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`WaveletBasedSupplement`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

/** Document of read versions. */
export const READSTATE_DOCUMENT = "m/read";
/** Document of thread states. */
export const PRESENTATION_DOCUMENT = "m/presentation";
/** Document of folders. */
export const FOLDERS_DOCUMENT = "m/folder";
/** Document of archive versions. */
export const ARCHIVING_DOCUMENT = "m/archiving";
/** Document of the muted flag. */
export const MUTED_DOCUMENT = "m/muted";
/** Document of the flag that overrides the archive versions. */
export const CLEARED_DOCUMENT = "m/cleared";
/** Document of wanted evaluations. */
export const ABUSE_DOCUMENT = "m/abuse";
/**
 * Document of seen versions, notified versions and the pending
 * notification flag.
 */
export const SEEN_DOCUMENT = "m/seen";
/** Document of private gadget states. */
export const GADGETS_DOCUMENT = "m/gadgets";

export const SEEN_VERSION_TAG = "seen";
export const NOTIFIED_VERSION_TAG = "notified";
export const WAVELET_TAG = "wavelet";
export const BLIP_READ_TAG = "blip";
export const PARTICIPANTS_READ_TAG = "participants";
export const TAGS_READ_TAG = "tags";
export const WAVELET_READ_TAG = "all";
export const CONVERSATION_TAG = "conversation";
export const THREAD_TAG = "thread";
export const ARCHIVE_TAG = "archive";
export const VERSION_ATTR = "v";
export const ID_ATTR = "i";
export const FOLDER_TAG = "folder";
export const MUTED_TAG = "muted";
export const MUTED_ATTR = "muted";
export const CLEARED_TAG = "cleared";
export const CLEARED_ATTR = "cleared";
export const SIGNATURE_ATTR = "signature";
export const STATE_ATTR = "state";
export const NOTIFICATION_TAG = "notification";
export const PENDING_NOTIFICATION_ATTR = "pending";
export const GADGET_TAG = "gadget";
/** Permitted by the schema but never read or written. */
export const PERMISSIONS_ATTR = "p";
export const STATE_TAG = "state";
export const NAME_ATTR = "name";
export const VALUE_ATTR = "value";

export const WANTED_EVAL_TAG = "wanted";
export const AGENT_ATTR = "agent";
export const CERTAINTY_ATTR = "certainty";
export const COMMENT_ATTR = "comment";
export const TIMESTAMP_ATTR = "timestamp";
export const WANTED_ATTR = "wanted";
export const WAVELET_ID_ATTR = "wavelet_id";
export const IGNORED_ATTR = "ignored";
export const ADDER_ATTR = "adder";
