// Tests of the HTTP API against a fake `fetch`, and of the cookie jar
// (also against a local HTTP server, through Node's own `fetch`).

import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { describe, expect, test } from "vitest";

import { WaveId, WaveRef, WaveletId } from "../../model/id";
import {
  AttachmentsResponse,
  DocumentSnapshot,
  ProfileResponse,
  SearchResponse,
  WaveViewSnapshot,
  WaveletSnapshot,
} from "../../wire";
import {
  AuthError,
  CONTACT_LINK_PREFIX,
  CookieJar,
  HttpError,
  HttpSearchService,
  InvalidResponseError,
  NetworkError,
  WaveHttpApi,
  sessionAuthToken,
  socketUrlFromAddress,
  socketUrlFromBaseUrl,
  withCookieJar,
} from "..";

interface Call {
  readonly url: string;
  readonly method: string;
  readonly headers: Headers;
  readonly body: unknown;
  readonly credentials: unknown;
}

interface Reply {
  readonly status?: number;
  readonly body?: string;
  readonly headers?: Record<string, string>;
}

/** A `fetch` that records its calls and answers with the given replies. */
function fakeFetch(...replies: (Reply | Error)[]): { fetch: typeof fetch; calls: Call[] } {
  const calls: Call[] = [];
  const queue = [...replies];
  const fetchFunction: typeof fetch = async (input, init) => {
    calls.push({
      url: String(input),
      method: init?.method ?? "GET",
      headers: new Headers(init?.headers),
      body: init?.body,
      credentials: init?.credentials,
    });
    const reply = queue.shift() ?? { status: 200, body: "{}" };
    if (reply instanceof Error) {
      throw reply;
    }
    return new Response(reply.body ?? "", { status: reply.status ?? 200, headers: reply.headers });
  };
  return { fetch: fetchFunction, calls };
}

function api(...replies: (Reply | Error)[]): { api: WaveHttpApi; calls: Call[] } {
  const { fetch, calls } = fakeFetch(...replies);
  return { api: new WaveHttpApi({ baseUrl: "http://wave.test:9898/", fetch }), calls };
}

const json = (value: unknown, status = 200): Reply => ({
  status,
  body: JSON.stringify(value),
  headers: { "content-type": "application/json" },
});

const failed = (status: number, message: string, headers: Record<string, string> = {}): Reply => ({
  status,
  body: JSON.stringify({ status: "FAILED", message }),
  headers,
});

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error("expected the promise to reject");
}

async function authError(promise: Promise<unknown>): Promise<AuthError> {
  const error = await rejection(promise);
  expect(error).toBeInstanceOf(AuthError);
  return error as AuthError;
}

const hash = Uint8Array.from([0xab, 0xcd]);
const documentSnapshot: DocumentSnapshot = {
  documentId: "b+1",
  documentOperation: { component: [{ characters: "hi" }] },
  author: "a@example.com",
  contributor: ["a@example.com"],
  lastModifiedVersion: 3n,
  lastModifiedTime: 1234567890123n,
};
const waveletSnapshot: WaveletSnapshot = {
  waveletId: "example.com/conv+root",
  participantId: ["a@example.com"],
  document: [documentSnapshot],
  version: { version: 3, historyHash: hash },
  lastModifiedTime: 1234567890123n,
  creator: "a@example.com",
  creationTime: 1234567890000n,
};

