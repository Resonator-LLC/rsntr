// Serialised forms of ids: the modern `domain/id` form and the legacy
// `domain!id` form.
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave
// (`IdSerialiser`, `ModernIdSerialiser`, `LegacyIdSerialiser`,
// `DualIdSerialiser`), licensed under the Apache License, Version 2.0.

import { DEFAULT_ESCAPER, EscapeError } from "./escaper";
import { isValidDomain, isValidIdentifier } from "./identifiers";
import { split } from "./java";
import { InvalidIdError, WaveId, WaveletId, WaveletName } from "./waveId";

/** Converts ids to and from strings. All methods throw {@link InvalidIdError}. */
export interface IdSerialiser {
  serialiseWaveId(id: WaveId): string;
  serialiseWaveletId(id: WaveletId): string;
  serialiseWaveletName(name: WaveletName): string;
  deserialiseWaveId(serialised: string): WaveId;
  deserialiseWaveletId(serialised: string): WaveletId;
  deserialiseWaveletName(serialised: string): WaveletName;
}

const SEP = "/";
const ELIDE = "~";

function serialiseModern(domain: string, id: string): string {
  // Ids built with `ofLegacy` may violate these.
  if (!isValidDomain(0, domain)) {
    throw new InvalidIdError(domain, `Invalid domain ${domain}`);
  }
  if (!isValidIdentifier(id)) {
    throw new InvalidIdError(id, `Invalid id ${id}`);
  }
  return `${domain}${SEP}${id}`;
}

function twoTokens(serialised: string, what: string): [string, string] {
  const tokens = split(serialised, SEP);
  const [domain, id] = tokens;
  if (tokens.length !== 2 || domain === undefined || id === undefined) {
    throw new InvalidIdError(
      serialised,
      `Required 2 '/'-separated tokens in serialised ${what}: ${serialised}`,
    );
  }
  return [domain, id];
}

/**
 * The modern serialisation: `domain/id` for ids and
 * `waveDomain/waveId/waveletDomain/waveletId` for wavelet names, where a
 * wavelet domain equal to the wave domain is written `~`.
 */
export class ModernIdSerialiser implements IdSerialiser {
  static readonly INSTANCE = new ModernIdSerialiser();

  serialiseWaveId(id: WaveId): string {
    return serialiseModern(id.domain, id.id);
  }

  serialiseWaveletId(id: WaveletId): string {
    return serialiseModern(id.domain, id.id);
  }

  serialiseWaveletName(name: WaveletName): string {
    const waveletDomain =
      name.waveletId.domain === name.waveId.domain ? ELIDE : name.waveletId.domain;
    // The wavelet id is not re-checked here, as in the original.
    return `${this.serialiseWaveId(name.waveId)}${SEP}${waveletDomain}${SEP}${name.waveletId.id}`;
  }

  deserialiseWaveId(serialised: string): WaveId {
    const [domain, id] = twoTokens(serialised, "wave id");
    return WaveId.of(domain, id);
  }

  deserialiseWaveletId(serialised: string): WaveletId {
    const [domain, id] = twoTokens(serialised, "wavelet id");
    return WaveletId.of(domain, id);
  }

  deserialiseWaveletName(serialised: string): WaveletName {
    if (serialised.endsWith(SEP)) {
      throw new InvalidIdError(serialised, "Serialised wavelet name had trailing '/'");
    }
    const tokens = split(serialised, SEP);
    const [waveDomain, waveId, waveletDomain, waveletId] = tokens;
    if (
      tokens.length !== 4 ||
      waveDomain === undefined ||
      waveId === undefined ||
      waveletDomain === undefined ||
      waveletId === undefined
    ) {
      throw new InvalidIdError(
        serialised,
        `Required 4 '/'-separated tokens in serialised wavelet name: ${serialised}`,
      );
    }
    if (waveletDomain === waveDomain) {
      throw new InvalidIdError(serialised, "Serialised wavelet name had un-normalised domains");
    }
    return WaveletName.of(
      WaveId.of(waveDomain, waveId),
      WaveletId.of(waveletDomain === ELIDE ? waveDomain : waveletDomain, waveletId),
    );
  }
}

