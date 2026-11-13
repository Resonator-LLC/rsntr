// The client RPC channel: the wave protocol over one WebSocket, kept
// connected with backoff.
//
// Takes the place of `WaveWebSocketClient` and `WaveSocketFactory` of
// `org.waveprotocol.box.webclient.client` in Apache Wave, licensed under the
// Apache License, Version 2.0, against the protocol of the resonator server
// (`crates/resonator-server/src/rpc/mod.rs`):
//
// - Every text frame is one envelope `{"messageType", "sequenceNumber",
//   "message"}`. The client numbers its requests; a response carries the
//   number of its request.
// - A request is answered by one response message, a stream by messages
//   until `RpcFinished`, and anything the server cannot serve by
//   `RpcFinished` with `failed` true and a text.
// - A connection not authenticated by cookie at the upgrade starts with a
//   `ProtocolAuthenticate` frame carrying a token.
//
// Unlike the original, nothing is queued while disconnected and nothing is
// replayed after a reconnect: requests fail fast, and every drop is
// reported to each pending call and open stream. The layer above re-opens
// its streams from the versions it knows and resubmits.
//
// Frames are handled one at a time, synchronously, in arrival order, and so
// are the callbacks they lead to: a submit response and the stream updates
// around it are delivered in the order the server sent them. That holds for
// `callWithCallback` and `openStream`, not for the promise of `call`, which
// settles in a later microtask.

import { type Observable, Emitter } from "../util/events";
import {
  CancelRpc,
  type Envelope,
  type MessageCodec,
  ProtocolAuthenticate,
  ProtocolAuthenticationResult,
  RpcFinished,
  encodeEnvelope,
  parseEnvelope,
} from "../wire";
import {
  AuthError,
  type CloseInfo,
  ClosedError,
  DisconnectedError,
  RpcFailedError,
  RpcFatalError,
  type RpcFatalDetail,
  RpcProtocolError,
} from "./errors";

/** The part of a WebSocket the channel uses. */
export interface SocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: (() => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: ((event: { code: number; reason: string }) => void) | null;
  onerror: ((event: unknown) => void) | null;
}

/** Opens a socket to a `ws:` or `wss:` URL. */
export type SocketFactory = (url: string) => SocketLike;

/**
 * - `idle`: not started.
 * - `connecting`: resolving the address and credential, or opening the socket.
 * - `authenticating`: waiting for the answer to `ProtocolAuthenticate`.
 * - `connected`: requests can be sent.
 * - `waiting`: disconnected; the next attempt is scheduled.
 * - `offline`: disconnected; waiting for the network to come back.
 * - `closed`: ended for good, by `close()` or a fatal error.
 */
export type ConnectionState =
  | "idle"
  | "connecting"
  | "authenticating"
  | "connected"
  | "waiting"
  | "offline"
  | "closed";

/** A change of {@link RpcChannel.state}. */
export interface StateChange {
  readonly state: ConnectionState;
  readonly previous: ConnectionState;
  /** How the connection ended, when the change follows from that. */
  readonly detail?: CloseInfo;
  /** In state `waiting`: the delay before the next attempt. */
  readonly retryInMs?: number;
}

export interface RpcChannelEvents {
  state: StateChange;
  /** No more reconnects: the user has to sign in again. */
  fatal: RpcFatalError;
  /** An undecodable or unexpected frame, or a close over a protocol error. */
  protocolError: RpcProtocolError;
}

/** Whether the device has a network. */
export interface NetworkStatus {
  isOnline(): boolean;
  /** Calls `listener` on every change; returns the unsubscribe function. */
  subscribe(listener: (online: boolean) => void): () => void;
}

