// Evaluations of whether a wavelet is wanted (`WantedEvaluation`).
//
// TypeScript port of the Rust crate `resonator-model` (module `supplement`),
// itself derived from `org.waveprotocol.wave.model.supplement`
// (`WantedEvaluation`, `SimpleWantedEvaluation`, `WantedEvaluationSet`,
// `SimpleWantedEvaluationSet`) of Apache Wave, licensed under the Apache
// License, Version 2.0.

import type { WaveletId } from "../id";

/** `String.hashCode`. */
function javaStringHash(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = (Math.imul(h, 31) + s.charCodeAt(i)) | 0;
  }
  return h;
}

/** The fields of a {@link WantedEvaluation}. */
export interface WantedEvaluationFields {
  /**
   * The wavelet judged; `null` when read from an element without a wavelet
   * id.
   */
  readonly waveletId: WaveletId | null;
  /** Address of the participant who added the user to the wavelet. */
  readonly adderAddress: string;
  readonly wanted: boolean;
  /** How sure the agent is, from 0 to 1. */
  readonly certainty: number;
  /** When the judgement was made, in milliseconds since the epoch. */
  readonly timestamp: number;
  readonly agentIdentity: string;
  readonly ignored: boolean;
  readonly comment: string;
}

/**
 * One judgement, by an agent, of whether the user wants a wavelet
 * (`WantedEvaluation`, `SimpleWantedEvaluation`).
 */
export class WantedEvaluation implements WantedEvaluationFields {
  readonly waveletId: WaveletId | null;
  readonly adderAddress: string;
  readonly wanted: boolean;
  readonly certainty: number;
  readonly timestamp: number;
  readonly agentIdentity: string;
  readonly ignored: boolean;
  readonly comment: string;

  constructor(fields: WantedEvaluationFields) {
    this.waveletId = fields.waveletId;
    this.adderAddress = fields.adderAddress;
    this.wanted = fields.wanted;
    this.certainty = fields.certainty;
    this.timestamp = fields.timestamp;
    this.agentIdentity = fields.agentIdentity;
    this.ignored = fields.ignored;
    this.comment = fields.comment;
  }

  /** Field by field; a NaN certainty equals nothing, as in Rust. */
  equals(other: WantedEvaluation): boolean {
    const sameWavelet =
      this.waveletId === null
        ? other.waveletId === null
        : other.waveletId !== null && this.waveletId.equals(other.waveletId);
    return (
      sameWavelet &&
      this.adderAddress === other.adderAddress &&
      this.wanted === other.wanted &&
      this.certainty === other.certainty &&
      this.timestamp === other.timestamp &&
      this.agentIdentity === other.agentIdentity &&
      this.ignored === other.ignored &&
      this.comment === other.comment
    );
  }

  /**
   * The original's `hashCode`
   * (`WantedEvaluation.ImplementationHelper.calculateHashCode`), which its
   * ordering falls back on. A 32-bit signed integer.
   */
  javaHashCode(): number {
    const id = this.waveletId;
    const waveletHash =
      id === null
        ? 1
        : (Math.imul(31, (31 + javaStringHash(id.domain)) | 0) + javaStringHash(id.id)) | 0;
    // `Double.doubleToLongBits`, folded to 32 bits; NaN is canonical.
    const bits = new DataView(new ArrayBuffer(8));
    bits.setFloat64(0, this.certainty);
    const certaintyHash = Number.isNaN(this.certainty)
      ? 0x7ff80000
      : (bits.getInt32(0) ^ bits.getInt32(4)) | 0;
    let result = 17;
    result = Math.imul(result, (37 + waveletHash) | 0);
    result = Math.imul(result, (37 + javaStringHash(this.adderAddress)) | 0);
    result = Math.imul(result, 37 + (this.wanted ? 1 : 0));
    result = Math.imul(result, (37 + certaintyHash) | 0);
    result = Math.imul(result, (37 + javaStringHash(this.comment)) | 0);
    // An int multiplied by a long, narrowed back to int: only the low 32
    // bits of the long matter.
    result = Number(BigInt.asIntN(32, BigInt(result) * (37n + BigInt(this.timestamp))));
    result = Math.imul(result, 37 + (this.ignored ? 1 : 0));
    return Math.imul(result, (37 + javaStringHash(this.agentIdentity)) | 0);
  }

  /**
   * `WantedEvaluation.COMPARATOR`: by certainty, then by timestamp, then
   * by the difference of the hash codes. Like the original this is not a
   * consistent order when that difference overflows. Negative, zero or
   * positive.
   */
  compare(other: WantedEvaluation): number {
    const byCertainty = Math.sign(this.certainty - other.certainty) || 0;
    if (byCertainty !== 0) {
      return byCertainty;
    }
    const byTimestamp = Math.sign(this.timestamp - other.timestamp);
    if (byTimestamp !== 0) {
      return byTimestamp;
    }
    return Math.sign((this.javaHashCode() - other.javaHashCode()) | 0);
  }
}

/**
 * The evaluations of one wavelet (`WantedEvaluationSet`,
 * `SimpleWantedEvaluationSet`).
 *
 * The original keeps the evaluations in a hash set, whose iteration order
 * decides between evaluations that tie; here they are kept in the order
 * given, without duplicates, and the first of a tie wins.
 */
export class WantedEvaluationSet {
  readonly waveletId: WaveletId;
  /** In the order given, without duplicates. */
  readonly evaluations: readonly WantedEvaluation[];

  constructor(waveletId: WaveletId, evaluations: Iterable<WantedEvaluation> = []) {
    const unique: WantedEvaluation[] = [];
    for (const evaluation of evaluations) {
      if (!unique.some((known) => known.equals(evaluation))) {
        unique.push(evaluation);
      }
    }
    this.waveletId = waveletId;
    this.evaluations = unique;
  }

  private defaultEvaluation(): WantedEvaluation {
    return new WantedEvaluation({
      waveletId: this.waveletId,
      adderAddress: "default address",
      wanted: true,
      certainty: 0,
      timestamp: 0,
      agentIdentity: "default",
      ignored: false,
      comment: "default rule",
    });
  }

  /**
   * The greatest evaluation by {@link WantedEvaluation.compare}, or a
   * default one (wanted, certainty zero) if there is none
   * (`getMostCertain`).
   */
  mostCertain(): WantedEvaluation {
    let result: WantedEvaluation | null = null;
    for (const evaluation of this.evaluations) {
      if (result === null || result.compare(evaluation) < 0) {
        result = evaluation;
      }
    }
    return result ?? this.defaultEvaluation();
  }

  /**
   * The evaluation with the latest timestamp, or a default one if there is
   * none (`getMostRecent`).
   */
  mostRecent(): WantedEvaluation {
    let result: WantedEvaluation | null = null;
    for (const evaluation of this.evaluations) {
      if (result === null || evaluation.timestamp > result.timestamp) {
        result = evaluation;
      }
    }
    return result ?? this.defaultEvaluation();
  }

  /** Whether the most recent evaluation says to ignore the wavelet. */
  isIgnored(): boolean {
    return this.mostRecent().ignored;
  }

  /** Whether the most certain evaluation says the wavelet is wanted. */
  isWanted(): boolean {
    return this.mostCertain().wanted;
  }
}
