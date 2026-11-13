// Errors of the op-based wavelet and the wave view.
//
// Derived from `org.waveprotocol.wave.model.wave.opbased` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import type { ParticipantId, WaveletId } from "../id";

/** The reason for a {@link WaveletError}. */
export type WaveletErrorDetail =
  /**
   * The author is not a participant and the wavelet is read-only for
   * non-participants.
   */
  | { readonly kind: "readOnly"; readonly author: ParticipantId }
  | { readonly kind: "duplicateWavelet"; readonly waveletId: WaveletId }
  | { readonly kind: "noSuchWavelet"; readonly waveletId: WaveletId }
  | { readonly kind: "invalidId"; readonly message: string };

function messageOf(d: WaveletErrorDetail): string {
  switch (d.kind) {
    case "readOnly":
      return `${d.author} is not a participant of the wavelet, which is read only`;
    case "duplicateWavelet":
      return `the wave already has a wavelet ${d.waveletId}`;
    case "noSuchWavelet":
      return `the wave has no wavelet ${d.waveletId}`;
    case "invalidId":
      return `invalid id: ${d.message}`;
  }
}

/**
 * Failure of a wavelet mutation that is not the failure of an operation.
 *
 * Unlike the Rust `WaveletError`, this does not wrap operation and document
 * errors: a wavelet mutation throws `WaveletOperationError` (the operation
 * does not apply) or `DocError` (a document edit is invalid) as they are.
 */
export class WaveletError extends Error {
  readonly detail: WaveletErrorDetail;

  constructor(detail: WaveletErrorDetail) {
    super(messageOf(detail));
    this.name = "WaveletError";
    this.detail = detail;
  }

  get kind(): WaveletErrorDetail["kind"] {
    return this.detail.kind;
  }
}
