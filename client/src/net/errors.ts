// Error types of the network layer.
//
// The server messages quoted here are those of the resonator server, which
// keeps the texts of the servlets of Apache Wave (Apache License 2.0).

/** A request did not get a response: no network, refused, aborted. */
export class NetworkError extends Error {
  constructor(message: string, cause?: unknown) {
    super(message, { cause });
    this.name = "NetworkError";
  }
}

/** An HTTP response with a status the request did not expect. */
export class HttpError extends Error {
  readonly status: number;
  /** The response body as text (possibly empty). */
  readonly body: string;
  readonly url: string;

  constructor(status: number, body: string, url: string) {
    super(`HTTP ${status} from ${url}${body === "" ? "" : `: ${body.slice(0, 200)}`}`);
    this.name = "HttpError";
    this.status = status;
    this.body = body;
    this.url = url;
  }
}

/** A successful HTTP response whose body is not what the endpoint returns. */
export class InvalidResponseError extends Error {
  readonly url: string;
  readonly body: string;

  constructor(url: string, body: string, cause?: unknown) {
    const reason = cause instanceof Error ? `: ${cause.message}` : "";
    super(`unexpected response body from ${url}${reason}`, { cause });
    this.name = "InvalidResponseError";
    this.url = url;
    this.body = body;
  }
}

/** The reason for an {@link AuthError}. `message` is the server's text. */
export type AuthErrorDetail =
  /** Sign-in refused: wrong address or password (403). */
  | { readonly kind: "invalidCredentials"; readonly message: string }
  /** Too many attempts (429); `retryAfterSecs` from `Retry-After`, if given. */
  | {
      readonly kind: "rateLimited";
      readonly retryAfterSecs: number | null;
      readonly message: string;
    }
  /** Registration is disabled by the administrator (403). */
  | { readonly kind: "registrationDisabled"; readonly message: string }
  /** Registration refused: the account exists (403). */
  | { readonly kind: "accountExists"; readonly message: string }
  /**
   * Refused for another reason the server states (403): an invalid
   * address, a password too short or too long.
   */
  | { readonly kind: "failed"; readonly message: string }
  /** The server could not read the request (400). */
  | { readonly kind: "badRequest"; readonly message: string }
  /**
   * The server's cross-site check refused a state-changing request (403):
   * the request's `Origin` is neither the server nor an allowed origin.
   */
  | { readonly kind: "crossSite"; readonly message: string }
  /** The bearer token is invalid or expired (401). */
  | { readonly kind: "unauthorized"; readonly message: string }
  /**
   * Nobody is signed in, or the signed-in user may not see what was asked
   * for (403 from a data endpoint; the server does not tell the two apart).
   */
  | { readonly kind: "forbidden"; readonly message: string }
  /** No user is signed in (decided by the client from `GET /session`). */
  | { readonly kind: "notSignedIn"; readonly message: string };

/** A request refused for reasons of authentication or authorisation. */
export class AuthError extends Error {
  readonly detail: AuthErrorDetail;

  constructor(detail: AuthErrorDetail) {
    super(detail.message === "" ? detail.kind : detail.message);
    this.name = "AuthError";
    this.detail = detail;
  }

  get kind(): AuthErrorDetail["kind"] {
    return this.detail.kind;
  }
}

/** How a WebSocket connection ended. */
export interface CloseInfo {
  /** The WebSocket close code; 1006 when there was no close frame. */
  readonly code: number;
  /** The close reason of the server, or a description of a local cause. */
  readonly reason: string;
  /**
   * `"remote"` for a close observed on the socket, `"local"` when the
   * client gave the connection up itself (failed or timed-out connect,
   * `dropConnection`, going offline, `close`).
   */
  readonly origin: "remote" | "local";
  /** The error behind a local cause, if any. */
  readonly cause?: unknown;
}

/** The server answered a request with `RpcFinished` failed. */
export class RpcFailedError extends Error {
  /** The server's error text. */
  readonly errorText: string;

