// Tests of the RPC channel against a fake socket, fake timers and a fake
// network status.

import { afterEach, describe, expect, test } from "vitest";

import {
  CancelRpc,
  JsonError,
  type MessageCodec,
  ProtocolAuthenticationResult,
  ProtocolOpenRequest,
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  ProtocolWaveletUpdate,
  RpcFinished,
  encodeEnvelope,
  parseEnvelope,
} from "../../wire";
import {
  AuthError,
  type CloseInfo,
  ClosedError,
  DisconnectedError,
  NetworkError,
  RpcChannel,
  type RpcChannelOptions,
  RpcFailedError,
  type RpcFatalError,
  RpcProtocolError,
  type StateChange,
  type StreamHandlers,
} from "..";
import { FakeNetwork, FakeSocket, FakeTimers, flush } from "./fakes";

const URL = "ws://wave.test/socket";

interface Harness {
  readonly channel: RpcChannel;
  readonly sockets: FakeSocket[];
  readonly timers: FakeTimers;
  readonly network: FakeNetwork;
  readonly states: StateChange[];
  readonly fatals: RpcFatalError[];
  readonly protocolErrors: RpcProtocolError[];
  /** The most recent socket. */
  socket(): FakeSocket;
  /** The names of the states gone through so far. */
  stateNames(): string[];
}

const channels: RpcChannel[] = [];

afterEach(() => {
  for (const channel of channels.splice(0)) {
    channel.close();
  }
});

function harness(options: Partial<RpcChannelOptions> = {}): Harness {
  const sockets: FakeSocket[] = [];
  const timers = new FakeTimers();
  const network = new FakeNetwork();
  const channel = new RpcChannel({
    url: URL,
    socketFactory: (url) => {
      const socket = new FakeSocket(url);
      sockets.push(socket);
      return socket;
    },
    timers,
    network,
    // No jitter unless a test asks for it.
    random: () => 0.5,
    ...options,
  });
  channels.push(channel);
  const states: StateChange[] = [];
  const fatals: RpcFatalError[] = [];
  const protocolErrors: RpcProtocolError[] = [];
  channel.events.on("state", (change) => states.push(change));
  channel.events.on("fatal", (error) => fatals.push(error));
  channel.events.on("protocolError", (error) => protocolErrors.push(error));
  return {
    channel,
    sockets,
    timers,
    network,
    states,
    fatals,
    protocolErrors,
    socket: () => sockets[sockets.length - 1]!,
    stateNames: () => states.map((s) => s.state),
  };
}

/** A channel connected in cookie mode. */
async function connected(options: Partial<RpcChannelOptions> = {}): Promise<Harness> {
  const h = harness(options);
  h.channel.connect();
  await flush();
  h.socket().open();
  expect(h.channel.state).toBe("connected");
  return h;
}

const frame = <T>(sequenceNumber: number, codec: MessageCodec<T>, message: T): string =>
  encodeEnvelope(sequenceNumber, codec, message);

const finished = (sequenceNumber: number, errorText?: string): string =>
  frame(
    sequenceNumber,
    RpcFinished,
    errorText === undefined ? { failed: false } : { failed: true, errorText },
  );

const hash = Uint8Array.from([1, 2, 3]);

const submitRequest = (): ProtocolSubmitRequest => ({
  waveletName: "example.com/w+a/~/conv+root",
  delta: {
    hashedVersion: { version: 3, historyHash: hash },
    author: "a@example.com",
    operation: [{ noOp: true }],
    addressPath: [],
  },
  channelId: "ch1",
});

const submitResponse = (operationsApplied = 1): ProtocolSubmitResponse => ({
  operationsApplied,
  hashedVersionAfterApplication: { version: 4, historyHash: hash },
});

const openRequest = (): ProtocolOpenRequest => ({
  participantId: "a@example.com",
  waveId: "example.com/w+a",
  waveletIdPrefix: [""],
  knownWavelet: [],
});

const update = (channelId: string): ProtocolWaveletUpdate => ({
  waveletName: "example.com/w+a/~/dummy+root",
  appliedDelta: [],
  channelId,
});

interface StreamLog {
  readonly log: string[];
  readonly disconnects: CloseInfo[];
  readonly handlers: StreamHandlers<ProtocolWaveletUpdate>;
}

function streamLog(name = "stream", log: string[] = []): StreamLog {
  const disconnects: CloseInfo[] = [];
  return {
    log,
    disconnects,
    handlers: {
      onMessage: (message) => log.push(`${name}:update:${message.channelId}`),
      onFinished: (error) => log.push(`${name}:finished:${error ?? "ok"}`),
      onDisconnected: (info) => {
        disconnects.push(info);
        log.push(`${name}:disconnected:${info.code}`);
      },
    },
  };
}

function open(h: Harness, log: StreamLog) {
  return h.channel.openStream(ProtocolOpenRequest, openRequest(), ProtocolWaveletUpdate, log.handlers);
}

function submit(h: Harness, log: string[], name = "submit") {
  return h.channel.callWithCallback(
    ProtocolSubmitRequest,
    submitRequest(),
    ProtocolSubmitResponse,
    (response) => log.push(`${name}:response:${response.operationsApplied}`),
    (error) => log.push(`${name}:error:${error.name}:${error.message}`),
  );
}

async function rejection(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error("expected the promise to reject");
}

