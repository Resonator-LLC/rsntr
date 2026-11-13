// The HTTP endpoints of the wave server: session, sign-in, registration,
// snapshots, profiles, locale, and (not yet served) search and attachments.
//
// Follows the client side of `WaveClientServlet` (session), `SnapshotFetcher`,
// `RemoteLocaleService`, `FetchProfilesServiceImpl`, `JsoSearchBuilderImpl`
// and the attachment upload form of Apache Wave, licensed under the Apache
// License, Version 2.0, against the endpoints of the resonator server
// (`crates/resonator-server/src/http`).

import { type WaveId, WaveRef, type WaveletId } from "../model/id";
import {
  AttachmentsResponse,
  DocumentSnapshot,
  type MessageCodec,
  ProfileResponse,
  SearchResponse,
  WaveViewSnapshot,
  WaveletSnapshot,
  parseJson,
} from "../wire";
import { AuthError, HttpError, InvalidResponseError, NetworkError } from "./errors";
import { PersonApi } from "./person";

/** The start of a link that adds a contact (`resonator:contact/<address>`). */
export const CONTACT_LINK_PREFIX = "resonator:contact/";

/** The `device` part of a `/session` body, if it is a device node's. */
function deviceInfo(body: Record<string, unknown>): { device?: DeviceInfo } {
  const device = body["device"];
  if (body["mode"] !== "device" || !isObject(device) || typeof device["account"] !== "string") {
    return {};
  }
  return {
    device: {
      account: device["account"],
      name: typeof device["name"] === "string" ? device["name"] : "",
      key: typeof device["key"] === "string" ? device["key"] : null,
      allowRemote: device["allow_remote"] === true,
    },
  };
}

/** What `GET /session` tells the client. */
export interface SessionInfo {
  /** The wave domain of the server. */
  readonly domain: string;
  /** The signed-in user's address; null if nobody is signed in. */
  readonly address: string | null;
  /** A seed for the client's id generator (`id` on the wire). */
  readonly idSeed: string;
  /**
   * The `host:port` the server names for the WebSocket. It is the server's
   * public address if configured, otherwise its first listen address, which
   * may be a wildcard: prefer {@link socketUrlFromBaseUrl} unless the
   * deployment sets a public address.
   */
  readonly websocketAddress: string;
  readonly registrationEnabled: boolean;
  /**
   * Whether the server federates with other wave servers: participants of
   * other domains can be added, and attachment ids name this domain.
   */
  readonly federation?: boolean;
  /**
   * The token for the `ProtocolAuthenticate` frame; null unless the request
   * carried a session cookie.
   */
  readonly socketToken: string | null;
  /**
   * Set when the server is a device node (`mode = "device"`): one account,
   * signed in by the app wrapping it; no registration, no sign-in screen,
   * contacts instead of the directory.
   */
  readonly device?: DeviceInfo;
}

/** What `/session` tells about a device node. */
export interface DeviceInfo {
  /** The device's one account. */
  readonly account: string;
  /** This device's name among its person's devices ("" if unnamed). */
  readonly name: string;
  /** This device's key (52 characters), if it federates. */
  readonly key: string | null;
  /** Whether other machines may sign in (with a password). */
  readonly allowRemote: boolean;
}

/** The result of a sign-in. */
export interface SignInResult {
  readonly address: string;
  readonly domain: string;
}

/** The result of a registration. */
export interface RegisterResult {
  readonly address: string;
  /** The server's confirmation text. */
  readonly message: string;
}

/** What `GET /fetch/<waveref>` returns, by the kind of reference. */
export type FetchResult =
  /** A wave reference: a view holding the root conversation wavelet. */
  | { readonly kind: "wave"; readonly snapshot: WaveViewSnapshot }
  | { readonly kind: "wavelet"; readonly snapshot: WaveletSnapshot }
  | { readonly kind: "document"; readonly snapshot: DocumentSnapshot };

export interface WaveHttpApiOptions {
  /**
   * The server's origin, such as `https://wave.example.com`, without a
   * trailing slash; empty for the page's own origin.
   */
  readonly baseUrl: string;
  /** Defaults to the global `fetch`. */
  readonly fetch?: typeof fetch;
  /**
   * A personal access token, sent as `Authorization: Bearer`. Without one
   * the session cookie authenticates (`credentials: "include"`).
   */
  readonly bearerToken?: string;
}

