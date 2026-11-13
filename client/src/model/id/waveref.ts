// References to a wave, wavelet or document, and their URI path and query
// encodings (`WaveRef`, `WaverefEncoder` with the JVM escapers).
//
// Derived from the `org.waveprotocol.wave.model.waveref` package of Apache
// Wave, licensed under the Apache License, Version 2.0.

import { percentEscape, split, urlDecodeBytes, utf8DecodeLossy } from "./java";
import { InvalidIdError, WaveId, WaveletId } from "./waveId";

/** A string that is not a valid encoded wave reference. */
export class InvalidWaveRefError extends Error {
  readonly reference: string;
  /** The reason, without the reference. */
  readonly reason: string;

  constructor(reference: string, reason: string) {
    super(`Invalid waveRef '${reference}': ${reason}`);
    this.name = "InvalidWaveRefError";
    this.reference = reference;
    this.reason = reason;
  }
}

/**
 * A reference to a wave, optionally narrowed to a wavelet and to a document
 * in that wavelet.
 */
export class WaveRef {
  private constructor(
    readonly waveId: WaveId,
    readonly waveletId: WaveletId | null,
    readonly documentId: string | null,
  ) {}

  static of(waveId: WaveId): WaveRef {
    return new WaveRef(waveId, null, null);
  }

  static ofWavelet(waveId: WaveId, waveletId: WaveletId): WaveRef {
    return new WaveRef(waveId, waveletId, null);
  }

  static ofDocument(waveId: WaveId, waveletId: WaveletId, documentId: string): WaveRef {
    return new WaveRef(waveId, waveletId, documentId);
  }

  /** Encodes the reference for use in a URI path. */
  encodeToUriPathSegment(): string {
    return this.encode(encodeToUriPathSegment);
  }

  /** Encodes the reference for use in a URI query string. */
  encodeToUriQueryString(): string {
    return this.encode(encodeToUriQueryString);
  }

  private encode(escape: (s: string) => string): string {
    let out = `${escape(this.waveId.domain)}/${escape(this.waveId.id)}`;
    if (this.waveletId !== null) {
      out +=
        this.waveletId.domain === this.waveId.domain ? "/~" : `/${escape(this.waveletId.domain)}`;
      out += `/${escape(this.waveletId.id)}`;
    }
    if (this.documentId !== null) {
      out += `/${escape(this.documentId)}`;
    }
    return out;
  }

  /**
   * Decodes `waveDomain/waveId[/waveletDomain/waveletId[/documentId]]`.
   * Throws {@link InvalidWaveRefError}.
   *
   * Quirks kept from the original: the wave domain is lower-cased, the
   * document id is not percent-decoded, tokens after the fifth are ignored,
   * and an encoded space decodes to `+`.
   */
  static decodeFromPath(path: string): WaveRef {
    const tokens = split(path, "/");
    const [t0, t1, t2, t3, t4] = tokens;
    if (t0 === undefined || t1 === undefined || tokens.length === 3) {
      throw new InvalidWaveRefError(
        path,
        "Invalid number of tokens in path given to decodeWaveRefFromPath",
      );
    }
    if (t0 === "" || t1 === "") {
      throw new InvalidWaveRefError(path, "The wave domain and thewave Id must not be empty.");
    }
    const waveDomain = decode(t0.toLowerCase());
    const decodedWaveId = decode(t1);
    let waveId: WaveId;
    try {
      waveId = WaveId.of(waveDomain, decodedWaveId);
    } catch (e) {
      if (e instanceof InvalidIdError) {
        throw new InvalidWaveRefError(path, `Invalid WaveID:${e.message}`);
      }
      throw e;
    }
    if (t2 === undefined || t3 === undefined) {
      return WaveRef.of(waveId);
    }
    if (t2 === "" || t3 === "") {
      throw new InvalidWaveRefError(
        path,
        "The wavelet domain and thewavelet Id must not be empty.",
      );
    }
    let waveletDomain = decode(t2);
    if (waveletDomain === "~") {
      waveletDomain = waveDomain;
    }
    const decodedWaveletId = decode(t3);
    let waveletId: WaveletId;
    try {
      waveletId = WaveletId.of(waveletDomain, decodedWaveletId);
    } catch (e) {
      if (e instanceof InvalidIdError) {
        throw new InvalidWaveRefError(path, "Invalid WaveletID");
      }
      throw e;
    }
    return t4 === undefined
      ? WaveRef.ofWavelet(waveId, waveletId)
      : WaveRef.ofDocument(waveId, waveletId, t4);
  }

  equals(other: WaveRef): boolean {
    return (
      this.waveId.equals(other.waveId) &&
      (this.waveletId === null
        ? other.waveletId === null
        : other.waveletId !== null && this.waveletId.equals(other.waveletId)) &&
      this.documentId === other.documentId
    );
  }

  toString(): string {
    let out = `[WaveRef:${this.waveId}`;
    if (this.waveletId !== null) {
      out += `${this.waveletId}`;
    }
    if (this.documentId !== null) {
      out += `[documentId:${this.documentId}]`;
    }
    return out + "]";
  }
}

const SAFE_PATH_CHARS = "-_.!~*'()@:$&,;=+";
const SAFE_QUERY_CHARS = "-_.!~*'()@:$,;/?:+";

/** Percent-escapes a string for use in a URI path segment. */
export function encodeToUriPathSegment(s: string): string {
  return percentEscape(s, SAFE_PATH_CHARS);
}

/** Percent-escapes a string for use in a URI query string. */
export function encodeToUriQueryString(s: string): string {
  return percentEscape(s, SAFE_QUERY_CHARS);
}

/**
 * `URLDecoder.decode` followed by replacing spaces with `+`. Bytes that are
 * not UTF-8 decode to U+FFFD.
 */
function decode(encoded: string): string {
  const bytes = urlDecodeBytes(encoded);
  if (bytes === null) {
    throw new InvalidWaveRefError(encoded, "Failed to decode");
  }
  return utf8DecodeLossy(bytes).replaceAll(" ", "+");
}