describe("session", () => {
  test("getSession maps the fields of a signed-in session", async () => {
    const { api: http, calls } = api(
      json({
        domain: "example.com",
        id: "AbCdEf0123",
        websocket_address: "wave.test:9898",
        registration_enabled: true,
        address: "a@example.com",
        socket_token: "wst_secret",
      }),
    );
    expect(await http.getSession()).toEqual({
      domain: "example.com",
      address: "a@example.com",
      idSeed: "AbCdEf0123",
      websocketAddress: "wave.test:9898",
      registrationEnabled: true,
      socketToken: "wst_secret",
    });
    expect(calls.length).toBe(1);
    expect(calls[0]!.url).toBe("http://wave.test:9898/session");
    expect(calls[0]!.method).toBe("GET");
    expect(calls[0]!.credentials).toBe("include");
    expect(calls[0]!.headers.get("accept")).toBe("application/json");
    expect(calls[0]!.headers.has("authorization")).toBe(false);
  });

  test("getSession without a user", async () => {
    const { api: http } = api(
      json({
        domain: "example.com",
        id: "seed",
        websocket_address: "0.0.0.0:9898",
        registration_enabled: false,
      }),
    );
    const session = await http.getSession();
    expect(session.federation).toBe(undefined);
    expect(session.address).toBe(null);
    expect(session.socketToken).toBe(null);
    expect(session.registrationEnabled).toBe(false);
  });

  test("getSession tells whether the server federates", async () => {
    const { api: http } = api(
      json({ domain: "a.example", id: "seed", websocket_address: "x:1", federation: true }),
    );
    expect((await http.getSession()).federation).toBe(true);
  });

  test("getSession of a device node", async () => {
    const { api: http } = api(
      json({
        domain: "abc.example",
        id: "seed",
        websocket_address: "127.0.0.1:1",
        address: "me@abc.example",
        mode: "device",
        device: { account: "me@abc.example", name: "phone", key: "k".repeat(52), allow_remote: false },
      }),
    );
    expect((await http.getSession()).device).toEqual({
      account: "me@abc.example",
      name: "phone",
      key: "k".repeat(52),
      allowRemote: false,
    });
    const { api: plain } = api(
      json({ domain: "a", id: "s", websocket_address: "x:1", device: { account: "me@a" } }),
    );
    // Without `mode: "device"` it is a server.
    expect((await plain.getSession()).device).toBe(undefined);
    const { api: unnamed } = api(
      json({ domain: "a", id: "s", websocket_address: "x:1", mode: "device", device: { account: "me@a" } }),
    );
    expect((await unnamed.getSession()).device).toEqual({
      account: "me@a",
      name: "",
      key: null,
      allowRemote: false,
    });
  });

  test("getSession rejects a body that is not a session", async () => {
    expect(await rejection(api({ body: "<html>" }).api.getSession())).toBeInstanceOf(
      InvalidResponseError,
    );
    expect(await rejection(api(json({ domain: "example.com" })).api.getSession())).toBeInstanceOf(
      InvalidResponseError,
    );
  });

  test("a bearer token is sent with every request", async () => {
    const { fetch, calls } = fakeFetch();
    const http = new WaveHttpApi({ baseUrl: "http://wave.test", fetch, bearerToken: "pat_1" });
    await http.signOut();
    expect(calls[0]!.headers.get("authorization")).toBe("Bearer pat_1");
  });

  test("sessionAuthToken reads the socket token of the session", async () => {
    const session = {
      domain: "example.com",
      idSeed: "s",
      websocketAddress: "h:1",
      registrationEnabled: true,
    };
    const signedIn = sessionAuthToken({
      getSession: async () => ({ ...session, address: "a@example.com", socketToken: "wst_1" }),
    });
    expect(await signedIn()).toBe("wst_1");
    const bearer = sessionAuthToken(
      { getSession: async () => ({ ...session, address: "a@example.com", socketToken: null }) },
      "pat_1",
    );
    expect(await bearer()).toBe("pat_1");
    const cookieOnly = sessionAuthToken({
      getSession: async () => ({ ...session, address: "a@example.com", socketToken: null }),
    });
    expect(await cookieOnly()).toBe(undefined);
    const signedOut = sessionAuthToken({
      getSession: async () => ({ ...session, address: null, socketToken: null }),
    });
    expect((await authError(signedOut())).kind).toBe("notSignedIn");
    const offline = sessionAuthToken({
      getSession: async () => {
        throw new NetworkError("down");
      },
    });
    expect(await rejection(offline())).toBeInstanceOf(NetworkError);
  });
});

