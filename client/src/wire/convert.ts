// Conversion between wire messages and model types.
//
// Port of the client-visible part of `crates/resonator-model/src/proto/convert.rs`,
// itself derived from `CoreWaveletOperationSerializer`,
// `WaveletOperationSerializer` and `SnapshotSerializer` of Apache Wave,
// licensed under the Apache License, Version 2.0. The storage and
// applied-delta conversions are server only and are not ported.
//
// The message types of this module always carry their required fields, but
// a message built in memory (or read with `readJson`) may lack them, so they
// are still checked here (`missingField`).

import { BlipData, DataError, WaveletData } from "../model/data";
import {
  AnnotationBoundaryMap,
  type Attribute,
  Attributes,
  AttributesUpdate,
  DocOp,
  type DocOpComponent,
  OperationError,
  type UpdateTriple,
  asInitialization,
} from "../model/docop";
import {
  InvalidIdError,
  InvalidParticipantAddress,
  ModernIdSerialiser,
  ParticipantId,
  type WaveId,
  WaveletName,
} from "../model/id";
import {
  DeltaError,
  NO_TIMESTAMP,
  TransformedWaveletDelta,
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
  WaveletOperationError,
} from "../model/operation";
import { HashedVersion } from "../model/version";
import type {
  AnnotationBoundary,
  Component,
  ElementStart,
  KeyValuePair,
  KeyValueUpdate,
  ProtocolDocumentOperation,
  ProtocolHashedVersion,
  ProtocolWaveletDelta,
  ProtocolWaveletOperation,
  ReplaceAttributes,
  UpdateAttributes,
} from "./federation";
import type { DocumentSnapshot, WaveletSnapshot } from "./waveclientRpc";

/** The reason for a {@link ProtoError}. */
export type ProtoErrorDetail =
  /** A required field is not set; `field` is `Message.proto_field_name`. */
  | { readonly kind: "missingField"; readonly field: string }
  /**
   * A field holds a value the model cannot represent (a negative version or
   * retain count), or the model holds one the field cannot.
   */
  | { readonly kind: "invalidValue"; readonly field: string; readonly message: string }
  /** The model value has no encoding, or the message no model value. */
  | { readonly kind: "unsupported"; readonly what: string }
  | { readonly kind: "operation"; readonly error: OperationError }
  | { readonly kind: "waveletOperation"; readonly error: WaveletOperationError }
  | { readonly kind: "delta"; readonly error: DeltaError }
  | { readonly kind: "data"; readonly error: DataError }
  | { readonly kind: "participantId"; readonly error: InvalidParticipantAddress }
  | { readonly kind: "id"; readonly error: InvalidIdError };

function protoErrorMessage(d: ProtoErrorDetail): string {
  switch (d.kind) {
    case "missingField":
      return `message missing required field: ${d.field}`;
    case "invalidValue":
      return `invalid value for ${d.field}: ${d.message}`;
    case "unsupported":
      return `Unsupported ${d.what}`;
    default:
      return d.error.message;
  }
}

/** Failure to convert between a wire message and a model value. */
export class ProtoError extends Error {
  readonly detail: ProtoErrorDetail;

  constructor(detail: ProtoErrorDetail) {
    super(protoErrorMessage(detail), "error" in detail ? { cause: detail.error } : undefined);
    this.name = "ProtoError";
    this.detail = detail;
  }

  get kind(): ProtoErrorDetail["kind"] {
    return this.detail.kind;
  }
}

function invalid(field: string, message: string): ProtoError {
  return new ProtoError({ kind: "invalidValue", field, message });
}

function required<T>(value: T | undefined | null, field: string): T {
  if (value === undefined || value === null) {
    throw new ProtoError({ kind: "missingField", field });
  }
  return value;
}

/** Runs `f`, turning the model's errors into {@link ProtoError}s. */
function converting<T>(f: () => T): T {
  try {
    return f();
  } catch (error) {
    if (error instanceof OperationError) {
      throw new ProtoError({ kind: "operation", error });
    }
    if (error instanceof WaveletOperationError) {
      throw new ProtoError({ kind: "waveletOperation", error });
    }
    if (error instanceof DeltaError) {
      throw new ProtoError({ kind: "delta", error });
    }
    if (error instanceof DataError) {
      throw new ProtoError({ kind: "data", error });
    }
    if (error instanceof InvalidParticipantAddress) {
      throw new ProtoError({ kind: "participantId", error });
    }
    if (error instanceof InvalidIdError) {
      throw new ProtoError({ kind: "id", error });
    }
    throw error;
  }
}