describe("authentication", () => {
  test("sends the token in the first frame and connects on the result", async () => {
    const h = harness({ getAuthToken: async () => "tok" });
    expect(h.channel.state).toBe("idle");
    h.channel.connect();
    await flush();
    expect(h.sockets.length).toBe(1);
    expect(h.socket().url).toBe(URL);
    expect(h.channel.state).toBe("connecting");
    expect(h.socket().sent).toEqual([]);

    h.socket().open();
    expect(h.channel.state).toBe("authenticating");
    expect(h.socket().sent).toEqual([
      '{"messageType":"ProtocolAuthenticate","sequenceNumber":1,"message":{"1":"tok"}}',
    ]);
    // Not usable yet.
    expect(() => submit(h, [])).toThrow(DisconnectedError);

    h.socket().receive(frame(1, ProtocolAuthenticationResult, {}));
    expect(h.channel.state).toBe("connected");
    expect(h.stateNames()).toEqual(["connecting", "authenticating", "connected"]);
    expect(h.states[0]).toEqual({ state: "connecting", previous: "idle" });
    expect(h.fatals).toEqual([]);
    // The connect timeout is disarmed.
    expect(h.timers.pending).toBe(0);
  });

  test("a refused token is fatal", async () => {
    const h = harness({ getAuthToken: () => "bad" });
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().receive(finished(1, "Auth token invalid"));

    expect(h.channel.state).toBe("closed");
    expect(h.fatals.length).toBe(1);
    expect(h.fatals[0]!.detail).toEqual({
      kind: "authenticationFailed",
      message: "Auth token invalid",
    });
    expect(h.channel.fatal).toBe(h.fatals[0]);
    expect(h.socket().closeCalls).toBe(1);
    expect(h.socket().attached).toBe(false);
    expect(h.timers.pending).toBe(0);
    expect(h.network.listenerCount).toBe(0);
    h.timers.advance(600_000);
    expect(h.sockets.length).toBe(1);
    expect(() => submit(h, [])).toThrow(ClosedError);
  });

  test("the server's authentication timeout is fatal", async () => {
    const h = harness({ getAuthToken: () => "tok" });
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().serverClose(1008, "authentication timeout");

    expect(h.channel.state).toBe("closed");
    expect(h.fatals.map((f) => f.kind)).toEqual(["authenticationTimeout"]);
    expect(h.timers.pending).toBe(0);
  });

  test("a transient authentication failure is retried", async () => {
    const h = harness({ getAuthToken: () => "tok" });
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().receive(finished(1, "Internal error"));

    expect(h.channel.state).toBe("waiting");
    expect(h.fatals).toEqual([]);
    expect(h.sockets[0]!.closeCalls).toBe(1);
    h.timers.advance(500);
    await flush();
    expect(h.sockets.length).toBe(2);
  });

  test("an unexpected answer to the authentication is a protocol error and retried", async () => {
    const h = harness({ getAuthToken: () => "tok" });
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().receive(frame(1, ProtocolSubmitResponse, submitResponse()));

    expect(h.channel.state).toBe("waiting");
    expect(h.protocolErrors.map((e) => e.kind)).toEqual(["unexpectedMessage"]);
    expect(h.fatals).toEqual([]);
  });

  test("a drop while authenticating is retried", async () => {
    const h = harness({ getAuthToken: () => "tok" });
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().serverClose(1006, "");
    expect(h.channel.state).toBe("waiting");
    expect(h.fatals).toEqual([]);
  });

  test("getAuthToken rejecting with an auth error is fatal", async () => {
    const cause = new AuthError({ kind: "notSignedIn", message: "Not logged in" });
    const h = harness({ getAuthToken: () => Promise.reject(cause) });
    h.channel.connect();
    await flush();

    expect(h.sockets.length).toBe(0);
    expect(h.channel.state).toBe("closed");
    expect(h.fatals[0]!.detail).toEqual({ kind: "authTokenUnavailable", cause });
    expect(h.timers.pending).toBe(0);
  });

  test("getAuthToken failing otherwise is retried with backoff", async () => {
    let calls = 0;
    const h = harness({
      getAuthToken: () => {
        calls += 1;
        if (calls < 3) {
          throw new NetworkError("no route");
        }
        return "tok";
      },
    });
    h.channel.connect();
    await flush();
    expect(h.channel.state).toBe("waiting");
    expect(h.states[1]!.retryInMs).toBe(500);
    expect(h.states[1]!.detail!.origin).toBe("local");
    h.timers.advance(500);
    await flush();
    expect(h.states[h.states.length - 1]!.retryInMs).toBe(1000);
    h.timers.advance(1000);
    await flush();
    expect(calls).toBe(3);
    expect(h.sockets.length).toBe(1);
    expect(h.fatals).toEqual([]);
  });

  test("the token is asked for again on every attempt", async () => {
    let n = 0;
    const h = harness({ getAuthToken: async () => `tok${++n}` });
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().receive(frame(1, ProtocolAuthenticationResult, {}));
    h.socket().serverClose(1001, "server shutting down");
    h.timers.advance(500);
    await flush();
    h.socket().open();
    const sent = parseEnvelope(h.socket().sent[0]!);
    expect(sent.messageType).toBe("ProtocolAuthenticate");
    expect(sent.message).toEqual({ "1": "tok2" });
    // Sequence numbers go on across connections.
    expect(sent.sequenceNumber).toBe(2);
  });

  test("cookie mode sends no frame", async () => {
    const h = harness();
    h.channel.connect();
    await flush();
    expect(h.channel.state).toBe("connecting");
    h.socket().open();
    expect(h.channel.state).toBe("connected");
    expect(h.socket().sent).toEqual([]);
    expect(h.stateNames()).toEqual(["connecting", "connected"]);
  });

  test("the url may be a function returning a promise", async () => {
    let n = 0;
    const h = harness({ url: async () => `ws://host${++n}/socket` });
    h.channel.connect();
    await flush();
    expect(h.socket().url).toBe("ws://host1/socket");
    h.socket().serverClose();
    h.timers.advance(500);
    await flush();
    expect(h.socket().url).toBe("ws://host2/socket");
  });
});

