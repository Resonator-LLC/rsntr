// Errors of the document-based data types (`AdtError` in the Rust
// `adt/mod.rs`; a separate module here only to keep the module graph free of
// cycles).
//
// TypeScript port of the Rust crate `resonator-model` (module `adt`), itself
// derived from `org.waveprotocol.wave.model.adt.docbased` of Apache Wave,
// licensed under the Apache License, Version 2.0.

/** The reason for an {@link AdtError}. */
export type AdtErrorDetail =
  /** The element that should hold the data is not in the document. */
  | { readonly kind: "missingContainer" }
  /** `Initializer.Helper.initialiseAttribute` met a key twice. */
  | { readonly kind: "duplicateInitialiserKey"; readonly key: string }
  /**
   * A list index beyond the end of the list (the original throws
   * `IndexOutOfBoundsException`).
   */
  | { readonly kind: "indexOutOfBounds"; readonly index: number; readonly size: number }
  /**
   * A value that has no attribute form (the original throws
   * `IllegalArgumentException`).
   */
  | { readonly kind: "unserialisable"; readonly message: string };

function messageOf(d: AdtErrorDetail): string {
  switch (d.kind) {
    case "missingContainer":
      return "the container element is not in the document";
    case "duplicateInitialiserKey":
      return `Duplicate initialiser key '${d.key}' in initialiser`;
    case "indexOutOfBounds":
      return `index ${d.index} is out of bounds for a list of ${d.size}`;
    case "unserialisable":
      return `value cannot be serialised: ${d.message}`;
  }
}

/**
 * Failure of a write to a document-based data type. Failures of the
 * document itself surface as `DocError` (Rust: `AdtError::Document`).
 */
export class AdtError extends Error {
  readonly detail: AdtErrorDetail;

  constructor(detail: AdtErrorDetail) {
    super(messageOf(detail));
    this.name = "AdtError";
    this.detail = detail;
  }

  get kind(): AdtErrorDetail["kind"] {
    return this.detail.kind;
  }

  static missingContainer(): AdtError {
    return new AdtError({ kind: "missingContainer" });
  }

  static duplicateInitialiserKey(key: string): AdtError {
    return new AdtError({ kind: "duplicateInitialiserKey", key });
  }

  static indexOutOfBounds(index: number, size: number): AdtError {
    return new AdtError({ kind: "indexOutOfBounds", index, size });
  }

  static unserialisable(message: string): AdtError {
    return new AdtError({ kind: "unserialisable", message });
  }
}
