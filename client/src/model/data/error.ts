// Errors of the wavelet data API.
//
// Derived from `org.waveprotocol.wave.model.wave.data` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import type { OperationError } from "../docop";

/** The reason for a {@link DataError}. */
export type DataErrorDetail =
  | { readonly kind: "duplicateDocument"; readonly docId: string }
  | { readonly kind: "positionOutOfBounds"; readonly position: number; readonly size: number }
  /** The initial content of a document is not valid. */
  | { readonly kind: "content"; readonly error: OperationError };

function dataErrorMessage(d: DataErrorDetail): string {
  switch (d.kind) {
    case "duplicateDocument":
      return `Duplicate doc id: ${d.docId}`;
    case "positionOutOfBounds":
      return `position ${d.position} out of bounds for ${d.size} participants`;
    case "content":
      return `invalid document content: ${d.error.message}`;
  }
}

/**
 * Misuse of the wavelet data API (`IllegalArgumentException` and
 * `IndexOutOfBoundsException` in the original).
 */
export class DataError extends Error {
  readonly detail: DataErrorDetail;

  constructor(detail: DataErrorDetail) {
    super(dataErrorMessage(detail), detail.kind === "content" ? { cause: detail.error } : undefined);
    this.name = "DataError";
    this.detail = detail;
  }

  get kind(): DataErrorDetail["kind"] {
    return this.detail.kind;
  }
}