describe("calls", () => {
  test("responses are matched to requests by sequence number", async () => {
    const h = await connected();
    const first = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    const second = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    const sent = h.socket().sent.map((text) => parseEnvelope(text));
    expect(sent.map((e) => [e.messageType, e.sequenceNumber])).toEqual([
      ["ProtocolSubmitRequest", 1],
      ["ProtocolSubmitRequest", 2],
    ]);
    expect(h.socket().sent[0]).toBe(frame(1, ProtocolSubmitRequest, submitRequest()));

    h.socket().receive(frame(2, ProtocolSubmitResponse, submitResponse(7)));
    h.socket().receive(frame(1, ProtocolSubmitResponse, submitResponse(5)));
    expect((await first).operationsApplied).toBe(5);
    expect(await second).toEqual(submitResponse(7));
  });

  test("a failed call rejects with the server's text", async () => {
    const h = await connected();
    const call = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    h.socket().receive(finished(1, "Unknown proto class: X"));
    const error = await rejection(call);
    expect(error).toBeInstanceOf(RpcFailedError);
    expect((error as RpcFailedError).errorText).toBe("Unknown proto class: X");
  });

  test("a call finished without a response fails", async () => {
    const h = await connected();
    const log: string[] = [];
    submit(h, log);
    h.socket().receive(finished(1));
    expect(log).toEqual(["submit:error:RpcFailedError:The RPC finished without a response"]);
  });

  test("a second response to a call is ignored", async () => {
    const h = await connected();
    const log: string[] = [];
    submit(h, log);
    h.socket().receive(frame(1, ProtocolSubmitResponse, submitResponse()));
    h.socket().receive(frame(1, ProtocolSubmitResponse, submitResponse()));
    h.socket().receive(finished(1, "late"));
    expect(log).toEqual(["submit:response:1"]);
    expect(h.protocolErrors).toEqual([]);
  });

  test("an interim response does not end the call", async () => {
    const h = await connected();
    const log: string[] = [];
    h.channel.callWithCallback(
      ProtocolSubmitRequest,
      submitRequest(),
      ProtocolSubmitResponse,
      (response) => log.push(`response:${response.operationsApplied}`),
      (error) => log.push(`error:${error.message}`),
      (response) => {
        if (response.pending !== true) {
          return false;
        }
        log.push("pending");
        return true;
      },
    );
    h.socket().receive(
      frame(1, ProtocolSubmitResponse, { ...submitResponse(0), pending: true }),
    );
    expect(log).toEqual(["pending"]);
    h.socket().receive(frame(1, ProtocolSubmitResponse, submitResponse(3)));
    h.socket().receive(frame(1, ProtocolSubmitResponse, submitResponse(9)));
    expect(log).toEqual(["pending", "response:3"]);
    expect(h.protocolErrors).toEqual([]);
  });

  test("calls fail fast when not connected", async () => {
    const h = harness();
    expect(() => submit(h, [])).toThrow(DisconnectedError);
    const error = await rejection(
      h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse),
    );
    expect(error).toBeInstanceOf(DisconnectedError);
    expect((error as DisconnectedError).close).toBe(null);
    expect(() => open(h, streamLog())).toThrow(DisconnectedError);

    h.channel.connect();
    await flush();
    // Connecting is not connected.
    expect(() => submit(h, [])).toThrow(DisconnectedError);
    expect(h.socket().sent).toEqual([]);
  });

  test("a message that cannot be encoded is not sent", async () => {
    const h = await connected();
    const bad = { ...submitRequest(), waveletName: "\ud800" };
    expect(() =>
      h.channel.callWithCallback(
        ProtocolSubmitRequest,
        bad,
        ProtocolSubmitResponse,
        () => {},
        () => {},
      ),
    ).toThrow(JsonError);
    expect(h.socket().sent).toEqual([]);
    expect(
      await rejection(h.channel.call(ProtocolSubmitRequest, bad, ProtocolSubmitResponse)),
    ).toBeInstanceOf(JsonError);
    // The channel still works.
    const log: string[] = [];
    const { sequenceNumber } = submit(h, log);
    h.socket().receive(frame(sequenceNumber, ProtocolSubmitResponse, submitResponse()));
    expect(log).toEqual(["submit:response:1"]);
  });

  test("a send that throws fails the call", async () => {
    const h = await connected();
    h.socket().failSend = true;
    const log: string[] = [];
    expect(() => submit(h, log)).toThrow(DisconnectedError);
    expect(log).toEqual([]);
  });
});