describe("sign-in, registration, sign-out", () => {
  test("signIn posts the credentials as JSON", async () => {
    const { api: http, calls } = api(
      json({ status: "SUCCESS", address: "a@example.com", domain: "example.com" }),
    );
    expect(await http.signIn("a", "secret")).toEqual({
      address: "a@example.com",
      domain: "example.com",
    });
    const call = calls[0]!;
    expect(call.url).toBe("http://wave.test:9898/auth/signin");
    expect(call.method).toBe("POST");
    expect(call.credentials).toBe("include");
    expect(call.headers.get("content-type")).toBe("application/json");
    expect(call.headers.get("accept")).toBe("application/json");
    expect(JSON.parse(call.body as string)).toEqual({ address: "a", password: "secret" });
  });

  test("signIn error mapping", async () => {
    const wrong = await authError(
      api(failed(403, "The username or password you entered is incorrect.")).api.signIn("a", "x"),
    );
    expect(wrong.detail).toEqual({
      kind: "invalidCredentials",
      message: "The username or password you entered is incorrect.",
    });
    expect(wrong.message).toBe("The username or password you entered is incorrect.");

    const limited = await authError(
      api(
        failed(429, "Too many attempts. Try again in 17 seconds.", { "retry-after": "17" }),
      ).api.signIn("a", "x"),
    );
    expect(limited.detail).toEqual({
      kind: "rateLimited",
      retryAfterSecs: 17,
      message: "Too many attempts. Try again in 17 seconds.",
    });
    const noHeader = await authError(api(failed(429, "Too many attempts.")).api.signIn("a", "x"));
    expect(noHeader.detail).toMatchObject({ kind: "rateLimited", retryAfterSecs: null });

    const bad = await authError(
      api(failed(400, "Request body is not a JSON object")).api.signIn("a", "x"),
    );
    expect(bad.kind).toBe("badRequest");

    const crossSite = await authError(
      api(failed(403, "Cross-site request rejected")).api.signIn("a", "x"),
    );
    expect(crossSite.kind).toBe("crossSite");

    const internal = await rejection(
      api(failed(500, "Internal server error")).api.signIn("a", "x"),
    );
    expect(internal).toBeInstanceOf(HttpError);
    expect((internal as HttpError).status).toBe(500);
    expect((internal as HttpError).body).toContain("Internal server error");
    expect((internal as HttpError).url).toBe("http://wave.test:9898/auth/signin");

    expect(await rejection(api({ status: 200, body: "not json" }).api.signIn("a", "x"))).toBeInstanceOf(
      InvalidResponseError,
    );
  });

  test("a request without a response is a network error", async () => {
    const cause = new TypeError("fetch failed");
    const error = await rejection(api(cause).api.signIn("a", "x"));
    expect(error).toBeInstanceOf(NetworkError);
    expect((error as NetworkError).cause).toBe(cause);
    expect(await rejection(api(cause).api.getSession())).toBeInstanceOf(NetworkError);
    expect(await rejection(api(cause).api.fetchProfiles(["a@example.com"]))).toBeInstanceOf(
      NetworkError,
    );
  });

  test("register posts the credentials and returns the address", async () => {
    const { api: http, calls } = api(
      json({ status: "SUCCESS", message: "Registration complete.", address: "new@example.com" }),
    );
    expect(await http.register("new", "secret")).toEqual({
      address: "new@example.com",
      message: "Registration complete.",
    });
    expect(calls[0]!.url).toBe("http://wave.test:9898/auth/register");
    expect(calls[0]!.method).toBe("POST");
    expect(JSON.parse(calls[0]!.body as string)).toEqual({ address: "new", password: "secret" });
  });

  test("register error mapping", async () => {
    const register = (reply: Reply): Promise<unknown> => api(reply).api.register("new", "pw");
    expect(
      (await authError(register(failed(403, "Registration disabled by administrator")))).kind,
    ).toBe("registrationDisabled");
    expect((await authError(register(failed(403, "Account already exists")))).kind).toBe(
      "accountExists",
    );
    expect(
      (await authError(register(failed(403, "Password must be at least 8 characters long"))))
        .detail,
    ).toEqual({ kind: "failed", message: "Password must be at least 8 characters long" });
    expect((await authError(register(failed(429, "Too many", { "retry-after": "3600" })))).detail)
      .toEqual({ kind: "rateLimited", retryAfterSecs: 3600, message: "Too many" });
    expect((await authError(register(failed(400, "Request body is not a JSON object")))).kind).toBe(
      "badRequest",
    );
    expect((await authError(register(failed(403, "Cross-site request rejected")))).kind).toBe(
      "crossSite",
    );
    expect(
      await rejection(
        register(
          failed(500, "An unexpected error occurred while trying to create the account"),
        ),
      ),
    ).toBeInstanceOf(HttpError);
  });

  test("signOut posts", async () => {
    const { api: http, calls } = api(json({ status: "SUCCESS" }));
    await http.signOut();
    expect(calls[0]!.url).toBe("http://wave.test:9898/auth/signout");
    expect(calls[0]!.method).toBe("POST");
    expect(calls[0]!.headers.get("accept")).toBe("application/json");
    expect(calls[0]!.body).toBe(undefined);
    expect((await authError(api(failed(403, "Cross-site request rejected")).api.signOut())).kind).toBe(
      "crossSite",
    );
  });
});

