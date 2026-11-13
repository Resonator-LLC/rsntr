// Wave ids, wavelet ids and wavelet names.
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave
// (`WaveId`, `WaveletId`, `WaveletName`, `InvalidIdException`), licensed
// under the Apache License, Version 2.0.

import { cmpUtf16 } from "../docop/utf16";
import { TOKEN_SEPARATOR } from "./constants";
import { DEFAULT_ESCAPER } from "./escaper";
import { isValidDomain, isValidIdentifier } from "./identifiers";
import { DualIdSerialiser, LegacyIdSerialiser } from "./serialiser";

/**
 * An id that is invalid or cannot be (de)serialised (`InvalidIdException`,
 * and `IllegalArgumentException` from the id factories).
 */
export class InvalidIdError extends Error {
  /** The offending id, domain or serialised string. */
  readonly id: string;
  /** The reason, without the id. */
  readonly reason: string;

  constructor(id: string, reason: string) {
    super(`Invalid id '${id}': ${reason}`);
    this.name = "InvalidIdError";
    this.id = id;
    this.reason = reason;
  }
}

function checkModern(domain: string, id: string): void {
  if (!isValidDomain(0, domain)) {
    throw new InvalidIdError(domain, "Invalid domain");
  }
  if (!isValidIdentifier(id)) {
    throw new InvalidIdError(id, "Invalid id");
  }
}

function checkLegacy(domain: string, id: string): void {
  if (domain.length === 0) {
    throw new InvalidIdError(domain, "Empty domain");
  }
  if (id.length === 0) {
    throw new InvalidIdError(id, "Empty id");
  }
  if (DEFAULT_ESCAPER.hasEscapeCharacters(domain)) {
    throw new InvalidIdError(
      domain,
      `Domain cannot contain characters that requires escaping: ${domain}`,
    );
  }
  if (!DEFAULT_ESCAPER.isEscapedProperly(TOKEN_SEPARATOR, id)) {
    throw new InvalidIdError(id, `Id is not properly escaped: ${id}`);
  }
}

type Order = -1 | 0 | 1;

function compareParts(aDomain: string, aId: string, bDomain: string, bId: string): Order {
  const d = cmpUtf16(aDomain, bDomain);
  return d !== 0 ? d : cmpUtf16(aId, bId);
}

/** Identifies a wave: a wave provider domain and an id unique within it. */
export class WaveId {
  private constructor(
    readonly domain: string,
    readonly id: string,
  ) {}

  /**
   * Creates an id from a domain and a local identifier, checking both
   * against the current validity rules (`of` and `ofChecked` in the
   * original). Throws {@link InvalidIdError}.
   */
  static of(domain: string, id: string): WaveId {
    checkModern(domain, id);
    return new WaveId(domain, id);
  }

  /**
   * Creates an id under the looser legacy rules: both parts non-empty, no
   * `~`, `+` or `!` in the domain, and the id properly escaped. The result
   * may not be serialisable in the modern form.
   */
  static ofLegacy(domain: string, id: string): WaveId {
    checkLegacy(domain, id);
    return new WaveId(domain, id);
  }

  /**
   * Deserialises a wave id in the modern (`domain/id`) or, failing that,
   * the legacy (`domain!id`) form.
   */
  static deserialise(s: string): WaveId {
    return DualIdSerialiser.MODERN.deserialiseWaveId(s);
  }

  /**
   * Serialises in the modern form, `domain/id`. Throws
   * {@link InvalidIdError} for a legacy id that the modern rules reject.
   */
  serialise(): string {
    return DualIdSerialiser.MODERN.serialiseWaveId(this);
  }

  equals(other: WaveId): boolean {
    return this.domain === other.domain && this.id === other.id;
  }

  /** Orders by domain, then id, in UTF-16 order. */
  compareTo(other: WaveId): Order {
    return compareParts(this.domain, this.id, other.domain, other.id);
  }

  /** Formats like the original `toString`, e.g. `[WaveId example.com/w+abc]`. */
  toString(): string {
    return `[WaveId ${this.domain}/${this.id}]`;
  }
}

/**
 * Identifies a wavelet within a wave: a domain and an id unique within the
 * wave.
 */
export class WaveletId {
  private constructor(
    readonly domain: string,
    readonly id: string,
  ) {}

  /** As {@link WaveId.of}. */
  static of(domain: string, id: string): WaveletId {
    checkModern(domain, id);
    return new WaveletId(domain, id);
  }

  /** As {@link WaveId.ofLegacy}. */
  static ofLegacy(domain: string, id: string): WaveletId {
    checkLegacy(domain, id);
    return new WaveletId(domain, id);
  }

  /** Deserialises a wavelet id in the legacy form, `domain!id`. */
  static deserialise(s: string): WaveletId {
    return new LegacyIdSerialiser().deserialiseWaveletId(s);
  }

  /**
   * Serialises in the legacy form, `domain!id` (unlike
   * {@link WaveId.serialise}; the asymmetry is in the original).
   */
  serialise(): string {
    return `${this.domain}!${this.id}`;
  }

  equals(other: WaveletId): boolean {
    return this.domain === other.domain && this.id === other.id;
  }

  /** Orders by domain, then id, in UTF-16 order. */
  compareTo(other: WaveletId): Order {
    return compareParts(this.domain, this.id, other.domain, other.id);
  }

  /** E.g. `[WaveletId example.com/conv+root]`. */
  toString(): string {
    return `[WaveletId ${this.domain}/${this.id}]`;
  }
}

/** The globally unique name of a wavelet: a wave id and a wavelet id. */
export class WaveletName {
  private constructor(
    readonly waveId: WaveId,
    readonly waveletId: WaveletId,
  ) {}

  static of(waveId: WaveId, waveletId: WaveletId): WaveletName {
    return new WaveletName(waveId, waveletId);
  }

  /** Throws {@link InvalidIdError} if either id is invalid. */
  static ofParts(
    waveDomain: string,
    waveId: string,
    waveletDomain: string,
    waveletId: string,
  ): WaveletName {
    return new WaveletName(WaveId.of(waveDomain, waveId), WaveletId.of(waveletDomain, waveletId));
  }

  equals(other: WaveletName): boolean {
    return this.waveId.equals(other.waveId) && this.waveletId.equals(other.waveletId);
  }

  compareTo(other: WaveletName): Order {
    const d = this.waveId.compareTo(other.waveId);
    return d !== 0 ? d : this.waveletId.compareTo(other.waveletId);
  }

  /** `[WaveletName waveDomain/waveId/waveletDomain/waveletId]`. */
  toString(): string {
    return (
      `[WaveletName ${this.waveId.domain}/${this.waveId.id}/` +
      `${this.waveletId.domain}/${this.waveletId.id}]`
    );
  }
}
