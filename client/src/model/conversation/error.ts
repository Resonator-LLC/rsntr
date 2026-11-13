// Errors of the conversation model.
//
// Derived from `org.waveprotocol.wave.model.conversation` of Apache Wave,
// licensed under the Apache License, Version 2.0.

/** The reason for a {@link ConversationError}. */
export type ConversationErrorDetail =
  /** The wavelet has no conversation manifest. */
  | { readonly kind: "noManifest" }
  /** No such blip in the conversation (never there, or deleted). */
  | { readonly kind: "noSuchBlip"; readonly blipId: string }
  /** No such thread in the conversation (never there, or deleted). */
  | { readonly kind: "noSuchThread"; readonly threadId: string }
  /** The id generated for a new blip is already used by a document. */
  | { readonly kind: "blipExists"; readonly blipId: string }
  | { readonly kind: "invalidArgument"; readonly message: string };

function messageOf(d: ConversationErrorDetail): string {
  switch (d.kind) {
    case "noManifest":
      return "the wavelet has no conversation manifest";
    case "noSuchBlip":
      return `no blip ${d.blipId} in the conversation`;
    case "noSuchThread":
      return `no thread ${d.threadId} in the conversation`;
    case "blipExists":
      return `a document ${d.blipId} already exists`;
    case "invalidArgument":
      return d.message;
  }
}

/**
 * Failure of a conversation operation.
 *
 * Unlike the Rust `ConversationError`, this does not wrap the errors of the
 * layers below: `DocError`, `WaveletError`, `WaveletOperationError` and
 * `InvalidIdError` are thrown as they are.
 */
export class ConversationError extends Error {
  readonly detail: ConversationErrorDetail;

  constructor(detail: ConversationErrorDetail) {
    super(messageOf(detail));
    this.name = "ConversationError";
    this.detail = detail;
  }

  get kind(): ConversationErrorDetail["kind"] {
    return this.detail.kind;
  }

  static noManifest(): ConversationError {
    return new ConversationError({ kind: "noManifest" });
  }

  static noSuchBlip(blipId: string): ConversationError {
    return new ConversationError({ kind: "noSuchBlip", blipId });
  }

  static noSuchThread(threadId: string): ConversationError {
    return new ConversationError({ kind: "noSuchThread", threadId });
  }

  static invalidArgument(message: string): ConversationError {
    return new ConversationError({ kind: "invalidArgument", message });
  }
}