describe("snapshots", () => {
  const waveId = WaveId.of("example.com", "w+abc");
  const waveletId = WaveletId.of("example.com", "conv+root");

  test("a wave reference returns a view snapshot", async () => {
    const view: WaveViewSnapshot = { waveId: "example.com/w+abc", wavelet: [waveletSnapshot] };
    const { api: http, calls } = api({ body: WaveViewSnapshot.encode(view) });
    const result = await http.fetchWave(WaveRef.of(waveId));
    expect(result).toEqual({ kind: "wave", snapshot: view });
    expect(calls[0]!.url).toBe("http://wave.test:9898/fetch/example.com/w+abc");
    expect(calls[0]!.credentials).toBe("include");

    const again = api({ body: WaveViewSnapshot.encode(view) });
    expect(await again.api.fetchWaveView(waveId)).toEqual(view);
    expect(again.calls[0]!.url).toBe(calls[0]!.url);
  });

  test("a wavelet reference returns a wavelet snapshot", async () => {
    const { api: http, calls } = api({ body: WaveletSnapshot.encode(waveletSnapshot) });
    const result = await http.fetchWave(WaveRef.ofWavelet(waveId, waveletId));
    expect(result).toEqual({ kind: "wavelet", snapshot: waveletSnapshot });
    expect(calls[0]!.url).toBe("http://wave.test:9898/fetch/example.com/w+abc/~/conv+root");

    const other = api({ body: WaveletSnapshot.encode(waveletSnapshot) });
    expect(
      await other.api.fetchWavelet(waveId, WaveletId.of("other.example.com", "user+a@example.com")),
    ).toEqual(waveletSnapshot);
    expect(other.calls[0]!.url).toBe(
      `http://wave.test:9898/fetch/${WaveRef.ofWavelet(
        waveId,
        WaveletId.of("other.example.com", "user+a@example.com"),
      ).encodeToUriPathSegment()}`,
    );
    expect(other.calls[0]!.url).toContain("/other.example.com/user+a");
  });

  test("a document reference returns a document snapshot", async () => {
    const { api: http, calls } = api({ body: DocumentSnapshot.encode(documentSnapshot) });
    const result = await http.fetchWave(WaveRef.ofDocument(waveId, waveletId, "b+1"));
    expect(result).toEqual({ kind: "document", snapshot: documentSnapshot });
    expect(calls[0]!.url).toBe("http://wave.test:9898/fetch/example.com/w+abc/~/conv+root/b+1");

    const again = api({ body: DocumentSnapshot.encode(documentSnapshot) });
    expect(await again.api.fetchDocument(waveId, waveletId, "b+1")).toEqual(documentSnapshot);
  });

  test("fetch error mapping", async () => {
    const ref = WaveRef.of(waveId);
    expect((await authError(api({ status: 403 }).api.fetchWave(ref))).detail).toEqual({
      kind: "forbidden",
      message: "",
    });
    expect(
      (await authError(api(failed(401, "Invalid or expired access token")).api.fetchWave(ref))).kind,
    ).toBe("unauthorized");
    const notFound = await rejection(api({ status: 404 }).api.fetchWave(ref));
    expect(notFound).toBeInstanceOf(HttpError);
    expect((notFound as HttpError).status).toBe(404);
    const invalid = await rejection(api({ body: '{"2":[]}' }).api.fetchWave(ref));
    expect(invalid).toBeInstanceOf(InvalidResponseError);
    expect((invalid as InvalidResponseError).body).toBe('{"2":[]}');
    expect(await rejection(api({ body: "<html>" }).api.fetchWave(ref))).toBeInstanceOf(
      InvalidResponseError,
    );
  });
});

