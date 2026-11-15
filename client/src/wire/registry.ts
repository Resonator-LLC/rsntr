// Registry of the message codecs by name, for code that goes from the
// `messageType` of an RPC envelope to a codec.
//
// Corresponds to the lookup by simple class name in `ProtoSerializer` and
// `WebSocketChannel` of Apache Wave, licensed under the Apache License,
// Version 2.0.

import {
  AppEventRequest,
  AppEventResponse,
  AppMediaRequest,
  AppOpenRequest,
  AppPair,
  AppRtcRequest,
  AppRtcResponse,
  AppRtcSlot,
  AppUiNode,
  AppUpdate,
} from "./app";
import { AttachmentMetadata, AttachmentsResponse, ImageMetadata } from "./attachment";
import {
  AnnotationBoundary,
  Component,
  ElementStart,
  KeyValuePair,
  KeyValueUpdate,
  MutateDocument,
  ProtocolDocumentOperation,
  ProtocolHashedVersion,
  ProtocolWaveletDelta,
  ProtocolWaveletOperation,
  ReplaceAttributes,
  UpdateAttributes,
} from "./federation";
import { type Envelope, JsonError, type MessageCodec } from "./json";
import { FetchedProfile, ProfileRequest, ProfileResponse } from "./profile";
import { CancelRpc, RpcFinished } from "./rpc";
import { Digest, SearchRequest, SearchResponse } from "./search";
import {
  DocumentSnapshot,
  ProtocolAuthenticate,
  ProtocolAuthenticationResult,
  ProtocolOpenRequest,
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  ProtocolWaveletUpdate,
  WaveViewSnapshot,
  WaveletSnapshot,
  WaveletVersion,
} from "./waveclientRpc";

/** Every message codec of this module, nested message types included. */
export const ALL_CODECS: readonly MessageCodec<unknown>[] = [
  // federation.protodevel
  ProtocolHashedVersion,
  ProtocolWaveletDelta,
  ProtocolWaveletOperation,
  MutateDocument,
  ProtocolDocumentOperation,
  Component,
  KeyValuePair,
  KeyValueUpdate,
  ElementStart,
  ReplaceAttributes,
  UpdateAttributes,
  AnnotationBoundary,
  // waveclient-rpc.proto
  ProtocolAuthenticate,
  ProtocolAuthenticationResult,
  ProtocolOpenRequest,
  WaveletVersion,
  DocumentSnapshot,
  WaveletSnapshot,
  WaveViewSnapshot,
  ProtocolWaveletUpdate,
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  // rpc.proto
  CancelRpc,
  RpcFinished,
  // profiles.proto
  ProfileRequest,
  ProfileResponse,
  FetchedProfile,
  // search.proto
  SearchRequest,
  SearchResponse,
  Digest,
  // wave apps
  AppOpenRequest,
  AppPair,
  AppUiNode,
  AppUpdate,
  AppEventRequest,
  AppEventResponse,
  AppMediaRequest,
  AppRtcSlot,
  AppRtcRequest,
  AppRtcResponse,
  // attachment.proto
  AttachmentsResponse,
  AttachmentMetadata,
  ImageMetadata,
];

const BY_NAME: ReadonlyMap<string, MessageCodec<unknown>> = new Map(
  ALL_CODECS.map((codec) => [codec.name, codec]),
);

/** The codec of the message type called `name`, if there is one. */
export function findCodec(name: string): MessageCodec<unknown> | undefined {
  return BY_NAME.get(name);
}

/**
 * The codec of the message type called `name`. Throws {@link JsonError}
 * (`unknownType`) if there is none.
 */
export function codecForName(name: string): MessageCodec<unknown> {
  const codec = BY_NAME.get(name);
  if (codec === undefined) {
    throw new JsonError({ kind: "unknownType", name });
  }
  return codec;
}

/**
 * The body of an envelope, decoded by the codec its `messageType` names,
 * with required fields checked. Throws {@link JsonError} (`unknownType`)
 * for a message type that is not registered.
 */
export function decodeEnvelopeBody(envelope: Envelope): unknown {
  return codecForName(envelope.messageType).decode(envelope.message);
}