export interface RequestOptions {
  readonly method?: "GET" | "POST" | "HEAD" | "DELETE";
  readonly json?: unknown;
  readonly form?: FormData;
}

interface RawResponse {
  readonly status: number;
  readonly headers: Headers;
  readonly text: string;
  readonly url: string;
}

type FailureContext = "signin" | "register" | "data";

const CROSS_SITE_MESSAGE = "Cross-site request rejected";
const REGISTRATION_DISABLED_MESSAGE = "Registration disabled by administrator";
const ACCOUNT_EXISTS_MESSAGE = "Account already exists";

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function tryParse(text: string): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return undefined;
  }
}

/** The `message` of a `{"status":"FAILED","message":...}` body, else "". */
function failureMessage(text: string): string {
  const body = tryParse(text);
  return isObject(body) && typeof body["message"] === "string" ? body["message"] : "";
}

function retryAfterSecs(headers: Headers): number | null {
  const value = headers.get("retry-after");
  return value !== null && /^[0-9]+$/.test(value.trim()) ? Number(value.trim()) : null;
}

/** The error for a response that is not the success the request expects. */
function failure(response: RawResponse, context: FailureContext): Error {
  const message = failureMessage(response.text);
  switch (response.status) {
    case 429:
      return new AuthError({
        kind: "rateLimited",
        retryAfterSecs: retryAfterSecs(response.headers),
        message,
      });
    case 401:
      return new AuthError({ kind: "unauthorized", message });
    case 400:
      if (context !== "data") {
        return new AuthError({ kind: "badRequest", message });
      }
      break;
    case 403:
      if (message === CROSS_SITE_MESSAGE) {
        return new AuthError({ kind: "crossSite", message });
      }
      if (context === "signin") {
        return new AuthError({ kind: "invalidCredentials", message });
      }
      if (context === "register") {
        if (message === REGISTRATION_DISABLED_MESSAGE) {
          return new AuthError({ kind: "registrationDisabled", message });
        }
        if (message === ACCOUNT_EXISTS_MESSAGE) {
          return new AuthError({ kind: "accountExists", message });
        }
        return new AuthError({ kind: "failed", message });
      }
      return new AuthError({ kind: "forbidden", message });
    default:
      break;
  }
  return new HttpError(response.status, response.text, response.url);
}

/**
 * Requests against one server: adds the base URL, credentials and the
 * bearer token, and turns transport failures into {@link NetworkError}.
 */
export class HttpTransport {
  readonly baseUrl: string;
  private readonly fetchFunction: typeof fetch;
  private readonly bearerToken: string | undefined;

  constructor(options: WaveHttpApiOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    const given = options.fetch;
    if (given !== undefined) {
      this.fetchFunction = given;
    } else if (typeof globalThis.fetch === "function") {
      // The browser's fetch must be called on the global object.
      this.fetchFunction = (input, init) => globalThis.fetch(input, init);
    } else {
      throw new TypeError("no fetch implementation: pass one in the options");
    }
    this.bearerToken = options.bearerToken;
  }

  /** The absolute URL of a server path that starts with `/`. */
  url(path: string): string {
    return `${this.baseUrl}${path}`;
  }

  async request(path: string, options: RequestOptions = {}): Promise<RawResponse> {
    const url = this.url(path);
    const headers: Record<string, string> = { Accept: "application/json" };
    let body: string | FormData | undefined;
    if (options.json !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(options.json);
    } else if (options.form !== undefined) {
      // The multipart content type, with its boundary, is set by fetch.
      body = options.form;
    }
    if (this.bearerToken !== undefined) {
      headers["Authorization"] = `Bearer ${this.bearerToken}`;
    }
    try {
      const response = await this.fetchFunction(url, {
        method: options.method ?? "GET",
        credentials: "include",
        headers,
        ...(body !== undefined ? { body } : {}),
      });
      const text = await response.text();
      return { status: response.status, headers: response.headers, text, url };
    } catch (error) {
      throw new NetworkError(
        `request to ${url} failed${error instanceof Error ? `: ${error.message}` : ""}`,
        error,
      );
    }
  }

  /** A request that must be answered with 200 (or `created`, 201). */
  async expectOk(
    path: string,
    options: RequestOptions = {},
    context: FailureContext = "data",
  ): Promise<RawResponse> {
    const response = await this.request(path, options);
    if (response.status !== 200 && response.status !== 201) {
      throw failure(response, context);
    }
    return response;
  }

