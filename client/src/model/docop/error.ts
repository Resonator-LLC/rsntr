// Error types for document operations.
//
// Derived from the `org.waveprotocol.wave.model.document.operation` package
// of Apache Wave, licensed under the Apache License, Version 2.0.

/** The reason for an {@link OperationError}. */
export type OperationErrorDetail =
  /** The operation violates the well-formedness rules of the automaton. */
  | { readonly kind: "illFormed"; readonly violation: string; readonly op: string }
  /** Two operations cannot be composed component-wise. */
  | { readonly kind: "illegalComposition" }
  /**
   * The output length of the first operation differs from the input length
   * of the second.
   */
  | { readonly kind: "sizeMismatch"; readonly resulting: number; readonly initial: number }
  /**
   * An attribute or annotation update does not match the state it is applied
   * to or composed with.
   */
  | { readonly kind: "mismatchedOldValue"; readonly message: string }
  /** A map or boundary was constructed from invalid arguments. */
  | { readonly kind: "invalidArgument"; readonly message: string }
  /** A UTF-16 offset falls inside a surrogate pair. */
  | { readonly kind: "surrogateSplit"; readonly offset: number }
  /** A UTF-16 offset lies beyond the end of the string. */
  | { readonly kind: "offsetOutOfBounds"; readonly offset: number; readonly len: number }
  /** The operation is not valid for the document it is applied to. */
  | { readonly kind: "validation"; readonly message: string }
  /** The operation does not apply to the document. */
  | { readonly kind: "apply"; readonly message: string }
  /**
   * The operation contains components that are not allowed in a document
   * initialization.
   */
  | { readonly kind: "notAnInitialization"; readonly component: string };

function operationErrorMessage(d: OperationErrorDetail): string {
  switch (d.kind) {
    case "illFormed":
      return `ill-formed operation (${d.violation}): ${d.op}`;
    case "illegalComposition":
      return "illegal composition";
    case "sizeMismatch":
      return `document size mismatch: op1 resulting length=${d.resulting}, op2 initial length=${d.initial}`;
    case "mismatchedOldValue":
      return `mismatched old value: ${d.message}`;
    case "invalidArgument":
      return `invalid argument: ${d.message}`;
    case "surrogateSplit":
      return `utf-16 offset ${d.offset} splits a surrogate pair`;
    case "offsetOutOfBounds":
      return `utf-16 offset ${d.offset} out of bounds for string of length ${d.len}`;
    case "validation":
      return `validation failed: ${d.message}`;
    case "apply":
      return `operation does not apply: ${d.message}`;
    case "notAnInitialization":
      return `initialization with unexpected component ${d.component}`;
  }
}

/**
 * Failure to build, apply, compose or invert a document operation.
 *
 * Covers every case where the Java original throws `OperationException`,
 * `IllegalArgumentException` or `IllegalStateException` on bad input.
 */
export class OperationError extends Error {
  readonly detail: OperationErrorDetail;

  constructor(detail: OperationErrorDetail) {
    super(operationErrorMessage(detail));
    this.name = "OperationError";
    this.detail = detail;
  }

  get kind(): OperationErrorDetail["kind"] {
    return this.detail.kind;
  }

  static illegalComposition(): OperationError {
    return new OperationError({ kind: "illegalComposition" });
  }

  static mismatchedOldValue(message: string): OperationError {
    return new OperationError({ kind: "mismatchedOldValue", message });
  }

  static invalidArgument(message: string): OperationError {
    return new OperationError({ kind: "invalidArgument", message });
  }

  static doesNotApply(message: string): OperationError {
    return new OperationError({ kind: "apply", message });
  }

  static notAnInitialization(component: string): OperationError {
    return new OperationError({ kind: "notAnInitialization", component });
  }
}

/** The reason for a {@link TransformError}. */
export type TransformErrorDetail =
  /** One operation spans more of the document than the other. */
  | {
      readonly kind: "ranOut";
      readonly this: string;
      readonly other: string;
      readonly otherSize: number;
      readonly index: number;
      readonly size: number;
      readonly spare: number;
    }
  /** The operations disagree about the document they apply to. */
  | { readonly kind: "incompatible" }
  /** A component appeared where the decomposition forbids it. */
  | { readonly kind: "unexpectedComponent"; readonly component: string; readonly op: string }
  /** An operation error raised while transforming. */
  | { readonly kind: "operation"; readonly error: OperationError };

function transformErrorMessage(d: TransformErrorDetail): string {
  switch (d.kind) {
    case "ranOut":
      return (
        `ran out of ${d.otherSize} ${d.other} op components after ${d.index} of ${d.size} ` +
        `${d.this} op components, with ${d.spare} spare positions`
      );
    case "incompatible":
      return "incompatible operations in transformation";
    case "unexpectedComponent":
      return `unexpected ${d.component} component in ${d.op} operation`;
    case "operation":
      return d.error.message;
  }
}

/** Failure to transform a pair of concurrent operations. */
export class TransformError extends Error {
  readonly detail: TransformErrorDetail;

  constructor(detail: TransformErrorDetail) {
    super(
      transformErrorMessage(detail),
      detail.kind === "operation" ? { cause: detail.error } : undefined,
    );
    this.name = "TransformError";
    this.detail = detail;
  }

  get kind(): TransformErrorDetail["kind"] {
    return this.detail.kind;
  }

  static incompatible(): TransformError {
    return new TransformError({ kind: "incompatible" });
  }

  /** Wraps an operation error (Rust's `From<OperationError>`). */
  static fromOperation(error: OperationError): TransformError {
    return new TransformError({ kind: "operation", error });
  }
}