describe("streams", () => {
  test("updates are delivered until the stream is cancelled", async () => {
    const h = await connected();
    const s = streamLog();
    const stream = open(h, s);
    expect(stream.sequenceNumber).toBe(1);
    expect(h.socket().sent).toEqual([frame(1, ProtocolOpenRequest, openRequest())]);

    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("a")));
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("b")));
    expect(s.log).toEqual(["stream:update:a", "stream:update:b"]);

    stream.cancel();
    stream.cancel();
    expect(h.socket().sent.slice(1)).toEqual([frame(1, CancelRpc, {})]);
    // An update that crossed the cancel is not delivered.
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("c")));
    expect(s.log.length).toBe(2);

    h.socket().receive(finished(1));
    expect(s.log).toEqual(["stream:update:a", "stream:update:b", "stream:finished:ok"]);
    // Over: later frames and cancels do nothing.
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("d")));
    stream.cancel();
    expect(s.log.length).toBe(3);
    expect(h.socket().sent.length).toBe(2);
    expect(h.protocolErrors).toEqual([]);
  });

  test("a failed stream reports the server's text", async () => {
    const h = await connected();
    const s = streamLog();
    const stream = open(h, s);
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("a")));
    h.socket().receive(finished(1, "Not logged in"));
    expect(s.log).toEqual(["stream:update:a", "stream:finished:Not logged in"]);
    // Nothing to cancel any more.
    stream.cancel();
    expect(h.socket().sent.length).toBe(1);
  });

  test("a submit response and stream updates are delivered in arrival order", async () => {
    const h = await connected();
    const log: string[] = [];
    open(h, streamLog("view", log));
    submit(h, log);
    const promised = h.channel
      .call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse)
      .then(() => log.push("promise"));

    const socket = h.socket();
    socket.receive(frame(1, ProtocolWaveletUpdate, update("a")));
    expect(log).toEqual(["view:update:a"]);
    socket.receive(frame(2, ProtocolSubmitResponse, submitResponse(1)));
    // Synchronously, inside the frame handler.
    expect(log).toEqual(["view:update:a", "submit:response:1"]);
    socket.receive(frame(3, ProtocolSubmitResponse, submitResponse(1)));
    socket.receive(frame(1, ProtocolWaveletUpdate, update("b")));
    expect(log).toEqual(["view:update:a", "submit:response:1", "view:update:b"]);
    // The promise form settles later: after the update that followed it.
    await promised;
    expect(log).toEqual(["view:update:a", "submit:response:1", "view:update:b", "promise"]);
  });

  test("a request made inside a callback is sent at once", async () => {
    const h = await connected();
    const log: string[] = [];
    h.channel.openStream(ProtocolOpenRequest, openRequest(), ProtocolWaveletUpdate, {
      onMessage: () => {
        log.push("update");
        submit(h, log);
      },
      onFinished: () => {},
      onDisconnected: () => {},
    });
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("a")));
    expect(h.socket().sent.length).toBe(2);
    expect(parseEnvelope(h.socket().sent[1]!).sequenceNumber).toBe(2);
    h.socket().receive(frame(2, ProtocolSubmitResponse, submitResponse(3)));
    expect(log).toEqual(["update", "submit:response:3"]);
  });

  test("frames for unknown sequence numbers are ignored", async () => {
    const h = await connected();
    const s = streamLog();
    open(h, s);
    h.socket().receive(frame(99, ProtocolWaveletUpdate, update("x")));
    h.socket().receive(finished(98, "late"));
    h.socket().receive(frame(97, ProtocolSubmitResponse, submitResponse()));
    // Not even decoded: an unknown type for an unknown request is ignored.
    h.socket().receive('{"messageType":"Nonsense","sequenceNumber":96,"message":{}}');
    expect(s.log).toEqual([]);
    expect(h.protocolErrors).toEqual([]);
    expect(h.channel.state).toBe("connected");
  });

  test("a callback that throws does not disturb the channel", async () => {
    const logged: string[] = [];
    const h = await connected({ logger: { log: (level, message) => logged.push(`${level}:${message}`) } });
    const s = streamLog();
    h.channel.openStream(ProtocolOpenRequest, openRequest(), ProtocolWaveletUpdate, {
      onMessage: () => {
        throw new Error("boom");
      },
      onFinished: () => {},
      onDisconnected: () => {},
    });
    open(h, s);
    h.channel.events.on("state", () => {
      throw new Error("listener boom");
    });
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("a")));
    h.socket().receive(frame(2, ProtocolWaveletUpdate, update("b")));
    expect(s.log).toEqual(["stream:update:b"]);
    expect(logged).toContain("error:a channel callback threw");
    h.socket().serverClose(1006, "");
    expect(h.channel.state).toBe("waiting");
    expect(s.log).toEqual(["stream:update:b", "stream:disconnected:1006"]);
    expect(logged).toContain("error:an event listener threw");
  });
});

