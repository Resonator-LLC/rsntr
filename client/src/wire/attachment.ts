// Messages of `box/attachment/attachment.proto` (package `attachment`), with
// their JSON codecs.
//
// Port of `crates/resonator-model/src/proto/attachment.rs`; derived from Apache Wave,
// licensed under the Apache License, Version 2.0. Verified against the
// Java-generated golden data only: the Rust server's endpoint for these
// messages has not been exercised from this client.

import { type MessageCodec, defineMessage, field } from "./json";

export interface ImageMetadata {
  /** A signed 32-bit integer. */
  readonly width: number;
  /** A signed 32-bit integer. */
  readonly height: number;
}

export const ImageMetadata: MessageCodec<ImageMetadata> = defineMessage("ImageMetadata", [
  field(1, "width", "required", "int32"),
  field(2, "height", "required", "int32"),
]);

export interface AttachmentMetadata {
  readonly attachmentId: string;
  readonly waveRef: string;
  readonly fileName: string;
  readonly mimeType: string;
  /** `int64`, `[low, high]` on the wire. Bytes. */
  readonly size: bigint;
  readonly creator: string;
  readonly attachmentUrl: string;
  readonly thumbnailUrl: string;
  readonly imageMetadata?: ImageMetadata;
  readonly thumbnailMetadata?: ImageMetadata;
  readonly malware?: boolean;
}

export const AttachmentMetadata: MessageCodec<AttachmentMetadata> = defineMessage(
  "AttachmentMetadata",
  [
    field(1, "attachmentId", "required", "string"),
    field(2, "waveRef", "required", "string"),
    field(3, "fileName", "required", "string"),
    field(4, "mimeType", "required", "string"),
    field(5, "size", "required", "int64"),
    field(6, "creator", "required", "string"),
    field(7, "attachmentUrl", "required", "string"),
    field(8, "thumbnailUrl", "required", "string"),
    field(9, "imageMetadata", "optional", ImageMetadata),
    field(10, "thumbnailMetadata", "optional", ImageMetadata),
    field(11, "malware", "optional", "bool"),
  ],
);

export interface AttachmentsResponse {
  readonly attachment: readonly AttachmentMetadata[];
}

export const AttachmentsResponse: MessageCodec<AttachmentsResponse> = defineMessage(
  "AttachmentsResponse",
  [field(1, "attachment", "repeated", AttachmentMetadata)],
);