  /** A GET answered with a protocol message in its JSON form. */
  async getMessage<T>(path: string, codec: MessageCodec<T>): Promise<T> {
    const response = await this.expectOk(path);
    try {
      return codec.decode(parseJson(response.text));
    } catch (error) {
      throw new InvalidResponseError(response.url, response.text, error);
    }
  }
}

/** A JSON object body, or {@link InvalidResponseError}. */
/** An account listed by the server's directory. */
/** What `GET /federation/probe` found out about a remote address. */
export interface ProbeResult {
  /** Its server answered. */
  readonly reachable: boolean;
  /** This server and that one may federate. */
  readonly allowed: boolean;
  /** Whether the address exists there; null if it did not say. */
  readonly exists: boolean | null;
  readonly message: string | null;
}

export interface DirectoryEntry {
  readonly address: string;
  readonly name: string;
  /** `agent`: a built-in AI agent; `robot`: a registered robot. */
  readonly kind: "human" | "agent" | "robot";
}

function jsonObject(response: RawResponse): Record<string, unknown> {
  const body = tryParse(response.text);
  if (!isObject(body)) {
    throw new InvalidResponseError(response.url, response.text);
  }
  return body;
}

function stringField(
  body: Record<string, unknown>,
  name: string,
  response: RawResponse,
): string {
  const value = body[name];
  if (typeof value !== "string") {
    throw new InvalidResponseError(
      response.url,
      response.text,
      new TypeError(`missing string field "${name}"`),
    );
  }
  return value;
}

/** Searches the signed-in user's waves. */
export interface SearchService {
  /**
   * The digests `index .. index + numResults` of the waves matching
   * `query` (such as `in:inbox`).
   */
  search(query: string, index: number, numResults: number): Promise<SearchResponse>;
}

/**
 * {@link SearchService} over `GET /search/?query=&index=&numResults=`, the
 * endpoint of the original `SearchServlet`.
 *
 * UNTESTED AGAINST A SERVER: resonator does not serve search yet.
 */
export class HttpSearchService implements SearchService {
  constructor(private readonly transport: HttpTransport) {}

  /** The path and query of a search request. */
  static path(query: string, index: number, numResults: number): string {
    return `/search/?query=${encodeURIComponent(query)}&index=${index}&numResults=${numResults}`;
  }

  search(query: string, index: number, numResults: number): Promise<SearchResponse> {
    return this.transport.getMessage(
      HttpSearchService.path(query, index, numResults),
      SearchResponse,
    );
  }
}

/** Attachment upload, download addresses and metadata. */
export interface AttachmentService {
  /**
   * The address the attachment's content is served at. `waveRef` names the
   * wave it is on (the reference in its URI path form); the server needs it
   * to fetch an attachment of another server.
   */
  attachmentUrl(attachmentId: string, waveRef?: string): string;
  /** The address the attachment's thumbnail is served at. */
  thumbnailUrl(attachmentId: string, waveRef?: string): string;
  /** The metadata of the attachments the user may see. */
  getAttachmentsInfo(attachmentIds: readonly string[], waveRef?: string): Promise<AttachmentsResponse>;
  /**
   * The HTTP status of the attachment's thumbnail: 503 while the server
   * the attachment was uploaded to cannot be reached.
   */
  status(attachmentId: string, waveRef?: string): Promise<number>;
  /**
   * Uploads the content of a new attachment of the wavelet `waveRef`
   * refers to (the reference in its URI path form).
   */
  upload(attachmentId: string, waveRef: string, file: Blob, fileName: string): Promise<void>;
}

function waveRefQuery(separator: "?" | "&", waveRef: string | undefined): string {
  return waveRef === undefined ? "" : `${separator}waveRef=${encodeURIComponent(waveRef)}`;
}

/**
 * {@link AttachmentService} over the endpoints of the original
 * `AttachmentServlet` and `AttachmentInfoServlet`: `GET /attachment/<id>`,
 * `GET /thumbnail/<id>`, `GET /attachmentsInfo?attachmentIds=a,b` and a
 * multipart `POST /attachment/<id>` with the form fields `attachmentId` and
 * `waveRef` and the file.
 */