export interface TimerApi {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface BackoffOptions {
  /** The delay before the first retry. Default 500. */
  readonly initialMs?: number;
  /** The largest delay. Default 30000. */
  readonly maxMs?: number;
  /** The growth per failed attempt. Default 2. */
  readonly factor?: number;
  /** The random spread, as a fraction: 0.3 is plus or minus 30%. Default 0.3. */
  readonly jitter?: number;
}

export type LogLevel = "debug" | "info" | "warn" | "error";

export interface Logger {
  log(level: LogLevel, message: string, detail?: unknown): void;
}

export interface RpcChannelOptions {
  /** The WebSocket address, resolved before every connection attempt. */
  readonly url: string | (() => string | Promise<string>);
  /** Default: the global `WebSocket`. */
  readonly socketFactory?: SocketFactory;
  /**
   * Called before every connection attempt. A token is sent in a
   * `ProtocolAuthenticate` frame; `undefined` means the WebSocket upgrade
   * itself authenticates (the cookie of a same-origin browser). A rejection
   * that `isAuthError` accepts means the user is signed out and is fatal;
   * any other rejection is retried with backoff.
   */
  readonly getAuthToken?: () => Promise<string | undefined> | string | undefined;
  /** Default: an instance of `AuthError`. */
  readonly isAuthError?: (error: unknown) => boolean;
  readonly backoff?: BackoffOptions;
  /**
   * How long an attempt may take from its start to `connected` before it
   * is given up and retried. Default 30000; 0 for no limit.
   */
  readonly connectTimeoutMs?: number;
  /** Default: the global timers. */
  readonly timers?: TimerApi;
  /** A number in [0, 1), for the jitter. Default `Math.random`. */
  readonly random?: () => number;
  /** Default: the browser's `navigator.onLine` and events; else always online. */
  readonly network?: NetworkStatus;
  readonly logger?: Logger;
}

/** The callbacks of a server stream. All run inside the frame handler. */
export interface StreamHandlers<Res> {
  onMessage(message: Res): void;
  /**
   * The server finished the stream (`RpcFinished`): `error` is undefined
   * for a clean end, as after `cancel()`, else the server's text. Also
   * called, with a text, if the channel has to give the stream up because
   * one of its frames cannot be decoded.
   */
  onFinished(error?: string): void;
  /** The connection dropped; the stream is gone and `onFinished` will not follow. */
  onDisconnected(info: CloseInfo): void;
}

export interface StreamHandle {
  readonly sequenceNumber: number;
  /**
   * Asks the server to end the stream (`CancelRpc`). No message is
   * delivered after this; `onFinished` still follows when the server
   * confirms, or `onDisconnected` if the connection drops first.
   */
  cancel(): void;
}

export interface CallHandle {
  readonly sequenceNumber: number;
}

/** What the server names when it cannot check a credential right now. */
const TRANSIENT_AUTH_FAILURE = "Internal error";

interface PendingCall {
  readonly kind: "call";
  readonly responseName: string;
  readonly decode: (body: unknown) => unknown;
  readonly onResponse: (message: unknown) => void;
  readonly onError: (error: Error) => void;
  /** Whether a response is interim: the call goes on after it. */
  readonly interim?: (message: unknown) => boolean;
}

interface PendingStream {
  readonly kind: "stream";
  readonly responseName: string;
  readonly decode: (body: unknown) => unknown;
  readonly handlers: StreamHandlers<unknown>;
  cancelled: boolean;
}

type Pending = PendingCall | PendingStream;

/** {@link NetworkStatus} of the browser; always online elsewhere. */
export function defaultNetworkStatus(): NetworkStatus {
  const scope = globalThis as {
    navigator?: { onLine?: unknown };
    addEventListener?: (type: string, listener: () => void) => void;
    removeEventListener?: (type: string, listener: () => void) => void;
  };
  const add = scope.addEventListener;
  const remove = scope.removeEventListener;
  if (
    typeof scope.navigator?.onLine !== "boolean" ||
    typeof add !== "function" ||
    typeof remove !== "function"
  ) {
    return { isOnline: () => true, subscribe: () => () => {} };
  }
  return {
    isOnline: () => scope.navigator?.onLine !== false,
    subscribe(listener) {
      const online = (): void => listener(true);
      const offline = (): void => listener(false);
      add.call(scope, "online", online);
      add.call(scope, "offline", offline);
      return () => {
        remove.call(scope, "online", online);
        remove.call(scope, "offline", offline);
      };
    },
  };
}

function defaultSocketFactory(): SocketFactory {
  const constructor = (globalThis as { WebSocket?: new (url: string) => unknown }).WebSocket;
  if (typeof constructor !== "function") {
    throw new TypeError("no WebSocket implementation: pass a socketFactory");
  }
  return (url) => new constructor(url) as SocketLike;
}

function defaultTimers(): TimerApi {
  return {
    setTimeout: (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
    clearTimeout: (handle) => globalThis.clearTimeout(handle as Parameters<typeof clearTimeout>[0]),
  };
}

const local = (reason: string, cause?: unknown): CloseInfo => ({
  code: 1006,
  reason,
  origin: "local",
  ...(cause !== undefined ? { cause } : {}),
});

/**
 * The RPC channel. Create it, subscribe to {@link RpcChannel.events}, call
 * {@link RpcChannel.connect}; it then stays connected until
 * {@link RpcChannel.close} or a fatal error.
 */
export class RpcChannel {
  private readonly url: string | (() => string | Promise<string>);
  private readonly socketFactory: SocketFactory;
  private readonly getAuthToken:
    | (() => Promise<string | undefined> | string | undefined)
    | undefined;
  private readonly isAuthError: (error: unknown) => boolean;
  private readonly initialMs: number;
  private readonly maxMs: number;
  private readonly factor: number;
  private readonly jitter: number;
  private readonly connectTimeoutMs: number;
  private readonly timers: TimerApi;
  private readonly random: () => number;
  private readonly network: NetworkStatus;
  private readonly logger: Logger | undefined;

  private readonly emitter: Emitter<RpcChannelEvents>;
  private currentState: ConnectionState = "idle";
  private socket: SocketLike | null = null;
  /** Bumped whenever an attempt or connection ends: stale callbacks check it. */
  private generation = 0;
  private nextSequenceNumber = 1;
  private readonly pending = new Map<number, Pending>();
  /** Failed attempts since the last connection that worked. */
  private failures = 0;
  /** Whether the current connection still has to prove itself by a frame. */
  private unproven = false;
  private retryTimer: unknown = null;
  private connectTimer: unknown = null;
  private unsubscribeNetwork: (() => void) | null = null;
  private fatalError: RpcFatalError | null = null;
  private waiters: { resolve: () => void; reject: (error: Error) => void }[] = [];

  constructor(options: RpcChannelOptions) {
    this.url = options.url;
    this.socketFactory = options.socketFactory ?? defaultSocketFactory();
    this.getAuthToken = options.getAuthToken;
    this.isAuthError = options.isAuthError ?? ((error) => error instanceof AuthError);
    this.initialMs = options.backoff?.initialMs ?? 500;
    this.maxMs = options.backoff?.maxMs ?? 30_000;
    this.factor = options.backoff?.factor ?? 2;
    this.jitter = options.backoff?.jitter ?? 0.3;
    this.connectTimeoutMs = options.connectTimeoutMs ?? 30_000;
    this.timers = options.timers ?? defaultTimers();
    this.random = options.random ?? Math.random;
    this.network = options.network ?? defaultNetworkStatus();
    this.logger = options.logger;
    // A listener that throws must not disturb the channel.
    this.emitter = new Emitter<RpcChannelEvents>((error) =>
      this.log("error", "an event listener threw", error),
    );
  }

  /** State changes, fatal errors and protocol errors. */
  get events(): Observable<RpcChannelEvents> {
    return this.emitter;
  }

  get state(): ConnectionState {
    return this.currentState;
  }

  /** The error that closed the channel, if one did. */
  get fatal(): RpcFatalError | null {
    return this.fatalError;
  }

  /** Starts connecting, and reconnecting whenever the connection drops. Idempotent. */
  connect(): void {
    if (this.currentState !== "idle") {
      return;
    }
    this.unsubscribeNetwork = this.network.subscribe((online) => this.onNetworkChange(online));
    this.startAttempt(false);
  }

  /**
   * Skips the wait before the next attempt, for a user action. Also tries
   * when the network is reported offline. Starts an idle channel; does
   * nothing while connecting or connected.
   */
  reconnectNow(): void {
    if (this.currentState === "idle") {
      this.connect();
    } else if (this.currentState === "waiting" || this.currentState === "offline") {
      this.clearRetryTimer();
      this.startAttempt(true);
    }
  }

  /**
   * Closes the channel for good. Pending calls fail with
   * {@link ClosedError}, open streams get `onDisconnected`; every timer and
   * the socket are released. Idempotent.
   */
  close(): void {
    if (this.currentState === "closed") {
      return;
    }
    this.shutDown(new ClosedError(), { code: 1000, reason: "closed by client", origin: "local" });
  }

  /** The same as {@link RpcChannel.close}. */
  dispose(): void {
    this.close();
  }

  /**
   * Drops the connection as if the network had failed; the channel then
   * reconnects with backoff. Does nothing unless a connection exists or is
   * being made.
   */
  dropConnection(): void {
    if (
      this.currentState === "connecting" ||
      this.currentState === "authenticating" ||
      this.currentState === "connected"
    ) {
      this.connectionLost(local("dropped by client"));
    }
  }

  /**
   * Resolves once the channel is `connected` (at once if it is). Rejects
   * with {@link RpcFatalError} or {@link ClosedError} if the channel closes
   * first. Does not start an idle channel.
   */
  whenConnected(): Promise<void> {
    if (this.currentState === "connected") {
      return Promise.resolve();
    }
    if (this.currentState === "closed") {
      return Promise.reject(this.fatalError ?? new ClosedError());
    }
    return new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject });
    });
  }

  /**
   * One request, one response, by callbacks that run synchronously inside
   * the frame handler, in arrival order with every other callback.
   *
   * Throws {@link DisconnectedError} if the channel is not connected,
   * {@link ClosedError} if it is closed, and the codec's `JsonError` if the
   * message cannot be encoded; no callback is called then. Otherwise
   * exactly one of the callbacks is called once: `onError` with
   * {@link RpcFailedError} (the server's `RpcFinished` text),
   * {@link DisconnectedError} (the connection dropped first),
   * {@link ClosedError} or {@link RpcProtocolError}.
   *
   * `interim`, if given, sees every response first: a response it returns
   * true for (and handles) does not end the call, which waits for the
   * next.
   */
  callWithCallback<Req, Res>(
    request: MessageCodec<Req>,
    message: Req,
    response: MessageCodec<Res>,
    onResponse: (message: Res) => void,
    onError: (error: Error) => void,
    interim?: (message: Res) => boolean,
  ): CallHandle {
    const entry: PendingCall = {
      kind: "call",
      responseName: response.name,
      decode: (body) => response.decode(body),
      onResponse: onResponse as (message: unknown) => void,
      onError,
      ...(interim !== undefined ? { interim: interim as (message: unknown) => boolean } : {}),
    };
    const sequenceNumber = this.send(request, message, entry);
    return { sequenceNumber };
  }

  /**
   * {@link RpcChannel.callWithCallback} as a promise, which rejects with
   * what the other throws or passes to `onError`. The promise settles in a
   * microtask after the frame handler: where the order relative to stream
   * messages matters, use the callback form.
   */
  call<Req, Res>(request: MessageCodec<Req>, message: Req, response: MessageCodec<Res>): Promise<Res> {
    return new Promise<Res>((resolve, reject) => {
      this.callWithCallback(request, message, response, resolve, reject);
    });
  }

  /**
   * Opens a server stream. Throws like {@link RpcChannel.callWithCallback}
   * if the request cannot be sent. The stream ends with exactly one of
   * `onFinished` and `onDisconnected`.
   */
  openStream<Req, Res>(
    request: MessageCodec<Req>,
    message: Req,
    response: MessageCodec<Res>,
    handlers: StreamHandlers<Res>,
  ): StreamHandle {
    const entry: PendingStream = {
      kind: "stream",
      responseName: response.name,
      decode: (body) => response.decode(body),
      handlers: handlers as StreamHandlers<unknown>,
      cancelled: false,
    };
    const sequenceNumber = this.send(request, message, entry);
    return {
      sequenceNumber,
      cancel: () => this.cancelStream(sequenceNumber, entry),
    };
  }

  // Sending.

  private send<Req>(request: MessageCodec<Req>, message: Req, entry: Pending): number {
    if (this.currentState === "closed") {
      throw new ClosedError(this.fatalError ?? undefined);
    }
    const socket = this.socket;
    if (this.currentState !== "connected" || socket === null) {
      throw new DisconnectedError(null);
    }
    const sequenceNumber = this.takeSequenceNumber();
    const text = encodeEnvelope(sequenceNumber, request, message);
    this.pending.set(sequenceNumber, entry);
    try {
      socket.send(text);
    } catch (error) {
      this.pending.delete(sequenceNumber);
      throw new DisconnectedError(local("send failed", error));
    }
    return sequenceNumber;
  }

  private takeSequenceNumber(): number {
    const sequenceNumber = this.nextSequenceNumber;
    // Numbers are per connection on the server; wrapping (never reached in
    // practice) must only avoid the numbers still in use.
    do {
      this.nextSequenceNumber =
        this.nextSequenceNumber >= 0x7fffffff ? 1 : this.nextSequenceNumber + 1;
    } while (this.pending.has(this.nextSequenceNumber));
    return sequenceNumber;
  }

  private cancelStream(sequenceNumber: number, entry: PendingStream): void {
    if (entry.cancelled || this.pending.get(sequenceNumber) !== entry) {
      return;
    }
    entry.cancelled = true;
    this.sendCancel(sequenceNumber);
  }

  private sendCancel(sequenceNumber: number): void {
    try {
      this.socket?.send(encodeEnvelope(sequenceNumber, CancelRpc, {}));
    } catch (error) {
      // The close that follows reports the stream as disconnected.
      this.log("debug", "could not send CancelRpc", error);
    }
  }

  // Connecting.

  private startAttempt(ignoreNetwork: boolean): void {
    if (!ignoreNetwork && !this.network.isOnline()) {
      this.setState("offline");
      return;
    }
    const generation = ++this.generation;
    this.setState("connecting");
    if (this.connectTimeoutMs > 0) {
      this.connectTimer = this.timers.setTimeout(() => {
        this.connectTimer = null;
        if (generation === this.generation) {
          this.connectionLost(local("connect timeout"));
        }
      }, this.connectTimeoutMs);
    }
    const url = this.url;
    const getAuthToken = this.getAuthToken;
    // Async wrappers, so that a synchronous throw is a rejection too.
    const resolveUrl = async (): Promise<string> => (typeof url === "function" ? url() : url);
    const resolveToken = async (): Promise<string | undefined> => getAuthToken?.();
    void Promise.all([resolveUrl(), resolveToken()]).then(
      ([resolvedUrl, token]) => {
        if (generation === this.generation) {
          this.openSocket(resolvedUrl, token);
        }
      },
      (error: unknown) => {
        if (generation !== this.generation) {
          return;
        }
        if (this.isAuthError(error)) {
          this.fail({ kind: "authTokenUnavailable", cause: error }, local("no credential", error));
        } else {
          this.log("warn", "could not prepare the connection", error);
          this.connectionLost(local("connect failed", error));
        }
      },
    );
  }

  private openSocket(url: string, token: string | undefined): void {
    let socket: SocketLike;
    try {
      socket = this.socketFactory(url);
    } catch (error) {
      this.log("warn", "could not open the socket", error);
      this.connectionLost(local("connect failed", error));
      return;
    }
    this.socket = socket;
    socket.onopen = () => {
      if (this.socket === socket) {
        this.onOpen(socket, token);
      }
    };
    socket.onmessage = (event) => {
      if (this.socket === socket) {
        this.onFrame(event.data);
      }
    };
    socket.onclose = (event) => {
      if (this.socket === socket) {
        this.connectionLost({
          code: typeof event?.code === "number" ? event.code : 1006,
          reason: typeof event?.reason === "string" ? event.reason : "",
          origin: "remote",
        });
      }
    };
    socket.onerror = (event) => {
      if (this.socket !== socket) {
        return;
      }
      this.log("debug", "socket error", event);
      // Before the socket opened an error is the failure of the attempt.
      // Later the close event follows and carries the code.
      if (this.currentState === "connecting") {
        this.connectionLost(local("connect failed", event));
      }
    };
  }

  private onOpen(socket: SocketLike, token: string | undefined): void {
    if (this.currentState !== "connecting") {
      return;
    }
    if (token === undefined) {
      // The server does not confirm cookie authentication: the connection
      // counts as working once a frame arrives.
      this.unproven = true;
      this.becomeConnected();
      return;
    }
    this.setState("authenticating");
    const sequenceNumber = this.takeSequenceNumber();
    this.pending.set(sequenceNumber, {
      kind: "call",
      responseName: ProtocolAuthenticationResult.name,
      decode: (body) => ProtocolAuthenticationResult.decode(body),
      onResponse: () => {
        this.failures = 0;
        this.becomeConnected();
      },
      onError: (error) => this.onAuthenticationError(error),
    });
    try {
      socket.send(encodeEnvelope(sequenceNumber, ProtocolAuthenticate, { token }));
    } catch (error) {
      this.pending.delete(sequenceNumber);
      this.connectionLost(local("send failed", error));
    }
  }

  private onAuthenticationError(error: Error): void {
    if (this.currentState !== "authenticating") {
      // The connection dropped; that is already being handled.
      return;
    }
    if (error instanceof RpcFailedError && error.errorText !== TRANSIENT_AUTH_FAILURE) {
      this.fail(
        { kind: "authenticationFailed", message: error.errorText },
        local("authentication failed"),
      );
    } else {
      this.connectionLost(local("authentication not completed", error));
    }
  }

  private becomeConnected(): void {
    this.clearConnectTimer();
    this.setState("connected");
    const waiters = this.waiters;
    this.waiters = [];
    for (const waiter of waiters) {
      waiter.resolve();
    }
  }

  // Receiving.

  private onFrame(data: unknown): void {
    let envelope: Envelope;
    try {
      if (typeof data !== "string") {
        throw new TypeError("not a text frame");
      }
      envelope = parseEnvelope(data);
    } catch (error) {
      this.protocolError(
        new RpcProtocolError({ kind: "undecodableFrame", sequenceNumber: null, cause: error }),
      );
      return;
    }
    if (this.unproven) {
      this.unproven = false;
      this.failures = 0;
    }
    const sequenceNumber = envelope.sequenceNumber;
    const entry = this.pending.get(sequenceNumber);
    if (entry === undefined) {
      // A late message of a request that is over.
      this.log("debug", `frame for unknown request ${sequenceNumber}`, envelope.messageType);
      return;
    }
    if (envelope.messageType === RpcFinished.name) {
      let finished: RpcFinished;
      try {
        finished = RpcFinished.decode(envelope.message);
      } catch (error) {
        this.undecodable(sequenceNumber, entry, error);
        return;
      }
      this.pending.delete(sequenceNumber);
      const errorText = finished.failed ? (finished.errorText ?? "") : undefined;
      if (entry.kind === "stream") {
        this.guard(() => entry.handlers.onFinished(errorText));
      } else {
        this.guard(() =>
          entry.onError(new RpcFailedError(errorText ?? "The RPC finished without a response")),
        );
      }
      return;
    }
    if (envelope.messageType !== entry.responseName) {
      const error = new RpcProtocolError({
        kind: "unexpectedMessage",
        sequenceNumber,
        messageType: envelope.messageType,
        expected: entry.responseName,
      });
      this.giveUp(sequenceNumber, entry, error);
      return;
    }
    let message: unknown;
    try {
      message = entry.decode(envelope.message);
    } catch (error) {
      this.undecodable(sequenceNumber, entry, error);
      return;
    }
    if (entry.kind === "call") {
      const interim = entry.interim;
      let more = false;
      if (interim !== undefined) {
        this.guard(() => {
          more = interim(message);
        });
      }
      if (more) {
        return;
      }
      this.pending.delete(sequenceNumber);
      this.guard(() => entry.onResponse(message));
    } else if (!entry.cancelled) {
      this.guard(() => entry.handlers.onMessage(message));
    }
  }

  private undecodable(sequenceNumber: number, entry: Pending, cause: unknown): void {
    this.giveUp(
      sequenceNumber,
      entry,
      new RpcProtocolError({ kind: "undecodableFrame", sequenceNumber, cause }),
    );
  }

  /**
   * Ends one request over a frame that makes no sense for it. The other
   * requests and the connection are left alone.
   */
  private giveUp(sequenceNumber: number, entry: Pending, error: RpcProtocolError): void {
    this.pending.delete(sequenceNumber);
    if (entry.kind === "stream") {
      // The stream has lost a message, so it is of no use any more.
      if (!entry.cancelled) {
        this.sendCancel(sequenceNumber);
      }
      this.guard(() => entry.handlers.onFinished(error.message));
    } else {
      this.guard(() => entry.onError(error));
    }
    this.protocolError(error);
  }

  private protocolError(error: RpcProtocolError): void {
    this.log("warn", error.message, error);
    this.emitter.emit("protocolError", error);
  }

  // Disconnecting.

  /** The connection, or the attempt to make one, ended. */
  private connectionLost(info: CloseInfo): void {
    if (this.currentState === "closed") {
      return;
    }
    if (info.origin === "remote" && info.code === 1008) {
      if (info.reason === "signed out") {
        this.fail({ kind: "signedOut", close: info }, info);
        return;
      }
      if (info.reason === "authentication timeout") {
        this.fail({ kind: "authenticationTimeout", close: info }, info);
        return;
      }
    }
    const entries = this.releaseConnection();
    const online = this.network.isOnline();
    if (online) {
      const delay = this.nextDelay();
      this.retryTimer = this.timers.setTimeout(() => {
        this.retryTimer = null;
        this.startAttempt(false);
      }, delay);
      this.setState("waiting", info, delay);
    } else {
      this.setState("offline", info);
    }
    this.failPending(entries, new DisconnectedError(info), info);
    if (
      info.origin === "remote" &&
      (info.code === 1002 || info.code === 1003 || info.code === 1007 || info.code === 1009)
    ) {
      this.protocolError(new RpcProtocolError({ kind: "protocolClose", close: info }));
    }
  }

  private fail(detail: RpcFatalDetail, info: CloseInfo): void {
    const error = new RpcFatalError(detail);
    this.fatalError = error;
    this.log("warn", error.message, error);
    this.shutDown(new DisconnectedError(info), info);
    this.emitter.emit("fatal", error);
  }

  /** Ends the channel: `callError` is what pending calls fail with. */
  private shutDown(callError: Error, info: CloseInfo): void {
    const entries = this.releaseConnection();
    this.unsubscribeNetwork?.();
    this.unsubscribeNetwork = null;
    this.setState("closed", info);
    this.failPending(entries, callError, info);
    const waiters = this.waiters;
    this.waiters = [];
    for (const waiter of waiters) {
      waiter.reject(this.fatalError ?? new ClosedError());
    }
  }

  /**
   * Detaches and closes the socket, stops the timers and takes the pending
   * requests, which are over.
   */
  private releaseConnection(): Pending[] {
    this.generation += 1;
    this.clearConnectTimer();
    this.clearRetryTimer();
    this.unproven = false;
    const socket = this.socket;
    this.socket = null;
    if (socket !== null) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      // Errors of a socket that is being closed are of no interest, but
      // must not go unhandled where that matters.
      socket.onerror = () => {};
      try {
        socket.close();
      } catch (error) {
        this.log("debug", "closing the socket failed", error);
      }
    }
    const entries = Array.from(this.pending.values());
    this.pending.clear();
    return entries;
  }

  private failPending(entries: readonly Pending[], callError: Error, info: CloseInfo): void {
    for (const entry of entries) {
      if (entry.kind === "call") {
        this.guard(() => entry.onError(callError));
      } else {
        this.guard(() => entry.handlers.onDisconnected(info));
      }
    }
  }

  private nextDelay(): number {
    const base = Math.min(this.maxMs, this.initialMs * this.factor ** this.failures);
    this.failures += 1;
    const spread = 1 + this.jitter * (2 * this.random() - 1);
    return Math.max(0, Math.min(this.maxMs, Math.round(base * spread)));
  }

  private onNetworkChange(online: boolean): void {
    if (online) {
      if (this.currentState === "offline" || this.currentState === "waiting") {
        this.clearRetryTimer();
        this.startAttempt(false);
      }
    } else if (this.currentState === "waiting") {
      this.clearRetryTimer();
      this.setState("offline");
    } else if (
      this.currentState === "connecting" ||
      this.currentState === "authenticating" ||
      this.currentState === "connected"
    ) {
      // Without a network the socket is dead, though it may take the
      // platform minutes to notice.
      this.connectionLost(local("offline"));
    }
  }

  // Helpers.

  private setState(state: ConnectionState, detail?: CloseInfo, retryInMs?: number): void {
    const previous = this.currentState;
    if (previous === state) {
      return;
    }
    this.currentState = state;
    this.log("debug", `${previous} -> ${state}`, detail);
    this.emitter.emit("state", {
      state,
      previous,
      ...(detail !== undefined ? { detail } : {}),
      ...(retryInMs !== undefined ? { retryInMs } : {}),
    });
  }

  private clearRetryTimer(): void {
    if (this.retryTimer !== null) {
      this.timers.clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
  }

  private clearConnectTimer(): void {
    if (this.connectTimer !== null) {
      this.timers.clearTimeout(this.connectTimer);
      this.connectTimer = null;
    }
  }

  /** Runs a callback of the layer above; its errors stay there. */
  private guard(callback: () => void): void {
    try {
      callback();
    } catch (error) {
      this.log("error", "a channel callback threw", error);
    }
  }

  private log(level: LogLevel, message: string, detail?: unknown): void {
    if (this.logger === undefined) {
      return;
    }
    try {
      this.logger.log(level, message, detail);
    } catch {
      // A broken logger must not disturb the channel.
    }
  }
}