describe("undecodable frames", () => {
  test("a frame that is not an envelope is a protocol error and nothing else", async () => {
    const h = await connected();
    const s = streamLog();
    open(h, s);
    h.socket().receive("garbage");
    h.socket().receive(new Uint8Array([1, 2, 3]));
    h.socket().receive('{"messageType":"ProtocolWaveletUpdate","message":{}}');
    h.socket().receive('{"messageType":"X","sequenceNumber":1,"message":{"1":"\\ud800"}}');
    expect(h.protocolErrors.map((e) => e.detail.kind)).toEqual([
      "undecodableFrame",
      "undecodableFrame",
      "undecodableFrame",
      "undecodableFrame",
    ]);
    expect(h.protocolErrors.every((e) => e.detail.kind === "undecodableFrame" && e.detail.sequenceNumber === null)).toBe(true);
    expect(h.channel.state).toBe("connected");
    // The stream goes on.
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("a")));
    expect(s.log).toEqual(["stream:update:a"]);
  });

  test("an undecodable update ends its stream but no other", async () => {
    const h = await connected();
    const log: string[] = [];
    open(h, streamLog("one", log));
    open(h, streamLog("two", log));
    submit(h, log);

    // An update without its required wavelet name.
    h.socket().receive('{"messageType":"ProtocolWaveletUpdate","sequenceNumber":1,"message":{"2":[]}}');
    expect(h.protocolErrors.length).toBe(1);
    expect(h.protocolErrors[0]!.detail).toMatchObject({
      kind: "undecodableFrame",
      sequenceNumber: 1,
    });
    expect(h.protocolErrors[0]!.cause).toBeInstanceOf(JsonError);
    expect(log.length).toBe(1);
    expect(log[0]).toMatch(/^one:finished:undecodable frame for request 1/);
    // The server is told to stop the stream that was given up.
    expect(h.socket().sent[h.socket().sent.length - 1]).toBe(frame(1, CancelRpc, {}));

    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("late")));
    h.socket().receive(frame(2, ProtocolWaveletUpdate, update("b")));
    h.socket().receive(frame(3, ProtocolSubmitResponse, submitResponse()));
    expect(log.slice(1)).toEqual(["two:update:b", "submit:response:1"]);
    expect(h.channel.state).toBe("connected");
  });

  test("a response of the wrong type fails its call", async () => {
    const h = await connected();
    const log: string[] = [];
    submit(h, log);
    h.socket().receive(frame(1, ProtocolWaveletUpdate, update("a")));
    expect(log).toEqual([
      "submit:error:RpcProtocolError:unexpected ProtocolWaveletUpdate for request 1, expected ProtocolSubmitResponse",
    ]);
    expect(h.protocolErrors.map((e) => e.kind)).toEqual(["unexpectedMessage"]);
    expect(h.channel.state).toBe("connected");
  });

  test("an undecodable response fails its call", async () => {
    const h = await connected();
    const call = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    h.socket().receive('{"messageType":"ProtocolSubmitResponse","sequenceNumber":1,"message":{"1":"x"}}');
    expect(await rejection(call)).toBeInstanceOf(RpcProtocolError);
    expect(h.protocolErrors.length).toBe(1);
  });
});