export class HttpAttachmentService implements AttachmentService {
  constructor(private readonly transport: HttpTransport) {}

  attachmentUrl(attachmentId: string, waveRef?: string): string {
    return this.transport.url(`/attachment/${encodeURIComponent(attachmentId)}${waveRefQuery("?", waveRef)}`);
  }

  thumbnailUrl(attachmentId: string, waveRef?: string): string {
    return this.transport.url(`/thumbnail/${encodeURIComponent(attachmentId)}${waveRefQuery("?", waveRef)}`);
  }

  getAttachmentsInfo(attachmentIds: readonly string[], waveRef?: string): Promise<AttachmentsResponse> {
    const ids = attachmentIds.map(encodeURIComponent).join(",");
    return this.transport.getMessage(
      `/attachmentsInfo?attachmentIds=${ids}${waveRefQuery("&", waveRef)}`,
      AttachmentsResponse,
    );
  }

  async status(attachmentId: string, waveRef?: string): Promise<number> {
    const path = `/thumbnail/${encodeURIComponent(attachmentId)}${waveRefQuery("?", waveRef)}`;
    const response = await this.transport.request(path, { method: "HEAD" });
    return response.status;
  }

  async upload(attachmentId: string, waveRef: string, file: Blob, fileName: string): Promise<void> {
    const form = new FormData();
    form.append("attachmentId", attachmentId);
    form.append("waveRef", waveRef);
    form.append("uploadFormElement", file, fileName);
    await this.transport.expectOk(`/attachment/${encodeURIComponent(attachmentId)}`, {
      method: "POST",
      form,
    });
  }
}

/**
 * The HTTP API of the wave server.
 *
 * Every method rejects with {@link NetworkError} if the request got no
 * response, {@link AuthError} for a refusal the server explains, and
 * {@link HttpError} for any other unexpected status.
 */
export class WaveHttpApi {
  readonly transport: HttpTransport;
  /** Untested against a server; see {@link HttpSearchService}. */
  readonly search: SearchService;
  /** Untested against a server; see {@link HttpAttachmentService}. */
  readonly attachments: AttachmentService;
  /** Person mode: devices, pairing, recovery, contacts (device nodes). */
  readonly person: PersonApi;

  constructor(options: WaveHttpApiOptions) {
    this.transport = new HttpTransport(options);
    this.search = new HttpSearchService(this.transport);
    this.attachments = new HttpAttachmentService(this.transport);
    this.person = new PersonApi(this.transport);
  }

  get baseUrl(): string {
    return this.transport.baseUrl;
  }

  /** `GET /session`. Succeeds whether or not somebody is signed in. */
  async getSession(): Promise<SessionInfo> {
    const response = await this.transport.expectOk("/session");
    const body = jsonObject(response);
    const address = body["address"];
    const socketToken = body["socket_token"];
    return {
      domain: stringField(body, "domain", response),
      address: typeof address === "string" ? address : null,
      idSeed: stringField(body, "id", response),
      websocketAddress: stringField(body, "websocket_address", response),
      registrationEnabled: body["registration_enabled"] === true,
      ...(body["federation"] === true ? { federation: true } : {}),
      socketToken: typeof socketToken === "string" ? socketToken : null,
      ...deviceInfo(body),
    };
  }

  /**
   * `POST /auth/signin`. On success the response sets the session cookie.
   * Rejects with {@link AuthError}: `invalidCredentials`, `rateLimited`,
   * `badRequest`, `crossSite`.
   */
  async signIn(address: string, password: string): Promise<SignInResult> {
    const response = await this.transport.expectOk(
      "/auth/signin",
      { method: "POST", json: { address, password } },
      "signin",
    );
    const body = jsonObject(response);
    return {
      address: stringField(body, "address", response),
      domain: stringField(body, "domain", response),
    };
  }

  /**
   * `POST /auth/register`. Does not sign in. Rejects with
   * {@link AuthError}: `registrationDisabled`, `accountExists`, `failed`
   * (invalid address or password, with the server's text), `rateLimited`,
   * `badRequest`, `crossSite`.
   */
  async register(address: string, password: string): Promise<RegisterResult> {
    const response = await this.transport.expectOk(
      "/auth/register",
      { method: "POST", json: { address, password } },
      "register",
    );
    const body = jsonObject(response);
    return {
      address: stringField(body, "address", response),
      message: typeof body["message"] === "string" ? body["message"] : "",
    };
  }