describe("profiles, avatars, locale", () => {
  test("fetchProfiles lists the addresses", async () => {
    const profiles: ProfileResponse = {
      profiles: [
        {
          address: "john.smith@example.com",
          name: "John Smith",
          imageUrl: "/iniavatars/100x100/john.smith@example.com",
          profileUrl: "",
        },
      ],
    };
    const { api: http, calls } = api({ body: ProfileResponse.encode(profiles) });
    expect(await http.fetchProfiles(["john.smith@example.com", "b+c@example.com"])).toEqual(
      profiles,
    );
    expect(calls[0]!.url).toBe(
      "http://wave.test:9898/profile/?addresses=john.smith%40example.com,b%2Bc%40example.com",
    );
    expect(http.resolve(profiles.profiles[0]!.imageUrl)).toBe(
      "http://wave.test:9898/iniavatars/100x100/john.smith@example.com",
    );
    expect(http.resolve("https://elsewhere.test/a.png")).toBe("https://elsewhere.test/a.png");
    expect((await authError(api({ status: 403 }).api.fetchProfiles(["a@example.com"]))).kind).toBe(
      "forbidden",
    );
  });

  test("avatarUrl", () => {
    const { api: http } = api();
    expect(http.avatarUrl("a@example.com", 64, 48)).toBe(
      "http://wave.test:9898/iniavatars/64x48/a%40example.com",
    );
    expect(new WaveHttpApi({ baseUrl: "", fetch }).avatarUrl("a@b.c", 10, 10)).toBe(
      "/iniavatars/10x10/a%40b.c",
    );
  });

  test("getLocale", async () => {
    const { api: http, calls } = api(json({ locale: "pt-BR" }));
    expect(await http.getLocale()).toBe("pt-BR");
    expect(calls[0]!.url).toBe("http://wave.test:9898/locale/");
    expect(calls[0]!.method).toBe("GET");
    expect(await api(json({ locale: null })).api.getLocale()).toBe(null);
    expect((await authError(api({ status: 403 }).api.getLocale())).kind).toBe("forbidden");
  });

  test("setLocale", async () => {
    const { api: http, calls } = api({ status: 200, body: "" });
    await http.setLocale("en_GB");
    expect(calls[0]!.url).toBe("http://wave.test:9898/locale/?locale=en_GB");
    expect(calls[0]!.method).toBe("POST");
    const bad = await rejection(api(failed(400, "Missing or invalid locale")).api.setLocale("x y"));
    expect(bad).toBeInstanceOf(HttpError);
    expect((bad as HttpError).status).toBe(400);
    expect((await authError(api({ status: 403 }).api.setLocale("en"))).kind).toBe("forbidden");
  });
});

