// Messages of `box/search/search.proto` (package `search`), with their JSON
// codecs.
//
// Port of `crates/resonator-model/src/proto/search.rs`; derived from Apache Wave,
// licensed under the Apache License, Version 2.0. Verified against the
// Java-generated golden data only: the Rust server's endpoint for these
// messages has not been exercised from this client.

import { type MessageCodec, defineMessage, field } from "./json";

export interface SearchRequest {
  readonly query: string;
  /** The index from which to return results. A signed 32-bit integer. */
  readonly index: number;
  /** The number of results to return. A signed 32-bit integer. */
  readonly numResults: number;
}

export const SearchRequest: MessageCodec<SearchRequest> = defineMessage("SearchRequest", [
  field(1, "query", "required", "string"),
  field(2, "index", "required", "int32"),
  field(3, "numResults", "required", "int32"),
]);

/** `SearchResponse.Digest`: the digest of one wave. */
export interface Digest {
  readonly title: string;
  /** A text snippet. */
  readonly snippet: string;
  /** The serialised wave id. */
  readonly waveId: string;
  /** `int64`, `[low, high]` on the wire. Milliseconds since the epoch. */
  readonly lastModified: bigint;
  /** A signed 32-bit integer. */
  readonly unreadCount: number;
  /** A signed 32-bit integer. */
  readonly blipCount: number;
  /** The participants other than the author. */
  readonly participants: readonly string[];
  /** The first participant. */
  readonly author: string;
}

export const Digest: MessageCodec<Digest> = defineMessage("Digest", [
  field(1, "title", "required", "string"),
  field(2, "snippet", "required", "string"),
  field(3, "waveId", "required", "string"),
  field(4, "lastModified", "required", "int64"),
  field(5, "unreadCount", "required", "int32"),
  field(6, "blipCount", "required", "int32"),
  field(7, "participants", "repeated", "string"),
  field(8, "author", "required", "string"),
]);

export interface SearchResponse {
  /** The query the response is for. */
  readonly query: string;
  /** The number of results if known, else -1. A signed 32-bit integer. */
  readonly totalResults: number;
  readonly digests: readonly Digest[];
}

export const SearchResponse: MessageCodec<SearchResponse> = defineMessage("SearchResponse", [
  field(1, "query", "required", "string"),
  field(2, "totalResults", "required", "int32"),
  field(3, "digests", "repeated", Digest),
]);
