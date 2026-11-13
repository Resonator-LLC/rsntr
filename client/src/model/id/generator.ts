// Generation of new ids (`IdGenerator`, `IdGeneratorImpl`,
// `TokenGeneratorImpl`).
//
// Derived from the `org.waveprotocol.wave.model.id` package of Apache Wave,
// licensed under the Apache License, Version 2.0.

import {
  BLIP_PREFIX,
  CONVERSATION_ROOT_WAVELET,
  CONVERSATION_WAVELET_PREFIX,
  GHOST_BLIP_PREFIX,
  USER_DATA_WAVELET_PREFIX,
  WAVE_PREFIX,
} from "./constants";
import { InvalidParticipantAddress, ParticipantId } from "./participant";
import * as util from "./util";
import { InvalidIdError, WaveId, WaveletId } from "./waveId";

/**
 * Generates ids for new waves, wavelets and documents. The methods throw
 * {@link InvalidIdError} where a generated id would be invalid (e.g. a bad
 * seed or domain).
 */
export interface IdGenerator {
  newWaveId(): WaveId;
  newConversationWaveletId(): WaveletId;
  newConversationRootWaveletId(): WaveletId;
  buildConversationRootWaveletId(waveId: WaveId): WaveletId;
  newUserDataWaveletId(address: string): WaveletId;
  newBlipId(): string;
  newDataDocumentId(): string;
  newUniqueToken(): string;
  newId(namespace: string): string;
  readonly defaultDomain: string;
}

/**
 * Generates ids of the form `<namespace>+<seed><counter>`, where the seed
 * comes from an injected source and the counter is unique per generator.
 */
export class IdGeneratorImpl implements IdGenerator {
  readonly defaultDomain: string;
  private readonly seed: () => string;
  private counter = 0;

  /**
   * `seed` is called once per generated token; ids are unique only if its
   * values are unique to this generator (the client passes the session id).
   */
  constructor(domain: string, seed: () => string) {
    this.defaultDomain = domain;
    this.seed = seed;
  }

  /**
   * The token the next call of `newUniqueToken` would return, given the
   * same seed value.
   */
  peekUniqueToken(): string {
    return this.seed() + base64EncodeInt(this.counter);
  }

  peekBlipId(): string {
    return util.join([BLIP_PREFIX, this.peekUniqueToken()]);
  }

  newWaveId(): WaveId {
    return WaveId.of(this.defaultDomain, this.newId(WAVE_PREFIX));
  }

  newConversationWaveletId(): WaveletId {
    return WaveletId.of(this.defaultDomain, this.newId(CONVERSATION_WAVELET_PREFIX));
  }

  newConversationRootWaveletId(): WaveletId {
    return WaveletId.of(this.defaultDomain, CONVERSATION_ROOT_WAVELET);
  }

  buildConversationRootWaveletId(waveId: WaveId): WaveletId {
    return WaveletId.of(waveId.domain, CONVERSATION_ROOT_WAVELET);
  }

  newUserDataWaveletId(address: string): WaveletId {
    let participant: ParticipantId;
    try {
      participant = ParticipantId.of(address);
    } catch (e) {
      if (e instanceof InvalidParticipantAddress) {
        throw new InvalidIdError(address, e.reason);
      }
      throw e;
    }
    return WaveletId.of(participant.domain(), util.join([USER_DATA_WAVELET_PREFIX, address]));
  }

  newBlipId(): string {
    return this.newId(BLIP_PREFIX);
  }

  newDataDocumentId(): string {
    const id = this.newUniqueToken();
    if (util.isBlipId(id)) {
      throw new InvalidIdError(id, `generated data document id '${id}' is a blip id`);
    }
    return id;
  }

  newUniqueToken(): string {
    const unique = this.counter;
    this.counter = (this.counter + 1) >>> 0;
    return this.seed() + base64EncodeInt(unique);
  }

  newId(namespace: string): string {
    const id = util.join([namespace, this.newUniqueToken()]);
    const isBlipId = util.isBlipId(id);
    if (namespace === BLIP_PREFIX || namespace === GHOST_BLIP_PREFIX) {
      if (!isBlipId) {
        throw new InvalidIdError(id, `generated blip id '${id}' is not a blip id`);
      }
    } else if (isBlipId) {
      throw new InvalidIdError(id, `generated data document id '${id}' is a blip id`);
    }
    return id;
  }
}

const WEB64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/**
 * Encodes an unsigned 32-bit number in web-safe base 64, most significant
 * digit first, without leading zero digits (`A`).
 */
export function base64EncodeInt(value: number): string {
  const v = value >>> 0;
  const digits = v === 0 ? 1 : Math.ceil((32 - Math.clz32(v)) / 6);
  let out = "";
  for (let i = digits - 1; i >= 0; i--) {
    out += WEB64_ALPHABET.charAt((v >>> (6 * i)) & 0x3f);
  }
  return out;
}

/**
 * Generates random tokens of web-safe base 64 characters from an injected
 * source of random bytes (`TokenGeneratorImpl`).
 */
export class TokenGenerator {
  /** `random` must fill the array it is given with random bytes. */
  constructor(private readonly random: (bytes: Uint8Array) => void) {}

  /** A token of `length` characters; `null` if `length` is zero. */
  generateToken(length: number): string | null {
    if (length <= 0) {
      return null;
    }
    // Every 4 characters represent 3 bytes.
    const bytes = new Uint8Array(Math.ceil(length / 4) * 3);
    this.random(bytes);
    let token = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const n =
        ((bytes[i] as number) << 16) | ((bytes[i + 1] as number) << 8) | (bytes[i + 2] as number);
      token +=
        WEB64_ALPHABET.charAt((n >> 18) & 0x3f) +
        WEB64_ALPHABET.charAt((n >> 12) & 0x3f) +
        WEB64_ALPHABET.charAt((n >> 6) & 0x3f) +
        WEB64_ALPHABET.charAt(n & 0x3f);
    }
    return token.slice(0, length);
  }
}
