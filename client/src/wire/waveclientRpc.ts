// Messages of `box/common/comms/waveclient-rpc.proto` (package
// `waveserver`): the client-server protocol, with their JSON codecs.
//
// Port of `crates/resonator-model/src/proto/waveclient_rpc.rs`; derived from
// Apache Wave, licensed under the Apache License, Version 2.0.

import {
  ProtocolDocumentOperation,
  ProtocolHashedVersion,
  ProtocolWaveletDelta,
} from "./federation";
import { type MessageCodec, defineMessage, field } from "./json";

export interface ProtocolAuthenticate {
  readonly token: string;
}

export const ProtocolAuthenticate: MessageCodec<ProtocolAuthenticate> =
  defineMessage("ProtocolAuthenticate", [
    field(1, "token", "required", "string"),
  ]);

// eslint-disable-next-line @typescript-eslint/no-empty-object-type
export interface ProtocolAuthenticationResult {}

export const ProtocolAuthenticationResult: MessageCodec<ProtocolAuthenticationResult> =
  defineMessage("ProtocolAuthenticationResult", []);

export interface WaveletVersion {
  readonly waveletId: string;
  readonly hashedVersion: ProtocolHashedVersion;
}

export const WaveletVersion: MessageCodec<WaveletVersion> = defineMessage(
  "WaveletVersion",
  [
    field(1, "waveletId", "required", "string"),
    field(2, "hashedVersion", "required", ProtocolHashedVersion),
  ],
);

/** A request to open a wave view. */
export interface ProtocolOpenRequest {
  readonly participantId: string;
  readonly waveId: string;
  readonly waveletIdPrefix: readonly string[];
  readonly knownWavelet: readonly WaveletVersion[];
}

export const ProtocolOpenRequest: MessageCodec<ProtocolOpenRequest> =
  defineMessage("ProtocolOpenRequest", [
    field(1, "participantId", "required", "string"),
    field(2, "waveId", "required", "string"),
    field(3, "waveletIdPrefix", "repeated", "string"),
    field(4, "knownWavelet", "repeated", WaveletVersion),
  ]);

/** The state of a document. */
export interface DocumentSnapshot {
  readonly documentId: string;
  /** The document content as an initialization. */
  readonly documentOperation: ProtocolDocumentOperation;
  readonly author: string;
  readonly contributor: readonly string[];
  /** `int64`, `[low, high]` on the wire. */
  readonly lastModifiedVersion: bigint;
  /** `int64`, `[low, high]` on the wire. Milliseconds since the epoch. */
  readonly lastModifiedTime: bigint;
}

export const DocumentSnapshot: MessageCodec<DocumentSnapshot> = defineMessage(
  "DocumentSnapshot",
  [
    field(1, "documentId", "required", "string"),
    field(2, "documentOperation", "required", ProtocolDocumentOperation),
    field(3, "author", "required", "string"),
    field(4, "contributor", "repeated", "string"),
    field(5, "lastModifiedVersion", "required", "int64"),
    field(6, "lastModifiedTime", "required", "int64"),
  ],
);

/** The state of a wavelet. */
export interface WaveletSnapshot {
  /** In the modern serialisation, `domain/id`. */
  readonly waveletId: string;
  readonly participantId: readonly string[];
  readonly document: readonly DocumentSnapshot[];
  readonly version: ProtocolHashedVersion;
  /** `int64`, `[low, high]` on the wire. Milliseconds since the epoch. */
  readonly lastModifiedTime: bigint;
  readonly creator: string;
  /** `int64`, `[low, high]` on the wire. Milliseconds since the epoch. */
  readonly creationTime: bigint;
}

export const WaveletSnapshot: MessageCodec<WaveletSnapshot> = defineMessage(
  "WaveletSnapshot",
  [
    field(1, "waveletId", "required", "string"),
    field(2, "participantId", "repeated", "string"),
    field(3, "document", "repeated", DocumentSnapshot),
    field(4, "version", "required", ProtocolHashedVersion),
    field(5, "lastModifiedTime", "required", "int64"),
    field(6, "creator", "required", "string"),
    field(7, "creationTime", "required", "int64"),
  ],
);

export interface WaveViewSnapshot {
  readonly waveId: string;
  readonly wavelet: readonly WaveletSnapshot[];
}

export const WaveViewSnapshot: MessageCodec<WaveViewSnapshot> = defineMessage(
  "WaveViewSnapshot",
  [
    field(1, "waveId", "required", "string"),
    field(2, "wavelet", "repeated", WaveletSnapshot),
  ],
);

/** An update of a wave view: a channel id, a marker, a snapshot or deltas. */
export interface ProtocolWaveletUpdate {
  readonly waveletName: string;
  readonly appliedDelta: readonly ProtocolWaveletDelta[];
  readonly commitNotice?: ProtocolHashedVersion;
  readonly resultingVersion?: ProtocolHashedVersion;
  readonly snapshot?: WaveletSnapshot;
  /** Defaults to false. */
  readonly marker?: boolean;
  readonly channelId?: string;
  /**
   * Not in the original: the wavelet's history was rewound to this version
   * after a change of host; `appliedDelta` are then the deltas the client
   * has seen since, which no longer exist.
   */
  readonly resetVersion?: ProtocolHashedVersion;
}

export const ProtocolWaveletUpdate: MessageCodec<ProtocolWaveletUpdate> =
  defineMessage("ProtocolWaveletUpdate", [
    field(1, "waveletName", "required", "string"),
    field(2, "appliedDelta", "repeated", ProtocolWaveletDelta),
    field(3, "commitNotice", "optional", ProtocolHashedVersion),
    field(4, "resultingVersion", "optional", ProtocolHashedVersion),
    field(5, "snapshot", "optional", WaveletSnapshot),
    field(6, "marker", "optional", "bool"),
    field(7, "channelId", "optional", "string"),
    field(8, "resetVersion", "optional", ProtocolHashedVersion),
  ]);

/** A request to apply a delta to a wavelet. */
export interface ProtocolSubmitRequest {
  readonly waveletName: string;
  readonly delta: ProtocolWaveletDelta;
  readonly channelId?: string;
}

export const ProtocolSubmitRequest: MessageCodec<ProtocolSubmitRequest> =
  defineMessage("ProtocolSubmitRequest", [
    field(1, "waveletName", "required", "string"),
    field(2, "delta", "required", ProtocolWaveletDelta),
    field(3, "channelId", "optional", "string"),
  ]);

/** The result of a submit request. */
export interface ProtocolSubmitResponse {
  /** A signed 32-bit integer. */
  readonly operationsApplied: number;
  readonly errorMessage?: string;
  readonly hashedVersionAfterApplication?: ProtocolHashedVersion;
  /**
   * An interim answer: the wavelet's host cannot be reached now, so the
   * server keeps the delta and sends it when it can; the final answer
   * follows on the same request.
   */
  readonly pending?: boolean;
}

export const ProtocolSubmitResponse: MessageCodec<ProtocolSubmitResponse> =
  defineMessage("ProtocolSubmitResponse", [
    field(1, "operationsApplied", "required", "int32"),
    field(2, "errorMessage", "optional", "string"),
    field(
      3,
      "hashedVersionAfterApplication",
      "optional",
      ProtocolHashedVersion,
    ),
    field(4, "pending", "optional", "bool"),
  ]);
