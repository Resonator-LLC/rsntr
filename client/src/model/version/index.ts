// Wavelet versions paired with a history hash.
//
// Derived from the `org.waveprotocol.wave.model.version` package of Apache
// Wave, licensed under the Apache License, Version 2.0.
//
// The history hash at version zero is the UTF-8 encoding of the wavelet
// name's `wave://` URI. Each applied delta extends the chain: the new hash
// is the first 20 bytes of `SHA-256(previous hash ++ applied delta bytes)`,
// where the bytes are the serialised `ProtocolAppliedWaveletDelta`.

import { IdUriCodec } from "../id";
import type { WaveletName } from "../id";
import { sha256 } from "./sha256";

export { sha256 } from "./sha256";

/** The reason for a {@link HashedVersionParseError}. */
export type HashedVersionParseErrorDetail =
  | { readonly kind: "version"; readonly value: string }
  | { readonly kind: "hash"; readonly value: string };

/** A string that is not a serialised hashed version. */
export class HashedVersionParseError extends Error {
  readonly detail: HashedVersionParseErrorDetail;

  constructor(detail: HashedVersionParseErrorDetail) {
    super(
      detail.kind === "version"
        ? `Invalid version: '${detail.value}'`
        : `Invalid base64 hash: '${detail.value}'`,
    );
    this.name = "HashedVersionParseError";
    this.detail = detail;
  }

  get kind(): HashedVersionParseErrorDetail["kind"] {
    return this.detail.kind;
  }
}

const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

function base64Encode(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] as number;
    const b1 = bytes[i + 1];
    const b2 = bytes[i + 2];
    const n = (b0 << 16) | ((b1 ?? 0) << 8) | (b2 ?? 0);
    out +=
      BASE64_ALPHABET.charAt((n >> 18) & 0x3f) +
      BASE64_ALPHABET.charAt((n >> 12) & 0x3f) +
      (b1 === undefined ? "=" : BASE64_ALPHABET.charAt((n >> 6) & 0x3f)) +
      (b2 === undefined ? "=" : BASE64_ALPHABET.charAt(n & 0x3f));
  }
  return out;
}

/**
 * Standard alphabet; padding optional; trailing bits ignored. `null` if the
 * input is not base64.
 */
function base64DecodeLenient(s: string): Uint8Array | null {
  let end = s.length;
  if (end > 0 && s.charAt(end - 1) === "=") {
    // Padding, if present, must complete a group of four.
    if (s.length % 4 !== 0) {
      return null;
    }
    end -= s.charAt(end - 2) === "=" ? 2 : 1;
  }
  if (end % 4 === 1) {
    return null;
  }
  const out = new Uint8Array(Math.floor((end * 3) / 4));
  let acc = 0;
  let bits = 0;
  let o = 0;
  for (let i = 0; i < end; i++) {
    const v = BASE64_ALPHABET.indexOf(s.charAt(i));
    if (v < 0) {
      return null;
    }
    acc = ((acc << 6) | v) & 0xffff;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o++] = (acc >> bits) & 0xff;
    }
  }
  return out;
}

/** A wavelet version number and the history hash at that version. */
export class HashedVersion {
  private constructor(
    readonly version: number,
    /** Must not be modified. */
    readonly historyHash: Uint8Array,
  ) {}

  /** The hash is copied. */
  static of(version: number, historyHash: Uint8Array): HashedVersion {
    return new HashedVersion(version, historyHash.slice());
  }

  /** A version with an empty hash. */
  static unsigned(version: number): HashedVersion {
    return new HashedVersion(version, new Uint8Array(0));
  }

  /**
   * Parses the form written by `toString`, `<version>:<base64 hash>`. The
   * hash and its colon may be absent. Throws
   * {@link HashedVersionParseError}.
   */
  static fromString(s: string): HashedVersion {
    const colon = s.indexOf(":");
    const versionPart = colon < 0 ? s : s.slice(0, colon);
    // A decimal integer, checked to be non-negative.
    const version = /^[+-]?[0-9]+$/.test(versionPart) ? Number(versionPart) : NaN;
    if (!Number.isSafeInteger(version) || version < 0) {
      throw new HashedVersionParseError({ kind: "version", value: versionPart });
    }
    if (colon < 0) {
      return HashedVersion.unsigned(version || 0);
    }
    const hashPart = s.slice(colon + 1);
    const hash = base64DecodeLenient(hashPart);
    if (hash === null) {
      throw new HashedVersionParseError({ kind: "hash", value: hashPart });
    }
    return new HashedVersion(version || 0, hash);
  }

  equals(other: HashedVersion): boolean {
    return this.compareTo(other) === 0;
  }

  /**
   * Orders by version, then by hash. Hash bytes compare as signed values,
   * as in the original, and a hash sorts after its proper prefixes.
   */
  compareTo(other: HashedVersion): -1 | 0 | 1 {
    if (this.version !== other.version) {
      return this.version < other.version ? -1 : 1;
    }
    const a = this.historyHash;
    const b = other.historyHash;
    const n = Math.min(a.length, b.length);
    for (let i = 0; i < n; i++) {
      const x = ((a[i] as number) << 24) >> 24;
      const y = ((b[i] as number) << 24) >> 24;
      if (x !== y) {
        return x < y ? -1 : 1;
      }
    }
    return a.length < b.length ? -1 : a.length > b.length ? 1 : 0;
  }

  /** `<version>:<hash in padded standard base64>`. */
  toString(): string {
    return `${this.version}:${base64Encode(this.historyHash)}`;
  }
}

/** Number of bits of the SHA-256 digest kept in a history hash. */
export const HASH_SIZE_BITS = 160;

/**
 * Creates hashed versions (`HashedVersionFactoryImpl` with the server's URI
 * codec).
 */
export const HashedVersionFactory = {
  /**
   * The version of a wavelet before any delta: version 0 with the UTF-8
   * bytes of the wavelet name URI as its hash.
   */
  createVersionZero(waveletName: WaveletName): HashedVersion {
    return HashedVersion.of(0, new TextEncoder().encode(IdUriCodec.waveletNameToUri(waveletName)));
  },

  /**
   * The version after applying a delta at `versionAppliedAt`.
   * `appliedDeltaBytes` are the serialised `ProtocolAppliedWaveletDelta`.
   */
  create(
    appliedDeltaBytes: Uint8Array,
    versionAppliedAt: HashedVersion,
    operationsApplied: number,
  ): HashedVersion {
    return HashedVersionFactory.createWithHashSize(
      appliedDeltaBytes,
      versionAppliedAt,
      operationsApplied,
      HASH_SIZE_BITS,
    );
  },

  /**
   * {@link HashedVersionFactory.create} with a hash truncated to
   * `hashSizeBits` (at most 256) instead of {@link HASH_SIZE_BITS}.
   */
  createWithHashSize(
    appliedDeltaBytes: Uint8Array,
    versionAppliedAt: HashedVersion,
    operationsApplied: number,
    hashSizeBits: number,
  ): HashedVersion {
    const digest = sha256(versionAppliedAt.historyHash, appliedDeltaBytes);
    const size = Math.min(Math.floor(hashSizeBits / 8), digest.length);
    return HashedVersion.of(
      versionAppliedAt.version + operationsApplied,
      digest.subarray(0, size),
    );
  },
} as const;