describe("search and attachments (no server yet)", () => {
  test("search builds the original's query", async () => {
    const response: SearchResponse = {
      query: "in:inbox",
      totalResults: 1,
      digests: [
        {
          title: "Hello",
          snippet: "Hello there",
          waveId: "example.com/w+abc",
          lastModified: 1234567890123n,
          unreadCount: 1,
          blipCount: 2,
          participants: ["a@example.com"],
          author: "a@example.com",
        },
      ],
    };
    const { api: http, calls } = api({ body: SearchResponse.encode(response) });
    expect(await http.search.search("in:inbox with:b@example.com", 10, 20)).toEqual(response);
    expect(calls[0]!.url).toBe(
      "http://wave.test:9898/search/?query=in%3Ainbox%20with%3Ab%40example.com&index=10&numResults=20",
    );
    expect(HttpSearchService.path("a b", 0, 5)).toBe("/search/?query=a%20b&index=0&numResults=5");
    expect(await rejection(api({ status: 404 }).api.search.search("q", 0, 1))).toBeInstanceOf(
      HttpError,
    );
  });

  test("attachment addresses and metadata", async () => {
    const response: AttachmentsResponse = { attachment: [] };
    const { api: http, calls } = api({ body: AttachmentsResponse.encode(response) });
    expect(http.attachments.attachmentUrl("example.com/att+1")).toBe(
      "http://wave.test:9898/attachment/example.com%2Fatt%2B1",
    );
    expect(http.attachments.thumbnailUrl("att1")).toBe("http://wave.test:9898/thumbnail/att1");
    expect(await http.attachments.getAttachmentsInfo(["att1", "att2"])).toEqual(response);
    expect(calls[0]!.url).toBe("http://wave.test:9898/attachmentsInfo?attachmentIds=att1,att2");
  });

  test("attachments of other servers name the wave, and their status is asked with HEAD", async () => {
    const response: AttachmentsResponse = { attachment: [] };
    const { api: http, calls } = api(
      { body: AttachmentsResponse.encode(response) },
      { status: 503, body: "" },
    );
    expect(http.attachments.attachmentUrl("b.example/att1", "a.example/w+abc")).toBe(
      "http://wave.test:9898/attachment/b.example%2Fatt1?waveRef=a.example%2Fw%2Babc",
    );
    expect(http.attachments.thumbnailUrl("b.example/att1", "a.example/w+abc")).toBe(
      "http://wave.test:9898/thumbnail/b.example%2Fatt1?waveRef=a.example%2Fw%2Babc",
    );
    await http.attachments.getAttachmentsInfo(["b.example/att1"], "a.example/w+abc");
    expect(calls[0]!.url).toBe(
      "http://wave.test:9898/attachmentsInfo?attachmentIds=b.example%2Fatt1&waveRef=a.example%2Fw%2Babc",
    );
    expect(await http.attachments.status("b.example/att1", "a.example/w+abc")).toBe(503);
    expect(calls[1]!.method).toBe("HEAD");
  });

  test("contacts list like the directory", async () => {
    const { api: http, calls } = api(
      json({
        accounts: [
          { address: "bob@b.example", name: "Bob", kind: "human", added: true },
          { address: "helper@a.example", kind: "agent", added: false },
          { name: "no address" },
        ],
      }),
      { status: 404, body: "" },
    );
    expect(await http.contacts("b o", 5)).toEqual([
      { address: "bob@b.example", name: "Bob", kind: "human" },
      { address: "helper@a.example", name: "helper@a.example", kind: "agent" },
    ]);
    expect(calls[0]!.url).toBe("http://wave.test:9898/contacts?query=b%20o&limit=5");
    // Not a device node.
    expect(await http.contacts("")).toEqual([]);
  });

  test("addContact posts an address or a contact link", async () => {
    const { api: http, calls } = api(
      json({ address: "bob@b.example", added: true }, 201),
      json({ address: "carol@c.example", added: false }),
      failed(400, "Not an address."),
    );
    expect(await http.addContact(" bob@b.example ")).toBe("bob@b.example");
    expect(calls[0]!.method).toBe("POST");
    expect(calls[0]!.url).toBe("http://wave.test:9898/contacts");
    expect(JSON.parse(calls[0]!.body as string)).toEqual({ address: "bob@b.example" });
    expect(await http.addContact(`${CONTACT_LINK_PREFIX}carol@c.example`)).toBe("carol@c.example");
    expect(JSON.parse(calls[1]!.body as string)).toEqual({ link: "resonator:contact/carol@c.example" });
    expect(await rejection(http.addContact("nonsense"))).toBeInstanceOf(HttpError);
  });

  test("probe asks about a remote address", async () => {
    const { api: http, calls } = api(json({ reachable: true, allowed: true, exists: false }));
    expect(await http.probe("bob@b.example")).toEqual({
      reachable: true,
      allowed: true,
      exists: false,
      message: null,
    });
    expect(calls[0]!.url).toBe("http://wave.test:9898/federation/probe?address=bob%40b.example");
    expect(await rejection(api({ status: 429, body: "{}" }).api.probe("bob@b.example"))).toBeInstanceOf(Error);
  });

  test("upload posts a multipart form", async () => {
    const { api: http, calls } = api({ status: 201, body: "OK" });
    await http.attachments.upload(
      "att1",
      "example.com/w+abc/~/conv+root",
      new Blob(["data"], { type: "text/plain" }),
      "notes.txt",
    );
    const call = calls[0]!;
    expect(call.url).toBe("http://wave.test:9898/attachment/att1");
    expect(call.method).toBe("POST");
    expect(call.credentials).toBe("include");
    // The content type, with its boundary, is left to fetch.
    expect(call.headers.has("content-type")).toBe(false);
    const form = call.body as FormData;
    expect(form).toBeInstanceOf(FormData);
    expect(form.get("attachmentId")).toBe("att1");
    expect(form.get("waveRef")).toBe("example.com/w+abc/~/conv+root");
    const file = form.get("uploadFormElement") as File;
    expect(file.name).toBe("notes.txt");
    expect(await file.text()).toBe("data");
    expect(
      (await authError(api({ status: 403 }).api.attachments.upload("a", "r", new Blob([]), "f")))
        .kind,
    ).toBe("forbidden");
  });
});