describe("disconnects", () => {
  test("a drop fails pending calls and streams, then the channel reconnects", async () => {
    const h = await connected();
    const log: string[] = [];
    const s = streamLog("view", log);
    open(h, s);
    submit(h, log);
    const call = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    const first = h.socket();

    first.serverClose(1006, "");
    expect(h.channel.state).toBe("waiting");
    expect(log).toEqual([
      "view:disconnected:1006",
      "submit:error:DisconnectedError:disconnected: 1006",
    ]);
    expect(s.disconnects).toEqual([{ code: 1006, reason: "", origin: "remote" }]);
    const error = await rejection(call);
    expect(error).toBeInstanceOf(DisconnectedError);
    expect((error as DisconnectedError).close).toEqual({ code: 1006, reason: "", origin: "remote" });
    const change = h.states[h.states.length - 1]!;
    expect(change).toEqual({
      state: "waiting",
      previous: "connected",
      detail: { code: 1006, reason: "", origin: "remote" },
      retryInMs: 500,
    });
    expect(first.attached).toBe(false);
    expect(() => submit(h, log)).toThrow(DisconnectedError);

    h.timers.advance(499);
    await flush();
    expect(h.sockets.length).toBe(1);
    h.timers.advance(1);
    await flush();
    expect(h.sockets.length).toBe(2);
    expect(h.channel.state).toBe("connecting");
    h.socket().open();
    expect(h.channel.state).toBe("connected");

    // Nothing is replayed, and the old socket is dead to the channel.
    expect(h.socket().sent).toEqual([]);
    first.receive(frame(1, ProtocolWaveletUpdate, update("ghost")));
    first.serverClose(1006, "");
    expect(h.channel.state).toBe("connected");
    expect(log.length).toBe(2);
    // New requests get new numbers.
    expect(submit(h, log).sequenceNumber).toBe(4);
  });

  test("the state is already changed when the callbacks of a drop run", async () => {
    const h = await connected();
    const seen: string[] = [];
    h.channel.openStream(ProtocolOpenRequest, openRequest(), ProtocolWaveletUpdate, {
      onMessage: () => {},
      onFinished: () => {},
      onDisconnected: () => {
        seen.push(h.channel.state);
        try {
          submit(h, []);
        } catch (error) {
          seen.push((error as Error).name);
        }
      },
    });
    h.socket().serverClose(1001, "server shutting down");
    expect(seen).toEqual(["waiting", "DisconnectedError"]);
  });

  test("delays grow by the factor and are capped at the maximum", async () => {
    const h = harness();
    h.channel.connect();
    const delays: number[] = [];
    for (let i = 0; i < 9; i++) {
      await flush();
      // The connection is refused.
      h.socket().error();
      h.socket().serverClose(1006, "");
      expect(h.channel.state).toBe("waiting");
      const delay = h.states[h.states.length - 1]!.retryInMs!;
      delays.push(delay);
      expect(h.timers.delays).toEqual([delay]);
      h.timers.advance(delay);
    }
    expect(delays).toEqual([500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
    expect(h.sockets.length).toBe(9);
  });

  test("backoff options are honoured", async () => {
    const h = harness({ backoff: { initialMs: 100, maxMs: 1000, factor: 3, jitter: 0 }, random: () => 0.99 });
    h.channel.connect();
    const delays: number[] = [];
    for (let i = 0; i < 5; i++) {
      await flush();
      h.socket().serverClose(1006, "");
      delays.push(h.timers.delays[0]!);
      h.timers.advance(delays[i]!);
    }
    expect(delays).toEqual([100, 300, 900, 1000, 1000]);
  });

  test("jitter spreads the delay within its bounds", async () => {
    const delaysFor = async (random: number): Promise<number[]> => {
      const h = harness({ random: () => random });
      h.channel.connect();
      const delays: number[] = [];
      for (let i = 0; i < 8; i++) {
        await flush();
        h.socket().serverClose(1006, "");
        delays.push(h.timers.delays[0]!);
        h.timers.advance(delays[i]!);
      }
      return delays;
    };
    // The lowest and (almost) the highest random number: -30% and +30%,
    // but never beyond the maximum.
    expect(await delaysFor(0)).toEqual([350, 700, 1400, 2800, 5600, 11200, 21000, 21000]);
    const high = await delaysFor(0.999999);
    expect(high.slice(0, 6)).toEqual([650, 1300, 2600, 5200, 10400, 20800]);
    expect(high.slice(6)).toEqual([30000, 30000]);
    for (const random of [0.1, 0.37, 0.8]) {
      (await delaysFor(random)).forEach((delay, i) => {
        const base = Math.min(30000, 500 * 2 ** i);
        expect(delay).toBeGreaterThanOrEqual(base * 0.7);
        expect(delay).toBeLessThanOrEqual(Math.min(30000, base * 1.3));
      });
    }
  });

  test("the backoff starts over after a connection that authenticated", async () => {
    const h = harness({ getAuthToken: () => "tok" });
    h.channel.connect();
    for (const expected of [500, 1000, 2000]) {
      await flush();
      h.socket().serverClose(1006, "");
      expect(h.timers.delays).toEqual([expected]);
      h.timers.advance(expected);
    }
    await flush();
    h.socket().open();
    // Opening alone proves nothing.
    h.socket().serverClose(1006, "");
    expect(h.timers.delays).toEqual([4000]);
    h.timers.advance(4000);
    await flush();
    h.socket().open();
    const authentication = parseEnvelope(h.socket().sent[0]!);
    h.socket().receive(frame(authentication.sequenceNumber, ProtocolAuthenticationResult, {}));
    expect(h.channel.state).toBe("connected");
    h.socket().serverClose(1001, "server shutting down");
    expect(h.timers.delays).toEqual([500]);
  });

  test("in cookie mode the backoff starts over once a frame arrives", async () => {
    const h = harness();
    h.channel.connect();
    await flush();
    h.socket().open();
    h.socket().serverClose(1006, "");
    expect(h.timers.delays).toEqual([500]);
    h.timers.advance(500);
    await flush();
    h.socket().open();
    // Connected, but closed again without a frame: the delay keeps growing.
    h.socket().serverClose(1006, "");
    expect(h.timers.delays).toEqual([1000]);
    h.timers.advance(1000);
    await flush();
    h.socket().open();
    const log: string[] = [];
    const { sequenceNumber } = submit(h, log);
    h.socket().receive(frame(sequenceNumber, ProtocolSubmitResponse, submitResponse()));
    h.socket().serverClose(1006, "");
    expect(h.timers.delays).toEqual([500]);
  });

  test.each([
    [1001, "server shutting down"],
    [1008, "idle timeout"],
    [1008, "client too slow"],
    [1006, ""],
    [1011, "unexpected"],
  ])("close %i %s is followed by a reconnect", async (code, reason) => {
    const h = await connected();
    h.socket().serverClose(code, reason);
    expect(h.channel.state).toBe("waiting");
    expect(h.states[h.states.length - 1]!.detail).toEqual({ code, reason, origin: "remote" });
    expect(h.fatals).toEqual([]);
    expect(h.protocolErrors).toEqual([]);
    h.timers.advance(500);
    await flush();
    expect(h.sockets.length).toBe(2);
  });

  test.each([
    [1009, "message too big"],
    [1002, "sequence number of an open RPC reused"],
    [1003, "text frames only"],
    [1007, "not a JSON envelope"],
  ])("close %i is a protocol error, then a reconnect with backoff", async (code, reason) => {
    const h = await connected();
    const s = streamLog();
    open(h, s);
    h.socket().serverClose(code, reason);
    expect(h.channel.state).toBe("waiting");
    expect(h.protocolErrors.length).toBe(1);
    expect(h.protocolErrors[0]!.detail).toEqual({
      kind: "protocolClose",
      close: { code, reason, origin: "remote" },
    });
    expect(s.log).toEqual([`stream:disconnected:${code}`]);
    expect(h.fatals).toEqual([]);
    // Not at once.
    await flush();
    expect(h.sockets.length).toBe(1);
    expect(h.timers.delays).toEqual([500]);
    h.timers.advance(500);
    await flush();
    expect(h.sockets.length).toBe(2);
    // And the delay grows if it happens again before anything works.
    h.socket().open();
    h.socket().serverClose(code, reason);
    expect(h.timers.delays).toEqual([1000]);
  });

  test("signed out is fatal: no reconnect", async () => {
    const h = await connected();
    const log: string[] = [];
    const s = streamLog("view", log);
    open(h, s);
    const call = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    const waiting = h.channel.whenConnected();
    h.socket().serverClose(1008, "signed out");

    expect(h.channel.state).toBe("closed");
    expect(h.fatals.length).toBe(1);
    expect(h.fatals[0]!.detail).toEqual({
      kind: "signedOut",
      close: { code: 1008, reason: "signed out", origin: "remote" },
    });
    expect(log).toEqual(["view:disconnected:1008"]);
    expect(await rejection(call)).toBeInstanceOf(DisconnectedError);
    // Already connected when asked, so this one resolved.
    await waiting;
    expect(await rejection(h.channel.whenConnected())).toBe(h.fatals[0]);
    expect(h.timers.pending).toBe(0);
    expect(h.network.listenerCount).toBe(0);
    h.timers.advance(600_000);
    await flush();
    h.network.set(false);
    h.network.set(true);
    h.channel.reconnectNow();
    h.channel.connect();
    await flush();
    expect(h.sockets.length).toBe(1);
    expect(h.stateNames()).toEqual(["connecting", "connected", "closed"]);
  });

  test("a socket factory that throws is retried", async () => {
    let fail = true;
    const sockets: FakeSocket[] = [];
    const h = harness({
      socketFactory: (url) => {
        if (fail) {
          throw new Error("bad url");
        }
        const socket = new FakeSocket(url);
        sockets.push(socket);
        return socket;
      },
    });
    h.channel.connect();
    await flush();
    expect(h.channel.state).toBe("waiting");
    fail = false;
    h.timers.advance(500);
    await flush();
    expect(sockets.length).toBe(1);
  });

  test("an attempt that takes too long is given up", async () => {
    const h = harness();
    h.channel.connect();
    await flush();
    expect(h.timers.delays).toEqual([30000]);
    h.timers.advance(30000);
    expect(h.channel.state).toBe("waiting");
    expect(h.states[h.states.length - 1]!.detail).toEqual({
      code: 1006,
      reason: "connect timeout",
      origin: "local",
    });
    expect(h.sockets[0]!.closeCalls).toBe(1);
    // The abandoned socket opening late changes nothing.
    h.sockets[0]!.open();
    expect(h.channel.state).toBe("waiting");

    // Also while the credential is being fetched.
    const slow = harness({ getAuthToken: () => new Promise<string>(() => {}), connectTimeoutMs: 1000 });
    slow.channel.connect();
    await flush();
    slow.timers.advance(1000);
    expect(slow.channel.state).toBe("waiting");
    expect(slow.sockets.length).toBe(0);
  });

  test("dropConnection behaves like a network failure", async () => {
    const h = await connected();
    const s = streamLog();
    open(h, s);
    h.channel.dropConnection();
    expect(h.channel.state).toBe("waiting");
    expect(s.log).toEqual(["stream:disconnected:1006"]);
    expect(s.disconnects[0]).toEqual({ code: 1006, reason: "dropped by client", origin: "local" });
    expect(h.sockets[0]!.closeCalls).toBe(1);
    // Nothing to drop now.
    h.channel.dropConnection();
    expect(h.states.length).toBe(3);
    h.timers.advance(500);
    await flush();
    h.socket().open();
    expect(h.channel.state).toBe("connected");
  });

  test("reconnectNow skips the wait", async () => {
    const h = await connected();
    h.socket().serverClose(1006, "");
    expect(h.timers.pending).toBe(1);
    h.channel.reconnectNow();
    expect(h.channel.state).toBe("connecting");
    await flush();
    expect(h.sockets.length).toBe(2);
    // The backoff timer is gone; only the connect timeout runs.
    expect(h.timers.delays).toEqual([30000]);
    // No effect while connecting or connected.
    h.channel.reconnectNow();
    h.socket().open();
    h.channel.reconnectNow();
    await flush();
    expect(h.sockets.length).toBe(2);
    expect(h.timers.pending).toBe(0);
  });

  test("reconnectNow starts an idle channel", async () => {
    const h = harness();
    h.channel.reconnectNow();
    await flush();
    expect(h.sockets.length).toBe(1);
    expect(h.network.listenerCount).toBe(1);
  });
});

describe("network status", () => {
  test("no attempts while offline; connects when the network returns", async () => {
    const h = harness();
    h.network.online = false;
    h.channel.connect();
    await flush();
    expect(h.channel.state).toBe("offline");
    expect(h.sockets.length).toBe(0);
    expect(h.timers.pending).toBe(0);
    h.timers.advance(600_000);
    await flush();
    expect(h.sockets.length).toBe(0);

    h.network.set(true);
    expect(h.channel.state).toBe("connecting");
    await flush();
    h.socket().open();
    expect(h.stateNames()).toEqual(["offline", "connecting", "connected"]);
  });

  test("going offline drops the connection and waits for the network", async () => {
    const h = await connected();
    const s = streamLog();
    open(h, s);
    h.network.set(false);
    expect(h.channel.state).toBe("offline");
    expect(s.disconnects).toEqual([{ code: 1006, reason: "offline", origin: "local" }]);
    expect(h.sockets[0]!.closeCalls).toBe(1);
    expect(h.timers.pending).toBe(0);

    h.network.set(true);
    await flush();
    expect(h.sockets.length).toBe(2);
    h.socket().open();
    expect(h.channel.state).toBe("connected");
  });

  test("going offline while waiting cancels the retry", async () => {
    const h = await connected();
    h.socket().serverClose(1006, "");
    expect(h.timers.pending).toBe(1);
    h.network.set(false);
    expect(h.channel.state).toBe("offline");
    expect(h.timers.pending).toBe(0);
  });

  test("a drop while offline does not schedule a retry", async () => {
    const h = await connected();
    // The platform has not told the listeners yet.
    h.network.online = false;
    h.socket().serverClose(1006, "");
    expect(h.channel.state).toBe("offline");
    expect(h.timers.pending).toBe(0);
  });

  test("the network returning while waiting connects at once", async () => {
    const h = await connected();
    h.socket().serverClose(1006, "");
    h.network.set(true);
    expect(h.channel.state).toBe("connecting");
    await flush();
    expect(h.sockets.length).toBe(2);
    expect(h.timers.delays).toEqual([30000]);
  });

  test("reconnectNow tries even when reported offline", async () => {
    const h = harness();
    h.network.online = false;
    h.channel.connect();
    h.channel.reconnectNow();
    await flush();
    expect(h.sockets.length).toBe(1);
    h.socket().open();
    expect(h.channel.state).toBe("connected");
  });
});

describe("close", () => {
  test("releases the socket, the timers and everything pending", async () => {
    const h = await connected();
    const log: string[] = [];
    const s = streamLog("view", log);
    open(h, s);
    submit(h, log);
    const call = h.channel.call(ProtocolSubmitRequest, submitRequest(), ProtocolSubmitResponse);
    const socket = h.socket();

    h.channel.close();
    expect(h.channel.state).toBe("closed");
    expect(socket.closeCalls).toBe(1);
    expect(socket.attached).toBe(false);
    expect(h.timers.pending).toBe(0);
    expect(h.network.listenerCount).toBe(0);
    expect(log).toEqual(["view:disconnected:1000", "submit:error:ClosedError:the channel is closed"]);
    expect(s.disconnects[0]).toEqual({ code: 1000, reason: "closed by client", origin: "local" });
    expect(await rejection(call)).toBeInstanceOf(ClosedError);
    expect(h.fatals).toEqual([]);
    expect(h.channel.fatal).toBe(null);

    // Permanent.
    expect(() => submit(h, log)).toThrow(ClosedError);
    expect(() => open(h, s)).toThrow(ClosedError);
    expect(await rejection(h.channel.whenConnected())).toBeInstanceOf(ClosedError);
    h.channel.close();
    h.channel.dispose();
    h.channel.connect();
    h.channel.reconnectNow();
    h.channel.dropConnection();
    socket.serverClose(1000, "");
    await flush();
    h.timers.advance(600_000);
    expect(h.sockets.length).toBe(1);
    expect(h.states[h.states.length - 1]).toEqual({
      state: "closed",
      previous: "connected",
      detail: { code: 1000, reason: "closed by client", origin: "local" },
    });
    expect(h.states.length).toBe(3);
  });

  test("while waiting it cancels the retry", async () => {
    const h = await connected();
    h.socket().serverClose(1006, "");
    expect(h.timers.pending).toBe(1);
    h.channel.close();
    expect(h.timers.pending).toBe(0);
    h.timers.advance(600_000);
    await flush();
    expect(h.sockets.length).toBe(1);
  });

  test("while connecting it abandons the attempt", async () => {
    const h = harness({ getAuthToken: async () => "tok" });
    h.channel.connect();
    // Before the credential arrives.
    h.channel.close();
    await flush();
    expect(h.sockets.length).toBe(0);
    expect(h.timers.pending).toBe(0);

    const other = harness();
    other.channel.connect();
    await flush();
    other.channel.dispose();
    expect(other.socket().closeCalls).toBe(1);
    other.socket().open();
    expect(other.channel.state).toBe("closed");
    expect(other.timers.pending).toBe(0);
  });

  test("an idle channel can be closed", () => {
    const h = harness();
    h.channel.close();
    expect(h.channel.state).toBe("closed");
    h.channel.connect();
    expect(h.sockets.length).toBe(0);
  });
});

describe("whenConnected", () => {
  test("resolves at once when connected, else on the next connection", async () => {
    const h = harness();
    let resolved = false;
    const waiting = h.channel.whenConnected().then(() => {
      resolved = true;
    });
    h.channel.connect();
    await flush();
    expect(resolved).toBe(false);
    h.socket().open();
    await waiting;
    expect(resolved).toBe(true);
    await h.channel.whenConnected();

    h.socket().serverClose(1006, "");
    let again = false;
    const next = h.channel.whenConnected().then(() => {
      again = true;
    });
    await flush();
    expect(again).toBe(false);
    h.timers.advance(500);
    await flush();
    h.socket().open();
    await next;
  });

  test("rejects when the channel is closed first", async () => {
    const h = harness();
    const waiting = h.channel.whenConnected();
    h.channel.close();
    expect(await rejection(waiting)).toBeInstanceOf(ClosedError);
  });
});
