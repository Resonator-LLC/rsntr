// Messages of `org/waveprotocol/wave/federation/federation.protodevel`
// (package `federation`) that are spoken on the client wire, with their JSON
// codecs.
//
// Port of `crates/resonator-model/src/proto/federation.rs`; derived from Apache
// Wave, licensed under the Apache License, Version 2.0. The messages that
// never reach the client as JSON (`ProtocolAppliedWaveletDelta`,
// `ProtocolSignedDelta`, `ProtocolSignature`, `ProtocolSignerInfo`) are not
// defined here.

import { type MessageCodec, defineMessage, field } from "./json";

/** A wavelet version and the history hash at that version. */
export interface ProtocolHashedVersion {
  /** An `int52` field: a safe integer. */
  readonly version: number;
  readonly historyHash: Uint8Array;
}

export const ProtocolHashedVersion: MessageCodec<ProtocolHashedVersion> = defineMessage(
  "ProtocolHashedVersion",
  [field(1, "version", "required", "int52"), field(2, "historyHash", "required", "bytes")],
);

export interface KeyValuePair {
  readonly key: string;
  readonly value: string;
}

export const KeyValuePair: MessageCodec<KeyValuePair> = defineMessage("KeyValuePair", [
  field(1, "key", "required", "string"),
  field(2, "value", "required", "string"),
]);

export interface KeyValueUpdate {
  readonly key: string;
  /** Absent if the key had no value. */
  readonly oldValue?: string;
  /** Absent if the key gets no value. */
  readonly newValue?: string;
}

export const KeyValueUpdate: MessageCodec<KeyValueUpdate> = defineMessage("KeyValueUpdate", [
  field(1, "key", "required", "string"),
  field(2, "oldValue", "optional", "string"),
  field(3, "newValue", "optional", "string"),
]);

export interface ElementStart {
  /** The tag name. */
  readonly type: string;
  readonly attribute: readonly KeyValuePair[];
}

export const ElementStart: MessageCodec<ElementStart> = defineMessage("ElementStart", [
  field(1, "type", "required", "string"),
  field(2, "attribute", "repeated", KeyValuePair),
]);

export interface ReplaceAttributes {
  /** Set when both attribute lists are empty. */
  readonly empty?: boolean;
  readonly oldAttribute: readonly KeyValuePair[];
  readonly newAttribute: readonly KeyValuePair[];
}

export const ReplaceAttributes: MessageCodec<ReplaceAttributes> = defineMessage(
  "ReplaceAttributes",
  [
    field(1, "empty", "optional", "bool"),
    field(2, "oldAttribute", "repeated", KeyValuePair),
    field(3, "newAttribute", "repeated", KeyValuePair),
  ],
);

export interface UpdateAttributes {
  /** Set when there are no updates. */
  readonly empty?: boolean;
  readonly attributeUpdate: readonly KeyValueUpdate[];
}

export const UpdateAttributes: MessageCodec<UpdateAttributes> = defineMessage("UpdateAttributes", [
  field(1, "empty", "optional", "bool"),
  field(2, "attributeUpdate", "repeated", KeyValueUpdate),
]);

export interface AnnotationBoundary {
  /** Set when there are no ends and no changes. */
  readonly empty?: boolean;
  readonly end: readonly string[];
  readonly change: readonly KeyValueUpdate[];
}

export const AnnotationBoundary: MessageCodec<AnnotationBoundary> = defineMessage(
  "AnnotationBoundary",
  [
    field(1, "empty", "optional", "bool"),
    field(2, "end", "repeated", "string"),
    field(3, "change", "repeated", KeyValueUpdate),
  ],
);

/**
 * A component of a document operation
 * (`ProtocolDocumentOperation.Component`). Exactly one field should be set.
 */
export interface Component {
  readonly annotationBoundary?: AnnotationBoundary;
  readonly characters?: string;
  readonly elementStart?: ElementStart;
  readonly elementEnd?: boolean;
  /** A signed 32-bit integer. */
  readonly retainItemCount?: number;
  readonly deleteCharacters?: string;
  readonly deleteElementStart?: ElementStart;
  readonly deleteElementEnd?: boolean;
  readonly replaceAttributes?: ReplaceAttributes;
  readonly updateAttributes?: UpdateAttributes;
}

export const Component: MessageCodec<Component> = defineMessage("Component", [
  field(1, "annotationBoundary", "optional", AnnotationBoundary),
  field(2, "characters", "optional", "string"),
  field(3, "elementStart", "optional", ElementStart),
  field(4, "elementEnd", "optional", "bool"),
  field(5, "retainItemCount", "optional", "int32"),
  field(6, "deleteCharacters", "optional", "string"),
  field(7, "deleteElementStart", "optional", ElementStart),
  field(8, "deleteElementEnd", "optional", "bool"),
  field(9, "replaceAttributes", "optional", ReplaceAttributes),
  field(10, "updateAttributes", "optional", UpdateAttributes),
]);

/** A document operation: a list of components. */
export interface ProtocolDocumentOperation {
  readonly component: readonly Component[];
}

export const ProtocolDocumentOperation: MessageCodec<ProtocolDocumentOperation> = defineMessage(
  "ProtocolDocumentOperation",
  [field(1, "component", "repeated", Component)],
);

/** `ProtocolWaveletOperation.MutateDocument`. */
export interface MutateDocument {
  readonly documentId: string;
  readonly documentOperation: ProtocolDocumentOperation;
}

export const MutateDocument: MessageCodec<MutateDocument> = defineMessage("MutateDocument", [
  field(1, "documentId", "required", "string"),
  field(2, "documentOperation", "required", ProtocolDocumentOperation),
]);

/** An operation within a delta. Exactly one field should be set. */
export interface ProtocolWaveletOperation {
  readonly addParticipant?: string;
  readonly removeParticipant?: string;
  readonly mutateDocument?: MutateDocument;
  readonly noOp?: boolean;
}

export const ProtocolWaveletOperation: MessageCodec<ProtocolWaveletOperation> = defineMessage(
  "ProtocolWaveletOperation",
  [
    field(1, "addParticipant", "optional", "string"),
    field(2, "removeParticipant", "optional", "string"),
    field(3, "mutateDocument", "optional", MutateDocument),
    field(4, "noOp", "optional", "bool"),
  ],
);

/** Operations by one author for a wavelet at a given version. */
export interface ProtocolWaveletDelta {
  /** The version the operations apply at. */
  readonly hashedVersion: ProtocolHashedVersion;
  readonly author: string;
  readonly operation: readonly ProtocolWaveletOperation[];
  /** Addresses on whose behalf the author acts; unused by the server. */
  readonly addressPath: readonly string[];
}

export const ProtocolWaveletDelta: MessageCodec<ProtocolWaveletDelta> = defineMessage(
  "ProtocolWaveletDelta",
  [
    field(1, "hashedVersion", "required", ProtocolHashedVersion),
    field(2, "author", "required", "string"),
    field(3, "operation", "repeated", ProtocolWaveletOperation),
    field(4, "addressPath", "repeated", "string"),
  ],
);