describe("socket addresses", () => {
  test("from the base URL", () => {
    expect(socketUrlFromBaseUrl("http://wave.test:9898")).toBe("ws://wave.test:9898/socket");
    expect(socketUrlFromBaseUrl("https://wave.test/")).toBe("wss://wave.test/socket");
    expect(socketUrlFromBaseUrl("", "https://wave.test/#example.com/w+abc")).toBe(
      "wss://wave.test/socket",
    );
    expect(() => socketUrlFromBaseUrl("")).toThrow(TypeError);
  });

  test("from the session's address", () => {
    expect(socketUrlFromAddress("wave.test:9898", false)).toBe("ws://wave.test:9898/socket");
    expect(socketUrlFromAddress("wave.test", true)).toBe("wss://wave.test/socket");
  });
});

describe("cookie jar", () => {
  /** A response with several `Set-Cookie` headers. */
  function setCookies(...values: string[]): Response {
    const headers = new Headers();
    for (const value of values) {
      headers.append("set-cookie", value);
    }
    return new Response("", { headers });
  }

  test("stores cookies and sends them back", async () => {
    const seen: (string | null)[] = [];
    const replies = [
      setCookies("WSESSIONID=abc; Path=/; HttpOnly; SameSite=Lax; Max-Age=1209600", "theme=dark"),
      new Response(""),
      setCookies("WSESSIONID=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
      new Response(""),
      setCookies("theme=x; Expires=Thu, 01 Jan 1970 00:00:00 GMT"),
      new Response(""),
    ];
    const jar = new CookieJar();
    const fetchWithJar = withCookieJar(async (_input, init) => {
      seen.push(new Headers(init?.headers).get("cookie"));
      return replies.shift()!;
    }, jar);

    await fetchWithJar("http://wave.test/auth/signin", { method: "POST" });
    expect(jar.get("WSESSIONID")).toBe("abc");
    expect(jar.size).toBe(2);
    await fetchWithJar("http://wave.test/session", { headers: { Accept: "application/json" } });
    await fetchWithJar("http://wave.test/auth/signout", { method: "POST" });
    expect(jar.get("WSESSIONID")).toBe(null);
    await fetchWithJar("http://wave.test/session");
    await fetchWithJar("http://wave.test/session");
    await fetchWithJar("http://wave.test/session");
    expect(seen).toEqual([
      null,
      "WSESSIONID=abc; theme=dark",
      "WSESSIONID=abc; theme=dark",
      "theme=dark",
      "theme=dark",
      null,
    ]);
    expect(jar.header()).toBe(null);
  });

  test("keeps the request's own headers and cookie", async () => {
    let headers = new Headers();
    const jar = new CookieJar();
    jar.set("a", "1");
    const fetchWithJar = withCookieJar(async (_input, init) => {
      headers = new Headers(init?.headers);
      return new Response("");
    }, jar);
    await fetchWithJar("http://wave.test/", { headers: { Cookie: "own=2", "X-Test": "yes" } });
    expect(headers.get("cookie")).toBe("own=2; a=1");
    expect(headers.get("x-test")).toBe("yes");
    await fetchWithJar(new Request("http://wave.test/", { headers: { "X-Other": "1" } }));
    expect(headers.get("x-other")).toBe("1");
    expect(headers.get("cookie")).toBe("a=1");
  });

  test("parses Set-Cookie values", () => {
    const jar = new CookieJar();
    jar.setCookie("a=1");
    jar.setCookie("b=x=y; Path=/");
    jar.setCookie("c=3; Max-Age=60");
    jar.setCookie("d=4; Expires=Fri, 01 Jan 2100 00:00:00 GMT");
    jar.setCookie("nonsense");
    jar.setCookie("=nameless");
    expect(jar.header()).toBe("a=1; b=x=y; c=3; d=4");
    jar.setCookie("a=2");
    jar.setCookie("c=; max-age=-1");
    // Max-Age wins over Expires.
    jar.setCookie("d=5; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=60");
    expect(jar.header()).toBe("a=2; b=x=y; d=5");
    jar.delete("b");
    jar.clear();
    expect(jar.size).toBe(0);
  });

  test("works with Node's fetch against an HTTP server, through the API", async () => {
    const cookiesSeen: (string | undefined)[] = [];
    const server = createServer((request, response) => {
      cookiesSeen.push(request.headers.cookie);
      if (request.url === "/auth/signin") {
        response.setHeader("Set-Cookie", [
          "WSESSIONID=s3cr3t; Path=/; HttpOnly; SameSite=Lax; Max-Age=1209600",
        ]);
        response.setHeader("Content-Type", "application/json");
        response.end('{"status":"SUCCESS","address":"a@example.com","domain":"example.com"}');
      } else if (request.url === "/auth/signout") {
        response.setHeader("Set-Cookie", "WSESSIONID=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
        response.end('{"status":"SUCCESS"}');
      } else {
        const signedIn = request.headers.cookie === "WSESSIONID=s3cr3t";
        response.setHeader("Content-Type", "application/json");
        response.end(
          JSON.stringify({
            domain: "example.com",
            id: "seed",
            websocket_address: "127.0.0.1:1",
            registration_enabled: true,
            ...(signedIn ? { address: "a@example.com", socket_token: "wst_1" } : {}),
          }),
        );
      }
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const { port } = server.address() as AddressInfo;
      const http = new WaveHttpApi({
        baseUrl: `http://127.0.0.1:${port}`,
        fetch: withCookieJar(fetch),
      });
      expect((await http.getSession()).address).toBe(null);
      await http.signIn("a", "secret");
      const session = await http.getSession();
      expect(session.address).toBe("a@example.com");
      expect(session.socketToken).toBe("wst_1");
      await http.signOut();
      expect((await http.getSession()).address).toBe(null);
      expect(cookiesSeen).toEqual([
        undefined,
        undefined,
        "WSESSIONID=s3cr3t",
        "WSESSIONID=s3cr3t",
        undefined,
      ]);
    } finally {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
  });
});
