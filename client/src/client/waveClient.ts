// The high-level client: session, sign-in, the server connection and the
// waves opened over it. For a user interface and for headless use (Node,
// agents) alike: `fetch`, the WebSocket and timers can be injected.
//
// Covers what `WebClient`, `Session`, `ClientIdGenerator`,
// `WaveWebSocketClient` and `StagesProvider` of
// `org.waveprotocol.box.webclient.client` do in Apache Wave (Apache License
// 2.0) below the user interface.

import { type Logger as ChannelLogger } from "../concurrencycontrol/common";
import { IdGeneratorImpl, ParticipantId, WaveId } from "../model/id";
import {
  AuthError,
  type BackoffOptions,
  type ConnectionState,
  type NetworkStatus,
  RpcChannel,
  type RpcChannelOptions,
  type RpcFatalError,
  type SearchService,
  type SessionInfo,
  type SocketFactory,
  type StateChange,
  type StreamHandle,
  type StreamHandlers,
  WaveHttpApi,
  sessionAuthToken,
  socketUrlFromBaseUrl,
} from "../net";
import { Emitter, type Observable } from "../util/events";
import {
  AppEventRequest,
  AppEventResponse,
  AppMediaRequest,
  AppOpenRequest,
  AppRtcRequest,
  AppRtcResponse,
  AppUpdate,
  type ProfileResponse,
  type SearchResponse,
} from "../wire";
import { BackoffScheduler, DEFAULT_TIMERS, type Timers } from "./backoffScheduler";
import { LiveWaveView, type UnsavedState } from "./liveWaveView";
import { OpenWave } from "./openWave";
import { RemoteWaveViewService } from "./remoteWaveViewService";
import { RpcWaveTransport } from "./rpcWaveTransport";

export interface WaveClientOptions {
  /** The server, e.g. `https://wave.example.com`; `""` for the page's own origin. */
  baseUrl: string;
  /**
   * Default: the global `fetch`. In Node wrap it with `withCookieJar`, or
   * the session cookie is lost.
   */
  fetch?: typeof fetch;
  /** Default: the global `WebSocket`. */
  socketFactory?: SocketFactory;
  /** Default: derived from `baseUrl` (`ws(s)://host/socket`). */
  socketUrl?: string;
  /**
   * How the socket is authenticated. `"token"` (default) sends the session's
   * socket token as first frame, which works everywhere; `"cookie"` relies
   * on the session cookie of a same-origin page.
   */
  socketAuth?: "token" | "cookie";
  backoff?: BackoffOptions;
  timers?: Timers;
  random?: () => number;
  network?: NetworkStatus;
  clock?: () => number;
  /** Trace output of the channel stack. */
  channelLogger?: ChannelLogger;
  /** Receives what event listeners throw. Default: rethrown from a microtask. */
  onListenerError?: (error: unknown) => void;
}

export type WaveClientEvents = {
  /** The connection state changed. */
  connection: StateChange;
  signedIn: { address: string };
  /**
   * The session ended: by `signOut()` (`error` null), or because the server
   * ended it (signed out elsewhere, expired). Open waves are closed.
   */
  signedOut: { error: RpcFatalError | null };
  /** The combined unsaved state of all open waves changed. */
  unsaved: UnsavedState;
  /**
   * How many submitted deltas wait for their wavelet's host (another
   * server or device the server cannot reach now) changed.
   */
  awaitingHost: number;
};

/** The client is not signed in. */
export class NotSignedInError extends Error {
  constructor() {
    super("not signed in");
    this.name = "NotSignedInError";
  }
}

interface Connection {
  readonly address: ParticipantId;
  readonly channel: RpcChannel;
  readonly transport: RpcWaveTransport;
  readonly idGenerator: IdGeneratorImpl;
  readonly unsubscribe: (() => void)[];
}

export class WaveClient {
  readonly api: WaveHttpApi;
  readonly events: Observable<WaveClientEvents>;

  private readonly emitter: Emitter<WaveClientEvents>;
  private readonly timers: Timers;
  private sessionInfo: SessionInfo | null = null;
  private connection: Connection | null = null;
  private readonly waves = new Map<string, { wave: OpenWave; cleanup: () => void }>();
  private lastUnsaved: UnsavedState = { inFlight: 0, unacknowledged: 0, uncommitted: 0, saved: true };
  private awaitingHostCount = 0;