  /**
   * `POST /auth/signout`: ends the session and every connection using it.
   * Succeeds if nobody was signed in.
   */
  async signOut(): Promise<void> {
    await this.transport.expectOk("/auth/signout", { method: "POST" });
  }

  /**
   * `GET /fetch/<waveref>`: a snapshot of a wave's root conversation
   * wavelet (as a view), of a wavelet or of a document, by what the
   * reference names. Rejects with {@link AuthError} `forbidden` for
   * anything the user may not see, including what does not exist, and
   * {@link HttpError} 404 for a reference the server cannot read.
   */
  async fetchWave(ref: WaveRef): Promise<FetchResult> {
    const path = `/fetch/${ref.encodeToUriPathSegment()}`;
    if (ref.documentId !== null) {
      return { kind: "document", snapshot: await this.transport.getMessage(path, DocumentSnapshot) };
    }
    if (ref.waveletId !== null) {
      return { kind: "wavelet", snapshot: await this.transport.getMessage(path, WaveletSnapshot) };
    }
    return { kind: "wave", snapshot: await this.transport.getMessage(path, WaveViewSnapshot) };
  }

  /** {@link WaveHttpApi.fetchWave} for a wave: its root conversation wavelet. */
  fetchWaveView(waveId: WaveId): Promise<WaveViewSnapshot> {
    return this.transport.getMessage(
      `/fetch/${WaveRef.of(waveId).encodeToUriPathSegment()}`,
      WaveViewSnapshot,
    );
  }

  /** {@link WaveHttpApi.fetchWave} for a wavelet. */
  fetchWavelet(waveId: WaveId, waveletId: WaveletId): Promise<WaveletSnapshot> {
    return this.transport.getMessage(
      `/fetch/${WaveRef.ofWavelet(waveId, waveletId).encodeToUriPathSegment()}`,
      WaveletSnapshot,
    );
  }

  /** {@link WaveHttpApi.fetchWave} for a document. */
  fetchDocument(
    waveId: WaveId,
    waveletId: WaveletId,
    documentId: string,
  ): Promise<DocumentSnapshot> {
    return this.transport.getMessage(
      `/fetch/${WaveRef.ofDocument(waveId, waveletId, documentId).encodeToUriPathSegment()}`,
      DocumentSnapshot,
    );
  }

  /**
   * `GET /profile/?addresses=a,b`: display names and avatar addresses. The
   * `imageUrl`s are server paths; see {@link WaveHttpApi.resolve}.
   */
  fetchProfiles(addresses: readonly string[]): Promise<ProfileResponse> {
    const list = addresses.map(encodeURIComponent).join(",");
    return this.transport.getMessage(`/profile/?addresses=${list}`, ProfileResponse);
  }

  /**
   * `GET /directory`: accounts of this server whose address contains
   * `query` (all for an empty query), people first. Empty if the server
   * has its directory switched off.
   */
  async directory(query: string, limit = 20): Promise<DirectoryEntry[]> {
    return this.accountList("/directory", query, limit);
  }

  /**
   * `GET /contacts?query=&limit=` (device mode): the people added as
   * contacts and everyone the user shares a conversation with, as
   * {@link directory} lists accounts. Empty if the server has no contacts.
   */
  async contacts(query: string, limit = 20): Promise<DirectoryEntry[]> {
    return this.accountList("/contacts", query, limit);
  }

  /**
   * `POST /contacts` (device mode): adds a contact by address or by a
   * `resonator:contact/<address>` link; resolves to the address added.
   */
  async addContact(addressOrLink: string): Promise<string> {
    const text = addressOrLink.trim();
    const body = text.startsWith(CONTACT_LINK_PREFIX) ? { link: text } : { address: text };
    const response = await this.transport.request("/contacts", { method: "POST", json: body });
    if (response.status !== 200 && response.status !== 201) {
      throw failure(response, "data");
    }
    return stringField(jsonObject(response), "address", response);
  }