/** A model number as an `int64` field; `what` describes the overflow. */
function toInt64(value: number, field: string): bigint {
  if (!Number.isSafeInteger(value)) {
    throw invalid(field, "exceeds the safe integer range");
  }
  return BigInt(value);
}

/** An `int64` field as a model number. */
function fromInt64(value: bigint | number, field: string): number {
  const n = typeof value === "bigint" ? value : BigInt(Math.trunc(value));
  if (n < BigInt(Number.MIN_SAFE_INTEGER) || n > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw invalid(field, "exceeds the safe integer range");
  }
  return Number(n);
}

const INT32_MAX = 0x7fffffff;

// Hashed versions.

/**
 * Fails if the version is not a non-negative safe integer (the Rust
 * original accepts the whole range of int64).
 */
export function serializeHashedVersion(version: HashedVersion): ProtocolHashedVersion {
  if (!Number.isSafeInteger(version.version) || version.version < 0) {
    throw invalid("ProtocolHashedVersion.version", "exceeds the safe integer range");
  }
  return { version: version.version, historyHash: version.historyHash.slice() };
}

/** Fails on a negative version or a missing field. */
export function deserializeHashedVersion(version: ProtocolHashedVersion): HashedVersion {
  const number = required(version.version, "ProtocolHashedVersion.version");
  if (number < 0) {
    throw invalid("ProtocolHashedVersion.version", "negative version");
  }
  if (!Number.isSafeInteger(number)) {
    throw invalid("ProtocolHashedVersion.version", "exceeds the safe integer range");
  }
  const hash = required(version.historyHash, "ProtocolHashedVersion.history_hash");
  return HashedVersion.of(number, hash);
}

// Document operations.

function keyValuePairs(attributes: Attributes): KeyValuePair[] {
  return attributes.entries.map(({ name, value }) => ({ key: name, value }));
}

function keyValueUpdate(
  key: string,
  oldValue: string | null,
  newValue: string | null,
): KeyValueUpdate {
  return {
    key,
    ...(oldValue !== null ? { oldValue } : {}),
    ...(newValue !== null ? { newValue } : {}),
  };
}

function elementStart(tag: string, attributes: Attributes): ElementStart {
  return { type: tag, attribute: keyValuePairs(attributes) };
}

function serializeComponent(component: DocOpComponent): Component {
  switch (component.type) {
    case "retain":
      if (
        !Number.isInteger(component.itemCount) ||
        component.itemCount > INT32_MAX ||
        component.itemCount < 0
      ) {
        throw invalid("Component.retain_item_count", "exceeds the range of int32");
      }
      return { retainItemCount: component.itemCount };
    case "characters":
      return { characters: component.characters };
    case "deleteCharacters":
      return { deleteCharacters: component.characters };
    case "elementStart":
      return { elementStart: elementStart(component.tag, component.attributes) };
    case "deleteElementStart":
      return { deleteElementStart: elementStart(component.tag, component.attributes) };
    case "elementEnd":
      return { elementEnd: true };
    case "deleteElementEnd":
      return { deleteElementEnd: true };
    case "replaceAttributes": {
      const { oldAttributes, newAttributes } = component;
      const replaceAttributes: ReplaceAttributes =
        oldAttributes.isEmpty() && newAttributes.isEmpty()
          ? { empty: true, oldAttribute: [], newAttribute: [] }
          : {
              oldAttribute: keyValuePairs(oldAttributes),
              newAttribute: keyValuePairs(newAttributes),
            };
      return { replaceAttributes };
    }
    case "updateAttributes": {
      const updateAttributes: UpdateAttributes = component.update.isEmpty()
        ? { empty: true, attributeUpdate: [] }
        : {
            attributeUpdate: component.update.updates.map((u) =>
              keyValueUpdate(u.name, u.oldValue, u.newValue),
            ),
          };
      return { updateAttributes };
    }
    case "annotationBoundary": {
      const map = component.boundary;
      const annotationBoundary: AnnotationBoundary = map.isEmpty()
        ? { empty: true, end: [], change: [] }
        : {
            end: [...map.endKeys],
            change: map.changes.map((c) => keyValueUpdate(c.key, c.oldValue, c.newValue)),
          };
      return { annotationBoundary };
    }
  }
}