function legacyParts(serialised: string, what: string): [string, string] {
  let parts: string[];
  try {
    parts = DEFAULT_ESCAPER.splitWithoutUnescaping(LegacyIdSerialiser.PART_SEPARATOR, serialised);
  } catch (e) {
    if (e instanceof EscapeError) {
      throw new InvalidIdError(serialised, e.message);
    }
    throw e;
  }
  const [domain, id] = parts;
  if (parts.length !== 2 || !domain || !id) {
    throw new InvalidIdError(serialised, `${what} must be of the form <domain>!<id>`);
  }
  return [domain, id];
}

/** Re-attributes an id error to the whole serialised string. */
function forSerialised<T>(serialised: string, f: () => T): T {
  try {
    return f();
  } catch (e) {
    if (e instanceof InvalidIdError) {
      throw new InvalidIdError(serialised, e.reason);
    }
    throw e;
  }
}

function noLegacyNames(): never {
  throw new InvalidIdError("", "No legacy serialization for wavelet names");
}

/** The legacy serialisation, `domain!id`. Wavelet names have no legacy form. */
export class LegacyIdSerialiser implements IdSerialiser {
  static readonly INSTANCE = new LegacyIdSerialiser();
  static readonly PART_SEPARATOR = "!";

  serialiseWaveId(id: WaveId): string {
    return `${id.domain}!${id.id}`;
  }

  serialiseWaveletId(id: WaveletId): string {
    return `${id.domain}!${id.id}`;
  }

  /** Always throws (`UnsupportedOperationException` in the original). */
  serialiseWaveletName(_name: WaveletName): string {
    return noLegacyNames();
  }

  deserialiseWaveId(serialised: string): WaveId {
    const [domain, id] = legacyParts(serialised, "Wave id");
    return forSerialised(serialised, () => WaveId.ofLegacy(domain, id));
  }

  deserialiseWaveletId(serialised: string): WaveletId {
    const [domain, id] = legacyParts(serialised, "Wavelet id");
    return forSerialised(serialised, () => WaveletId.ofLegacy(domain, id));
  }

  /** Always throws (`UnsupportedOperationException` in the original). */
  deserialiseWaveletName(_serialised: string): WaveletName {
    return noLegacyNames();
  }
}

function orElse<T>(first: () => T, second: () => T): T {
  try {
    return first();
  } catch (e) {
    if (e instanceof InvalidIdError) {
      return second();
    }
    throw e;
  }
}

/** Reads both forms (modern first) and writes one of them. */
export class DualIdSerialiser implements IdSerialiser {
  /** Writes the modern form. */
  static readonly MODERN = new DualIdSerialiser(true);
  /** Writes the legacy form. */
  static readonly LEGACY = new DualIdSerialiser(false);

  private readonly modern = new ModernIdSerialiser();
  private readonly legacy = new LegacyIdSerialiser();

  private constructor(private readonly toModern: boolean) {}

  private get writer(): IdSerialiser {
    return this.toModern ? this.modern : this.legacy;
  }

  serialiseWaveId(id: WaveId): string {
    return this.writer.serialiseWaveId(id);
  }

  serialiseWaveletId(id: WaveletId): string {
    return this.writer.serialiseWaveletId(id);
  }

  serialiseWaveletName(name: WaveletName): string {
    return this.writer.serialiseWaveletName(name);
  }

  deserialiseWaveId(serialised: string): WaveId {
    return orElse(
      () => this.modern.deserialiseWaveId(serialised),
      () => this.legacy.deserialiseWaveId(serialised),
    );
  }

  deserialiseWaveletId(serialised: string): WaveletId {
    return orElse(
      () => this.modern.deserialiseWaveletId(serialised),
      () => this.legacy.deserialiseWaveletId(serialised),
    );
  }

  deserialiseWaveletName(serialised: string): WaveletName {
    return orElse(
      () => this.modern.deserialiseWaveletName(serialised),
      () => this.legacy.deserialiseWaveletName(serialised),
    );
  }
}