  private async accountList(endpoint: string, query: string, limit: number): Promise<DirectoryEntry[]> {
    const path = `${endpoint}?query=${encodeURIComponent(query)}&limit=${Math.trunc(limit)}`;
    const response = await this.transport.request(path);
    if (response.status === 404) {
      return [];
    }
    if (response.status !== 200) {
      throw failure(response, "data");
    }
    const accounts = jsonObject(response)["accounts"];
    if (!Array.isArray(accounts)) {
      throw new InvalidResponseError(response.url, response.text);
    }
    const entries: DirectoryEntry[] = [];
    for (const account of accounts) {
      if (!isObject(account) || typeof account["address"] !== "string") {
        continue;
      }
      const kind = account["kind"];
      entries.push({
        address: account["address"],
        name: typeof account["name"] === "string" ? account["name"] : account["address"],
        kind: kind === "agent" || kind === "robot" ? kind : "human",
      });
    }
    return entries;
  }

  /**
   * `GET /federation/probe?address=`: whether a participant of another
   * domain can be added from here. Rejects if the server does not federate
   * or refuses (too many probes).
   */
  async probe(address: string): Promise<ProbeResult> {
    const response = await this.transport.request(`/federation/probe?address=${encodeURIComponent(address)}`);
    if (response.status !== 200) {
      throw failure(response, "data");
    }
    const body = jsonObject(response);
    const exists = body["exists"];
    const message = body["message"];
    return {
      reachable: body["reachable"] === true,
      allowed: body["allowed"] === true,
      exists: typeof exists === "boolean" ? exists : null,
      message: typeof message === "string" ? message : null,
    };
  }

  /** The address of the generated avatar of `address`, in pixels. */
  avatarUrl(address: string, width: number, height: number): string {
    return this.transport.url(
      `/iniavatars/${Math.trunc(width)}x${Math.trunc(height)}/${encodeURIComponent(address)}`,
    );
  }

  /** The absolute form of a server path such as a profile's `imageUrl`. */
  resolve(path: string): string {
    return path.startsWith("/") ? this.transport.url(path) : path;
  }

  /** `GET /locale/`: the signed-in user's stored locale, or null. */
  async getLocale(): Promise<string | null> {
    const response = await this.transport.expectOk("/locale/");
    const locale = jsonObject(response)["locale"];
    return typeof locale === "string" ? locale : null;
  }

  /**
   * `POST /locale/?locale=`: stores the signed-in user's locale (letters,
   * digits, `_` and `-`; at most 35 characters, else {@link HttpError} 400).
   */
  async setLocale(locale: string): Promise<void> {
    await this.transport.expectOk(`/locale/?locale=${encodeURIComponent(locale)}`, {
      method: "POST",
    });
  }
}

/**
 * The WebSocket address of the server at an HTTP base URL: `ws:` for
 * `http:`, `wss:` for `https:`, path `/socket`. An empty base URL means the
 * page's own origin and needs `pageUrl` (default: `location.href`).
 */
export function socketUrlFromBaseUrl(baseUrl: string, pageUrl?: string): string {
  let base = baseUrl;
  if (base === "") {
    const page =
      pageUrl ?? (globalThis as { location?: { href?: string } }).location?.href ?? undefined;
    if (page === undefined) {
      throw new TypeError("an empty base URL needs the page URL");
    }
    base = new URL(page).origin;
  }
  const url = new URL(base);
  const scheme = url.protocol === "https:" || url.protocol === "wss:" ? "wss:" : "ws:";
  return `${scheme}//${url.host}/socket`;
}

/**
 * The WebSocket address for the `host:port` of
 * {@link SessionInfo.websocketAddress}, as the original client builds it.
 */
export function socketUrlFromAddress(websocketAddress: string, secure: boolean): string {
  return `${secure ? "wss" : "ws"}://${websocketAddress}/socket`;
}

/**
 * A `getAuthToken` for an `RpcChannel` that re-reads `GET /session` before
 * every connection: the socket token of the session, or `undefined` if the
 * session has none (a bearer client then authenticates with its own token
 * if `bearerToken` is given). Rejects with {@link AuthError} `notSignedIn`
 * if nobody is signed in, which the channel takes as fatal; a
 * {@link NetworkError} makes it retry.
 */
export function sessionAuthToken(
  api: Pick<WaveHttpApi, "getSession">,
  bearerToken?: string,
): () => Promise<string | undefined> {
  return async () => {
    const session = await api.getSession();
    if (session.address === null) {
      throw new AuthError({ kind: "notSignedIn", message: "Not logged in" });
    }
    return session.socketToken ?? bearerToken;
  };
}