  /** How many submitted deltas wait for their wavelet's host. */
  get awaitingHost(): number {
    return this.awaitingHostCount;
  }
  private disposed = false;

  constructor(private readonly options: WaveClientOptions) {
    this.api = new WaveHttpApi({
      baseUrl: options.baseUrl,
      ...(options.fetch !== undefined ? { fetch: options.fetch } : {}),
    });
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.emitter = new Emitter<WaveClientEvents>(
      options.onListenerError ??
        ((error) =>
          queueMicrotask(() => {
            throw error;
          })),
    );
    this.events = this.emitter;
  }

  // ---- session ----

  /** What the server said about the session at the last `start`/`signIn`. */
  get session(): SessionInfo | null {
    return this.sessionInfo;
  }

  /** The signed-in user, or null. */
  get address(): ParticipantId | null {
    return this.connection?.address ?? null;
  }

  get signedIn(): boolean {
    return this.connection !== null;
  }

  /** The state of the server connection; `"idle"` while signed out. */
  get connectionState(): ConnectionState {
    return this.connection?.channel.state ?? "idle";
  }

  /** The combined unsaved state of all open waves. */
  get unsavedState(): UnsavedState {
    return this.lastUnsaved;
  }

  /**
   * Reads the session. If it belongs to a signed-in user (a cookie from an
   * earlier sign-in), the connection is started. Resolves with the session.
   */
  async start(): Promise<SessionInfo> {
    this.checkLive();
    const session = await this.api.getSession();
    this.sessionInfo = session;
    if (session.address !== null && this.connection === null) {
      this.connect(session, session.address);
    }
    return session;
  }

  /** Signs in and starts the connection. Throws `AuthError`. */
  async signIn(address: string, password: string): Promise<SessionInfo> {
    this.checkLive();
    if (this.connection !== null) {
      await this.signOut();
    }
    await this.api.signIn(address, password);
    const session = await this.start();
    if (session.address === null) {
      // The cookie did not stick (in Node: `fetch` without a cookie jar).
      throw new AuthError({
        kind: "notSignedIn",
        message: "Signed in, but the session cookie was not kept",
      });
    }
    return session;
  }

  /** Creates an account. Does not sign in. Throws `AuthError`. */
  async register(address: string, password: string): Promise<void> {
    this.checkLive();
    await this.api.register(address, password);
  }

  /** Closes the open waves and the connection, and ends the session. */
  async signOut(): Promise<void> {
    const wasSignedIn = this.connection !== null;
    this.disconnect();
    try {
      await this.api.signOut();
    } finally {
      this.sessionInfo = null;
      if (wasSignedIn) {
        this.emitter.emit("signedOut", { error: null });
      }
    }
  }

  /** Resolves when the connection is up; rejects if the session ends first. */
  whenConnected(): Promise<void> {
    if (this.connection === null) {
      return Promise.reject(new NotSignedInError());
    }
    return this.connection.channel.whenConnected();
  }

  /** Retries the connection now instead of waiting out the backoff. */
  reconnectNow(): void {
    this.connection?.channel.reconnectNow();
  }

  /** Drops the socket as if the network had failed; it reconnects. For tests and diagnostics. */
  dropConnection(): void {
    this.connection?.channel.dropConnection();
  }

  /** Closes everything. The client cannot be used afterwards. */
  dispose(): void {
    this.disposed = true;
    this.disconnect();
    this.emitter.clear();
  }

  private checkLive(): void {
    if (this.disposed) {
      throw new Error("the wave client was disposed");
    }
  }