  constructor(errorText: string) {
    super(errorText);
    this.name = "RpcFailedError";
    this.errorText = errorText;
  }
}

/**
 * The channel is not connected, or the connection dropped before the
 * response arrived. Nothing is queued or replayed.
 */
export class DisconnectedError extends Error {
  /** How the connection ended; null if there was none. */
  readonly close: CloseInfo | null;

  constructor(close: CloseInfo | null) {
    super(close === null ? "not connected" : `disconnected: ${close.code} ${close.reason}`.trim());
    this.name = "DisconnectedError";
    this.close = close;
  }
}

/** The channel was closed for good, by `close()` or a fatal error. */
export class ClosedError extends Error {
  constructor(cause?: unknown) {
    super("the channel is closed", { cause });
    this.name = "ClosedError";
  }
}

/** The reason for an {@link RpcFatalError}. */
export type RpcFatalDetail =
  /** The server closed the connection because the session ended. */
  | { readonly kind: "signedOut"; readonly close: CloseInfo }
  /** The server refused the socket token; `message` is its text. */
  | { readonly kind: "authenticationFailed"; readonly message: string }
  /** The server saw no valid credential within its time limit. */
  | { readonly kind: "authenticationTimeout"; readonly close: CloseInfo }
  /** `getAuthToken` failed with an authentication error. */
  | { readonly kind: "authTokenUnavailable"; readonly cause: unknown };

function fatalMessage(d: RpcFatalDetail): string {
  switch (d.kind) {
    case "signedOut":
      return "signed out";
    case "authenticationFailed":
      return `authentication failed: ${d.message}`;
    case "authenticationTimeout":
      return "authentication timeout";
    case "authTokenUnavailable":
      return `no credential for the connection: ${
        d.cause instanceof Error ? d.cause.message : String(d.cause)
      }`;
  }
}

/** The channel cannot go on without the user signing in again. */
export class RpcFatalError extends Error {
  readonly detail: RpcFatalDetail;

  constructor(detail: RpcFatalDetail) {
    super(fatalMessage(detail), "cause" in detail ? { cause: detail.cause } : undefined);
    this.name = "RpcFatalError";
    this.detail = detail;
  }

  get kind(): RpcFatalDetail["kind"] {
    return this.detail.kind;
  }
}

/** The reason for an {@link RpcProtocolError}. */
export type RpcProtocolErrorDetail =
  /** A frame that is not text, not an envelope, or has an undecodable body. */
  | {
      readonly kind: "undecodableFrame";
      /** The sequence number, if the envelope could be read. */
      readonly sequenceNumber: number | null;
      readonly cause: unknown;
    }
  /** A response of a type the request does not have. */
  | {
      readonly kind: "unexpectedMessage";
      readonly sequenceNumber: number;
      readonly messageType: string;
      readonly expected: string;
    }
  /** The server closed the connection over a protocol violation. */
  | { readonly kind: "protocolClose"; readonly close: CloseInfo };

function protocolMessage(d: RpcProtocolErrorDetail): string {
  switch (d.kind) {
    case "undecodableFrame":
      return `undecodable frame${
        d.sequenceNumber === null ? "" : ` for request ${d.sequenceNumber}`
      }: ${d.cause instanceof Error ? d.cause.message : String(d.cause)}`;
    case "unexpectedMessage":
      return `unexpected ${d.messageType} for request ${d.sequenceNumber}, expected ${d.expected}`;
    case "protocolClose":
      return `connection closed over a protocol error: ${d.close.code} ${d.close.reason}`.trim();
  }
}

/** The two ends disagree about the protocol. Not fatal by itself. */
export class RpcProtocolError extends Error {
  readonly detail: RpcProtocolErrorDetail;

  constructor(detail: RpcProtocolErrorDetail) {
    super(protocolMessage(detail), "cause" in detail ? { cause: detail.cause } : undefined);
    this.name = "RpcProtocolError";
    this.detail = detail;
  }

  get kind(): RpcProtocolErrorDetail["kind"] {
    return this.detail.kind;
  }
}
