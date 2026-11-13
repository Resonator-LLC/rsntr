// Errors of the document layer (`DocError` in the Rust `doc/mod.rs`; a
// separate module here only to keep the module graph free of cycles).
//
// TypeScript port of the Rust crate `resonator-model` (module `doc`), itself
// derived from `org.waveprotocol.wave.model.document` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { OperationError } from "../docop";

/** The reason for a {@link DocError}. */
export type DocErrorDetail =
  /** A non-invertible operation does not apply to the document. */
  | { readonly kind: "invalidOperation"; readonly message: string }
  | { readonly kind: "invalidLocation"; readonly location: number; readonly size: number }
  /**
   * A node or point that does not belong to the current tree, or is of the
   * wrong kind.
   */
  | { readonly kind: "invalidNode" }
  | { readonly kind: "invalidArgument"; readonly message: string }
  /** The document does not have the structure the caller expects. */
  | { readonly kind: "structure"; readonly message: string }
  /** A document operation error (building, validating or applying). */
  | { readonly kind: "operation"; readonly error: OperationError }
  /** The wavelet rejected the operation; `cause` is the wavelet's error. */
  | { readonly kind: "wavelet"; readonly cause: unknown };

function messageOf(d: DocErrorDetail): string {
  switch (d.kind) {
    case "invalidOperation":
      return `invalid operation: ${d.message}`;
    case "invalidLocation":
      return `location ${d.location} is outside the document of size ${d.size}`;
    case "invalidNode":
      return "node is not valid for this document";
    case "invalidArgument":
      return d.message;
    case "structure":
      return `unexpected document structure: ${d.message}`;
    case "operation":
      return d.error.message;
    case "wavelet":
      return d.cause instanceof Error ? d.cause.message : String(d.cause);
  }
}

function causeOf(d: DocErrorDetail): ErrorOptions | undefined {
  switch (d.kind) {
    case "operation":
      return { cause: d.error };
    case "wavelet":
      return { cause: d.cause };
    default:
      return undefined;
  }
}

/** Failure to read or edit a document. */
export class DocError extends Error {
  readonly detail: DocErrorDetail;

  constructor(detail: DocErrorDetail) {
    super(messageOf(detail), causeOf(detail));
    this.name = "DocError";
    this.detail = detail;
  }

  get kind(): DocErrorDetail["kind"] {
    return this.detail.kind;
  }

  static invalidOperation(message: string): DocError {
    return new DocError({ kind: "invalidOperation", message });
  }

  static invalidLocation(location: number, size: number): DocError {
    return new DocError({ kind: "invalidLocation", location, size });
  }

  static invalidNode(): DocError {
    return new DocError({ kind: "invalidNode" });
  }

  static invalidArgument(message: string): DocError {
    return new DocError({ kind: "invalidArgument", message });
  }

  static structure(message: string): DocError {
    return new DocError({ kind: "structure", message });
  }

  /** Wraps a document operation error (Rust's `From<OperationError>`). */
  static fromOperation(error: OperationError): DocError {
    return new DocError({ kind: "operation", error });
  }

  /** Wraps the error of a wavelet that rejected an operation. */
  static wavelet(cause: unknown): DocError {
    return new DocError({ kind: "wavelet", cause });
  }

  /**
   * What Rust's `?` does in this module: a `DocError` as is, an
   * `OperationError` wrapped; anything else is returned unchanged.
   */
  static lift(error: unknown): unknown {
    return error instanceof OperationError ? DocError.fromOperation(error) : error;
  }
}
