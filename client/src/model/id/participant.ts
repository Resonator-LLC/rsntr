// Participant ids (`ParticipantId`, `ParticipantIdUtil`).
//
// Derived from `org.waveprotocol.wave.model.wave.ParticipantId` and
// `ParticipantIdUtil` of Apache Wave, licensed under the Apache License,
// Version 2.0.

import { cmpUtf16 } from "../docop/utf16";
import { isValidDomain } from "./identifiers";
import { split } from "./java";

/** An address that is not a valid participant address. */
export class InvalidParticipantAddress extends Error {
  readonly address: string;
  /** The reason, without the address. */
  readonly reason: string;

  constructor(address: string, reason: string) {
    super(`Invalid address '${address}': ${reason}`);
    this.name = "InvalidParticipantAddress";
    this.address = address;
    this.reason = reason;
  }
}

/** Separates the name from the domain in an address. */
export const DOMAIN_PREFIX = "@";

/**
 * A wave participant, identified by a lower-cased address such as
 * `name@example.com`. A domain participant has an empty name
 * (`@example.com`).
 */
export class ParticipantId {
  readonly address: string;

  private constructor(address: string) {
    this.address = address.toLowerCase();
  }

  /**
   * Creates an id from a validated address: exactly one `@`, which is not
   * the last character. The address is lower-cased. Throws
   * {@link InvalidParticipantAddress}.
   */
  static of(address: string): ParticipantId {
    const i = address.indexOf(DOMAIN_PREFIX);
    if (i < 0) {
      throw new InvalidParticipantAddress(address, "Missing domain prefix: @");
    }
    if (i + 1 >= address.length) {
      throw new InvalidParticipantAddress(address, "Missing domain");
    }
    if (address.lastIndexOf(DOMAIN_PREFIX) !== i) {
      throw new InvalidParticipantAddress(address, "Multiple domain prefixes: @");
    }
    return new ParticipantId(address);
  }

  /**
   * As in the original, the same validation as {@link ParticipantId.of}
   * ("unsafe" because the Java variant throws an unchecked exception). For
   * an id without validation use {@link ParticipantId.newUnchecked}.
   */
  static ofUnsafe(address: string): ParticipantId {
    return ParticipantId.of(address);
  }

  /** {@link ParticipantId.of} for `name@domain`. */
  static ofParts(name: string, domain: string): ParticipantId {
    return ParticipantId.of(`${name}${DOMAIN_PREFIX}${domain}`);
  }

  /**
   * Creates an id from any string, lower-casing it but not validating it
   * (the public constructor of the original). Historic deltas contain
   * addresses that are not valid.
   */
  static newUnchecked(address: string): ParticipantId {
    return new ParticipantId(address);
  }

  /** The placeholder participant `unknown`. */
  static unknown(): ParticipantId {
    return new ParticipantId("unknown");
  }

  /** The part before the `@`; the whole address if there is no `@`. */
  name(): string {
    return split(this.address, DOMAIN_PREFIX)[0] ?? "";
  }

  /** The part after the last `@`; the whole address if there is no `@`. */
  domain(): string {
    const parts = split(this.address, DOMAIN_PREFIX);
    return parts[parts.length - 1] ?? "";
  }

  /**
   * Whether `address` is lower-case and has a non-empty name followed by a
   * valid domain.
   */
  static isNormalizedAddress(address: string): boolean {
    if (address !== address.toLowerCase()) {
      return false;
    }
    const at = address.indexOf(DOMAIN_PREFIX);
    return at > 0 && isValidDomain(at + 1, address);
  }

  /** Whether `address` is a domain address, `@` followed by a valid domain. */
  static isDomainAddress(address: string): boolean {
    return address.startsWith(DOMAIN_PREFIX) && isValidDomain(1, address);
  }

  /**
   * The address of the participant standing for everyone in `domain`.
   * Throws {@link InvalidParticipantAddress} for an invalid domain.
   */
  static makeDomainAddress(domain: string): string {
    if (!isValidDomain(0, domain)) {
      throw new InvalidParticipantAddress(domain, `Invalid domain: ${domain}`);
    }
    return `${DOMAIN_PREFIX}${domain}`;
  }

  /** The participant standing for everyone in `domain`. */
  static sharedDomainParticipant(domain: string): ParticipantId {
    return ParticipantId.of(ParticipantId.makeDomainAddress(domain));
  }

  equals(other: ParticipantId): boolean {
    return this.address === other.address;
  }

  /** Orders by the `@`-separated parts of the address, in UTF-16 order. */
  compareTo(other: ParticipantId): -1 | 0 | 1 {
    const parts = split(this.address, DOMAIN_PREFIX);
    const otherParts = split(other.address, DOMAIN_PREFIX);
    const n = Math.min(parts.length, otherParts.length);
    for (let i = 0; i < n; i++) {
      const diff = cmpUtf16(parts[i] as string, otherParts[i] as string);
      if (diff !== 0) {
        return diff;
      }
    }
    return parts.length < otherParts.length ? -1 : parts.length > otherParts.length ? 1 : 0;
  }

  /** The address. */
  toString(): string {
    return this.address;
  }
}