/**
 * Converts a document operation to its message, one component each. Fails
 * if a retain count exceeds the range of int32.
 */
export function serializeDocOp(op: DocOp): ProtocolDocumentOperation {
  return { component: op.components.map(serializeComponent) };
}

/**
 * Builds attributes from pairs in any order; a repeated key keeps its last
 * value, as when the original collects the pairs into a hash map.
 */
function attributes(pairs: readonly KeyValuePair[] | undefined): Attributes {
  const values = new Map<string, string>();
  for (const pair of pairs ?? []) {
    const key = required(pair.key, "KeyValuePair.key");
    values.set(key, required(pair.value, "KeyValuePair.value"));
  }
  const entries: Attribute[] = Array.from(values, ([name, value]) => ({ name, value }));
  return Attributes.fromUnsorted(entries);
}

function updateKey(update: KeyValueUpdate): string {
  return required(update.key, "KeyValueUpdate.key");
}

function deserializeComponent(c: Component): DocOpComponent | null {
  if (c.annotationBoundary != null) {
    const a = c.annotationBoundary;
    if (a.empty === true) {
      return { type: "annotationBoundary", boundary: AnnotationBoundaryMap.empty() };
    }
    const changes = (a.change ?? []).map((change) => ({
      key: updateKey(change),
      oldValue: change.oldValue ?? null,
      newValue: change.newValue ?? null,
    }));
    return {
      type: "annotationBoundary",
      boundary: AnnotationBoundaryMap.create([...(a.end ?? [])], changes),
    };
  }
  if (c.characters != null) {
    return { type: "characters", characters: c.characters };
  }
  if (c.elementStart != null) {
    return {
      type: "elementStart",
      tag: required(c.elementStart.type, "ElementStart.type"),
      attributes: attributes(c.elementStart.attribute),
    };
  }
  if (c.elementEnd != null) {
    return { type: "elementEnd" };
  }
  if (c.retainItemCount != null) {
    if (c.retainItemCount < 0) {
      throw invalid("Component.retain_item_count", "negative retain count");
    }
    return { type: "retain", itemCount: c.retainItemCount };
  }
  if (c.deleteCharacters != null) {
    return { type: "deleteCharacters", characters: c.deleteCharacters };
  }
  if (c.deleteElementStart != null) {
    return {
      type: "deleteElementStart",
      tag: required(c.deleteElementStart.type, "ElementStart.type"),
      attributes: attributes(c.deleteElementStart.attribute),
    };
  }
  if (c.deleteElementEnd != null) {
    return { type: "deleteElementEnd" };
  }
  if (c.replaceAttributes != null) {
    const r = c.replaceAttributes;
    if (r.empty === true) {
      return {
        type: "replaceAttributes",
        oldAttributes: Attributes.empty(),
        newAttributes: Attributes.empty(),
      };
    }
    return {
      type: "replaceAttributes",
      oldAttributes: attributes(r.oldAttribute),
      newAttributes: attributes(r.newAttribute),
    };
  }
  if (c.updateAttributes != null) {
    const u = c.updateAttributes;
    if (u.empty === true) {
      return { type: "updateAttributes", update: AttributesUpdate.empty() };
    }
    const triples: UpdateTriple[] = (u.attributeUpdate ?? []).map((update) => [
      updateKey(update),
      update.oldValue ?? null,
      update.newValue ?? null,
    ]);
    return { type: "updateAttributes", update: AttributesUpdate.fromTriples(triples) };
  }
  return null;
}

/**
 * Converts a message to a document operation and checks that it is
 * well-formed.
 *
 * A component with several fields set is read as the first of them in
 * field order, a boolean component counts when present even if false, and
 * a component with no field set is skipped, as in the original. A negative
 * retain count is rejected.
 */
export function deserializeDocOp(op: ProtocolDocumentOperation): DocOp {
  return converting(() => {
    const components: DocOpComponent[] = [];
    for (const c of op.component ?? []) {
      const component = deserializeComponent(c);
      if (component !== null) {
        components.push(component);
      }
    }
    return DocOp.create(components);
  });
}

// Wavelet operations.

/**
 * Converts a wavelet operation to its message. Blip submits and version
 * updates have no encoding.
 */