  private connect(session: SessionInfo, address: string): void {
    const participant = ParticipantId.of(address);
    const options = this.options;
    const channelOptions: { -readonly [K in keyof RpcChannelOptions]: RpcChannelOptions[K] } = {
      url: options.socketUrl ?? socketUrlFromBaseUrl(options.baseUrl),
      timers: this.timers,
    };
    if (options.socketFactory !== undefined) {
      channelOptions.socketFactory = options.socketFactory;
    }
    if ((options.socketAuth ?? "token") === "token") {
      channelOptions.getAuthToken = sessionAuthToken(this.api);
    }
    if (options.backoff !== undefined) {
      channelOptions.backoff = options.backoff;
    }
    if (options.random !== undefined) {
      channelOptions.random = options.random;
    }
    if (options.network !== undefined) {
      channelOptions.network = options.network;
    }
    const channel = new RpcChannel(channelOptions);
    const connection: Connection = {
      address: participant,
      channel,
      transport: new RpcWaveTransport(channel),
      // Ids are unique by the session's seed (`ClientIdGenerator`).
      idGenerator: new IdGeneratorImpl(participant.domain(), () => session.idSeed),
      unsubscribe: [],
    };
    connection.unsubscribe.push(
      channel.events.on("state", (change) => this.emitter.emit("connection", change)),
      channel.events.on("fatal", (error) => this.onFatal(connection, error)),
    );
    this.connection = connection;
    this.emitter.emit("signedIn", { address: participant.address });
    channel.connect();
  }

  private onFatal(connection: Connection, error: RpcFatalError): void {
    if (this.connection !== connection) {
      return;
    }
    this.disconnect();
    this.sessionInfo = null;
    this.emitter.emit("signedOut", { error });
  }

  private disconnect(): void {
    for (const { wave } of [...this.waves.values()]) {
      wave.close();
    }
    const connection = this.connection;
    this.connection = null;
    if (connection !== null) {
      for (const unsubscribe of connection.unsubscribe) {
        unsubscribe();
      }
      connection.channel.dispose();
    }
  }

  private connected(): Connection {
    this.checkLive();
    if (this.connection === null) {
      throw new NotSignedInError();
    }
    return this.connection;
  }

  // ---- waves ----

  /** The open wave with an id, if any. */
  wave(waveId: WaveId | string): OpenWave | null {
    return this.waves.get(toWaveId(waveId).serialise())?.wave ?? null;
  }

  openWaves(): OpenWave[] {
    return [...this.waves.values()].map((entry) => entry.wave);
  }

  /**
   * Starts opening a wave and returns it at once; its wavelets arrive
   * later (`whenOpened()`, or the `opened` event), also if the client is
   * offline now. A wave that is already open is returned as it is.
   */
  attachWave(waveId: WaveId | string): OpenWave {
    const connection = this.connected();
    const id = toWaveId(waveId);
    const key = id.serialise();
    const existing = this.waves.get(key);
    if (existing !== undefined) {
      return existing.wave;
    }
    const options = this.options;
    const schedulerOptions: ConstructorParameters<typeof BackoffScheduler>[0] = {
      timers: this.timers,
    };
    if (options.random !== undefined) {
      schedulerOptions.random = options.random;
    }
    const scheduler = new BackoffScheduler(schedulerOptions);
    const serviceOptions: ConstructorParameters<typeof RemoteWaveViewService>[2] = {
      onMarker: () => scheduler.reset(),
      onAwaitingHost: (change) => {
        this.awaitingHostCount = Math.max(0, this.awaitingHostCount + change);
        this.emitter.emit("awaitingHost", this.awaitingHostCount);
      },
    };
    if (options.clock !== undefined) {
      serviceOptions.clock = options.clock;
    }
    const service = new RemoteWaveViewService(id, connection.transport, serviceOptions);
    const liveOptions: ConstructorParameters<typeof LiveWaveView>[0] = {
      waveId: id,
      viewer: connection.address,
      service,
      scheduler,
    };
    if (options.clock !== undefined) {
      liveOptions.clock = options.clock;
    }
    if (options.channelLogger !== undefined) {
      liveOptions.logger = options.channelLogger;
    }
    if (options.onListenerError !== undefined) {
      liveOptions.onListenerError = options.onListenerError;
    }
    const live = new LiveWaveView(liveOptions);
    const wave = new OpenWave(
      id,
      live,
      connection.idGenerator,
      () => {
        const entry = this.waves.get(key);
        if (entry?.wave === wave) {
          this.waves.delete(key);
          entry.cleanup();
          this.unsavedChanged();
        }
      },
      options.onListenerError,
    );
    const offUnsaved = live.events.on("unsaved", () => this.unsavedChanged());
    this.waves.set(key, {
      wave,
      cleanup: () => {
        offUnsaved();
        scheduler.cancel();
        service.dispose();
      },
    });
    live.open();
    return wave;
  }