export function serializeOp(op: WaveletOperation): ProtocolWaveletOperation {
  const kind = op.kind;
  switch (kind.type) {
    case "noOp":
      return { noOp: true };
    case "addParticipant":
      return { addParticipant: kind.participant.address };
    case "removeParticipant":
      return { removeParticipant: kind.participant.address };
    case "blip": {
      const blipOp = kind.op.kind;
      if (blipOp.type !== "content") {
        throw new ProtoError({ kind: "unsupported", what: `blip operation: ${op}` });
      }
      return {
        mutateDocument: {
          documentId: kind.blipId,
          documentOperation: serializeDocOp(blipOp.op),
        },
      };
    }
    case "versionUpdate":
      throw new ProtoError({ kind: "unsupported", what: `wavelet operation: ${op}` });
  }
}

/**
 * Converts a message to a wavelet operation in the given context. If
 * several fields are set the first of no-op, add participant, remove
 * participant and mutate document wins; a present `noOp` counts even if
 * false. Participant addresses are lower-cased but not validated.
 */
export function deserializeOp(
  op: ProtocolWaveletOperation,
  context: WaveletOperationContext,
): WaveletOperation {
  if (op.noOp != null) {
    return WaveletOperation.noOp(context);
  }
  if (op.addParticipant != null) {
    return WaveletOperation.addParticipant(context, ParticipantId.newUnchecked(op.addParticipant));
  }
  if (op.removeParticipant != null) {
    return WaveletOperation.removeParticipant(
      context,
      ParticipantId.newUnchecked(op.removeParticipant),
    );
  }
  if (op.mutateDocument != null) {
    const documentId = required(op.mutateDocument.documentId, "MutateDocument.document_id");
    const documentOperation = required(
      op.mutateDocument.documentOperation,
      "MutateDocument.document_operation",
    );
    return WaveletOperation.blipContent(context, documentId, deserializeDocOp(documentOperation));
  }
  throw new ProtoError({ kind: "unsupported", what: "operation: (empty)" });
}

// Deltas.

function serializeOps(ops: Iterable<WaveletOperation>): ProtocolWaveletOperation[] {
  return Array.from(ops, serializeOp);
}

/** Converts a client delta to its message. */
export function serializeDelta(delta: WaveletDelta): ProtocolWaveletDelta {
  return {
    hashedVersion: serializeHashedVersion(delta.targetVersion),
    author: delta.author.address,
    operation: serializeOps(delta.ops),
    addressPath: [],
  };
}

/**
 * Converts a transformed delta to the message sent to clients: the
 * applied-at version without a hash, the author and the operations.
 */
export function serializeTransformedDelta(delta: TransformedWaveletDelta): ProtocolWaveletDelta {
  return {
    hashedVersion: serializeHashedVersion(HashedVersion.unsigned(delta.appliedAtVersion)),
    author: delta.author.address,
    operation: serializeOps(delta.ops),
    addressPath: [],
  };
}

/**
 * Converts a message to a client delta. Operation contexts have the author
 * as creator, no timestamp and a version increment of one.
 *
 * As in the original, the author's address is validated only if the delta
 * has operations.
 */
export function deserializeDelta(delta: ProtocolWaveletDelta): WaveletDelta {
  return converting(() => {
    const author = required(delta.author, "ProtocolWaveletDelta.author");
    const ops = (delta.operation ?? []).map((op) =>
      deserializeOp(op, new WaveletOperationContext(ParticipantId.of(author), NO_TIMESTAMP, 1)),
    );
    const hashedVersion = deserializeHashedVersion(
      required(delta.hashedVersion, "ProtocolWaveletDelta.hashed_version"),
    );
    return new WaveletDelta(ParticipantId.newUnchecked(author), hashedVersion, ops);
  });
}

/**
 * Converts a non-empty delta message to a transformed delta with the given
 * resulting version and application timestamp. The message's own hashed
 * version is ignored.
 */
export function deserializeTransformedDelta(
  delta: ProtocolWaveletDelta,
  resultingVersion: HashedVersion,
  applicationTimestamp: number,
): TransformedWaveletDelta {
  return converting(() => {
    const author = ParticipantId.of(required(delta.author, "ProtocolWaveletDelta.author"));
    const operations = delta.operation ?? [];
    if (operations.length === 0) {
      throw invalid("ProtocolWaveletDelta.operation", "Cannot deserialize an empty delta");
    }
    const last = operations.length - 1;
    const ops = operations.map((op, i) =>
      deserializeOp(
        op,
        new WaveletOperationContext(
          author,
          applicationTimestamp,
          1,
          i === last ? resultingVersion : null,
        ),
      ),
    );
    return new TransformedWaveletDelta(author, resultingVersion, applicationTimestamp, ops);
  });
}

// Snapshots.

/** Converts a document and its metadata to a snapshot message. */
export function serializeDocument(document: BlipData): DocumentSnapshot {
  if (document.lastModifiedVersion < 0) {
    throw invalid("DocumentSnapshot.last_modified_version", "negative version");
  }
  return {
    documentId: document.id,
    documentOperation: serializeDocOp(document.content.asOperation()),
    author: document.author.address,
    contributor: document.contributors.map((p) => p.address),
    lastModifiedVersion: toInt64(
      document.lastModifiedVersion,
      "DocumentSnapshot.last_modified_version",
    ),
    lastModifiedTime: toInt64(document.lastModifiedTime, "DocumentSnapshot.last_modified_time"),
  };
}

/**
 * Converts a wavelet to a snapshot message at `hashedVersion`, which need
 * not be the wavelet's own. Documents are listed in id order (the original
 * lists them in hash-map order).
 */
export function serializeWavelet(
  wavelet: WaveletData,
  hashedVersion: HashedVersion,
): WaveletSnapshot {
  return converting(() => ({
    waveletId: ModernIdSerialiser.INSTANCE.serialiseWaveletId(wavelet.waveletId),
    participantId: wavelet.participants.map((p) => p.address),
    document: wavelet.documents().map(serializeDocument),
    version: serializeHashedVersion(hashedVersion),
    lastModifiedTime: toInt64(wavelet.lastModifiedTime, "WaveletSnapshot.last_modified_time"),
    creator: wavelet.creator.address,
    creationTime: toInt64(wavelet.creationTime, "WaveletSnapshot.creation_time"),
  }));
}

/**
 * Converts a snapshot message of a wavelet in the wave `waveId` to wavelet
 * data at the snapshot's own version. All addresses are validated.
 */
export function deserializeWavelet(snapshot: WaveletSnapshot, waveId: WaveId): WaveletData {
  return converting(() => {
    const creator = ParticipantId.of(required(snapshot.creator, "WaveletSnapshot.creator"));
    const waveletId = ModernIdSerialiser.INSTANCE.deserialiseWaveletId(
      required(snapshot.waveletId, "WaveletSnapshot.wavelet_id"),
    );
    const creationTime = fromInt64(
      required(snapshot.creationTime, "WaveletSnapshot.creation_time"),
      "WaveletSnapshot.creation_time",
    );
    const version = deserializeHashedVersion(
      required(snapshot.version, "WaveletSnapshot.version"),
    );
    const lastModifiedTime = fromInt64(
      required(snapshot.lastModifiedTime, "WaveletSnapshot.last_modified_time"),
      "WaveletSnapshot.last_modified_time",
    );
    const wavelet = WaveletData.empty(
      WaveletName.of(waveId, waveletId),
      creator,
      version,
      creationTime,
    );
    for (const participant of snapshot.participantId ?? []) {
      wavelet.addParticipant(ParticipantId.of(participant));
    }
    for (const document of snapshot.document ?? []) {
      const op = deserializeDocOp(
        required(document.documentOperation, "DocumentSnapshot.document_operation"),
      );
      const content = asInitialization(op);
      const contributors = (document.contributor ?? []).map((p) => ParticipantId.of(p));
      const lastModifiedVersion = fromInt64(
        required(document.lastModifiedVersion, "DocumentSnapshot.last_modified_version"),
        "DocumentSnapshot.last_modified_version",
      );
      const documentId = required(document.documentId, "DocumentSnapshot.document_id");
      const author = ParticipantId.of(required(document.author, "DocumentSnapshot.author"));
      const documentTime = fromInt64(
        required(document.lastModifiedTime, "DocumentSnapshot.last_modified_time"),
        "DocumentSnapshot.last_modified_time",
      );
      if (lastModifiedVersion < 0) {
        throw invalid("DocumentSnapshot.last_modified_version", "negative version");
      }
      wavelet.createDocument(
        documentId,
        author,
        contributors,
        content,
        documentTime,
        lastModifiedVersion,
      );
    }
    wavelet.setLastModifiedTime(lastModifiedTime);
    return wavelet;
  });
}