  /**
   * Opens a wave and resolves when its wavelets have arrived. Rejects with
   * `WaveFailedError` (and closes the wave) if that fails. A wave the user
   * cannot see, or that does not exist, opens with no wavelets.
   */
  async openWave(waveId: WaveId | string): Promise<OpenWave> {
    const wave = this.attachWave(waveId);
    try {
      await wave.whenOpened();
    } catch (error) {
      wave.close();
      throw error;
    }
    return wave;
  }

  /**
   * Creates a wave: a new wave id from the session's id generator, the
   * root conversation with a first blip, and the given participants and
   * title. Resolves when the (empty) view is open and the wave has been
   * created locally; `wave.whenSaved()` tells when the server has it.
   */
  async createWave(
    options: { participants?: readonly (ParticipantId | string)[]; title?: string; text?: string } = {},
  ): Promise<{ wave: OpenWave; rootBlipId: string }> {
    const connection = this.connected();
    const wave = await this.openWave(connection.idGenerator.newWaveId());
    const rootBlipId = wave.createRootConversation();
    for (const participant of options.participants ?? []) {
      wave.addParticipant(participant);
    }
    if (options.text !== undefined && options.text !== "") {
      wave.appendText(rootBlipId, options.text);
    }
    if (options.title !== undefined) {
      wave.setTitle(options.title);
    }
    return { wave, rootBlipId };
  }

  private unsavedChanged(): void {
    let inFlight = 0;
    let unacknowledged = 0;
    let uncommitted = 0;
    for (const { wave } of this.waves.values()) {
      const state = wave.unsavedState;
      inFlight += state.inFlight;
      unacknowledged += state.unacknowledged;
      uncommitted += state.uncommitted;
    }
    const last = this.lastUnsaved;
    if (
      last.inFlight === inFlight &&
      last.unacknowledged === unacknowledged &&
      last.uncommitted === uncommitted
    ) {
      return;
    }
    this.lastUnsaved = {
      inFlight,
      unacknowledged,
      uncommitted,
      saved: unacknowledged === 0 && uncommitted === 0,
    };
    this.emitter.emit("unsaved", this.lastUnsaved);
  }

  // ---- other services ----

  fetchProfiles(addresses: readonly (ParticipantId | string)[]): Promise<ProfileResponse> {
    return this.api.fetchProfiles(addresses.map(String));
  }

  /** The URL of a participant's generated avatar image. */
  avatarUrl(address: ParticipantId | string, width: number, height: number): string {
    return this.api.avatarUrl(String(address), width, height);
  }

  /** Search over the user's waves. The server side is not available yet. */
  get searchService(): SearchService {
    return this.api.search;
  }

  search(query: string, index = 0, numResults = 20): Promise<SearchResponse> {
    return this.api.search.search(query, index, numResults);
  }

  /**
   * Opens the view of a gadget's app: `onUpdate` gets its status and, while
   * it runs, every new render. Throws if not connected.
   */
  openApp(request: AppOpenRequest, handlers: StreamHandlers<AppUpdate>): StreamHandle {
    return this.connected().channel.openStream(AppOpenRequest, request, AppUpdate, handlers);
  }

  /** Hands a UI event to an app this client has open. */
  appEvent(request: AppEventRequest): Promise<AppEventResponse> {
    return this.connected().channel.call(AppEventRequest, request, AppEventResponse);
  }

  /** Hands an open app a frame of the microphone. */
  appMedia(request: AppMediaRequest): Promise<AppEventResponse> {
    return this.connected().channel.call(AppMediaRequest, request, AppEventResponse);
  }

  /** Offers an open app a WebRTC connection for its media. */
  appRtc(request: AppRtcRequest): Promise<AppRtcResponse> {
    return this.connected().channel.call(AppRtcRequest, request, AppRtcResponse);
  }
}

function toWaveId(waveId: WaveId | string): WaveId {
  return typeof waveId === "string" ? WaveId.deserialise(waveId) : waveId;
}
