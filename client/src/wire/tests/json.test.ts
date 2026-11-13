// Tests of the JSON form of the protocol messages.
//
// Port of `crates/resonator-model/src/proto/json/tests.rs`. `json_golden.txt`
// (in the Rust crate's testdata) was produced by `JsonGoldenGenerator.java`
// with the Java protobuf runtime, Gson and the helper classes of Apache
// Wave. There is no protobuf binary codec here, so the golden records are
// verified through JSON only: decoding the golden JSON and encoding the
// result must reproduce the golden text exactly. The other tests are
// hand-written from the templates, as in the Rust file.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";

import {
  AttachmentsResponse,
  DocumentSnapshot,
  JsonError,
  type MessageCodec,
  ProfileResponse,
  ProtocolAuthenticate,
  ProtocolDocumentOperation,
  ProtocolHashedVersion,
  ProtocolOpenRequest,
  ProtocolSubmitRequest,
  ProtocolSubmitResponse,
  ProtocolWaveletUpdate,
  RpcFinished,
  SearchRequest,
  SearchResponse,
  WaveletSnapshot,
  codecForName,
  decodeBody,
  decodeEnvelopeBody,
  encodeEnvelope,
  findCodec,
  formatJavaDouble,
  int64ToNumber,
  numberToInt64,
  parseEnvelope,
  parseJson,
  rpcFinishedFailure,
  rpcFinishedSuccess,
  writeString,
} from "..";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../../crates/resonator-model/testdata/json_golden.txt", import.meta.url),
);

function hashed(version: number, hash: readonly number[]): ProtocolHashedVersion {
  return { version, historyHash: Uint8Array.from(hash) };
}

const STRINGS: readonly string[] = [
  "",
  "plain",
  "quote \" backslash \\ slash / angle <b>&amp;</b> = '",
  "tab\tbackspace\u0008newline\nreturn\rformfeed\u000cnul\u0000unit\u001fdel\u007f",
  "line\u2028paragraph\u2029separators",
  "caf\u00e9 \u4e2d\u6587 \u{1F600} \u{10FFFF}",
  "\ufeffbom and \ufffd replacement",
  "a@example.com",
];

const INT52: readonly bigint[] = [
  0n,
  1n,
  7n,
  -1n,
  9_999_999n,
  10_000_000n,
  10_000_001n,
  12_345_678n,
  123_456_789_012n,
  10_000_000_000n,
  1_700_000_000_000n,
  1_699_999_999_999n,
  9_007_199_254_740_991n,
  9_007_199_254_740_992n,
  -10_000_000n,
  -1_234_567_890_123n,
];

const I64_MAX = (1n << 63n) - 1n;
const I64_MIN = -(1n << 63n);

const LONGS: readonly bigint[] = [
  0n,
  1n,
  -1n,
  2_147_483_647n,
  2_147_483_648n,
  4_294_967_295n,
  4_294_967_296n,
  1_700_000_000_123n,
  -1_700_000_000_123n,
  I64_MAX,
  I64_MIN,
  -4_294_967_296n,
];

/** Rust's `str::lines`. */
function lines(text: string): string[] {
  const result = text.split("\n").map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  if (result[result.length - 1] === "") {
    result.pop();
  }
  return result;
}

/** Undoes the escaping of `JsonGoldenGenerator.line`. */
function unescape(fieldText: string): string {
  return fieldText.replace(/\\u([0-9A-Fa-f]{4})/g, (_, hex: string) =>
    String.fromCharCode(Number.parseInt(hex, 16)),
  );
}

interface GoldenRecord {
  readonly kind: "M" | "E";
  readonly name: string;
  readonly seq: number | null;
  readonly json: string;
}

function goldenRecords(): GoldenRecord[] {
  return lines(readFileSync(GOLDEN_PATH, "utf8")).map((line) => {
    const fields = line.split("\t");
    if (fields[0] === "M" && fields.length === 4) {
      return { kind: "M", name: fields[1]!, seq: null, json: unescape(fields[3]!) };
    }
    if (fields[0] === "E" && fields.length === 5) {
      return {
        kind: "E",
        name: fields[1]!,
        seq: Number.parseInt(fields[2]!, 10),
        json: unescape(fields[4]!),
      };
    }
    throw new Error(`unexpected golden line ${line}`);
  });
}

/**
 * The golden records that cannot round-trip: the int52 value 2^53 is not a
 * safe integer, and this codec refuses those (see the header of json.ts).
 */
function isUnsafeInt52Record(record: GoldenRecord): boolean {
  return (
    record.name === "ProtocolHashedVersion" && record.json.includes('"1":9.007199254740992E15,')
  );
}

function errorOf(action: () => unknown): JsonError {
  try {
    action();
  } catch (error) {
    if (error instanceof JsonError) {
      return error;
    }
    throw error;
  }
  throw new Error("expected a JsonError");
}

describe("golden file", () => {
  const records = goldenRecords();

  test("matches the Java encoding", () => {
    let messages = 0;
    let envelopes = 0;
    let skipped = 0;
    for (const record of records) {
      const codec = findCodec(record.name);
      expect(codec, `golden record of unknown type ${record.name}`).toBeDefined();
      if (isUnsafeInt52Record(record)) {
        // Must be refused, not rounded.
        if (record.seq === null) {
          expect(errorOf(() => codec!.parse(record.json)).kind).toBe("type");
        } else {
          const envelope = parseEnvelope(record.json);
          expect(errorOf(() => decodeEnvelopeBody(envelope)).kind).toBe("type");
        }
        skipped++;
        continue;
      }
      if (record.seq === null) {
        const message = codec!.parse(record.json);
        expect(codec!.encode(message), record.name).toBe(record.json);
        messages++;
      } else {
        const envelope = parseEnvelope(record.json);
        expect(envelope.messageType).toBe(record.name);
        expect(envelope.sequenceNumber).toBe(record.seq);
        const message = decodeBody(envelope, codec!);
        expect(decodeEnvelopeBody(envelope)).toEqual(message);
        expect(encodeEnvelope(record.seq, codec!, message), record.name).toBe(record.json);
        envelopes++;
      }
    }
    expect(records.length).toBe(194);
    expect(messages).toBe(96);
    expect(envelopes).toBe(96);
    expect(skipped).toBe(2);
  });

  // The Rust test also rebuilds every golden input; that needs the model
  // and the random operation generator. The inputs that need neither are
  // rebuilt here, which checks the writer independently of the reader.

  function messagesNamed(name: string): string[] {
    return records.filter((r) => r.kind === "M" && r.name === name).map((r) => r.json);
  }

  test("hashed versions written from values", () => {
    const golden = messagesNamed("ProtocolHashedVersion");
    expect(golden.length).toBe(INT52.length + 2);
    INT52.forEach((version, i) => {
      if (version === 9_007_199_254_740_992n) {
        // Not a safe integer; the double formatter itself still agrees.
        expect(`{"1":${formatJavaDouble(Number(version))},"2":""}`).toBe(golden[i]);
        expect(errorOf(() => ProtocolHashedVersion.encode(hashed(Number(version), []))).kind).toBe(
          "invalidValue",
        );
        return;
      }
      expect(ProtocolHashedVersion.encode(hashed(Number(version), []))).toBe(golden[i]);
      expect(ProtocolHashedVersion.parse(golden[i]!)).toEqual(hashed(Number(version), []));
    });
    const allBytes = Array.from({ length: 256 }, (_, i) => i);
    expect(ProtocolHashedVersion.encode(hashed(3, allBytes))).toBe(golden[INT52.length]);
    const uri = Array.from("wave://example.com/w+abc/conv+root", (c) => c.charCodeAt(0));
    expect(ProtocolHashedVersion.encode(hashed(0, uri))).toBe(golden[INT52.length + 1]);
  });

  test("strings written from values", () => {
    const golden = messagesNamed("ProtocolAuthenticate");
    expect(golden.length).toBe(STRINGS.length);
    STRINGS.forEach((token, i) => {
      expect(ProtocolAuthenticate.encode({ token })).toBe(golden[i]);
      expect(ProtocolAuthenticate.parse(golden[i]!)).toEqual({ token });
    });
  });

  test("control messages written from values", () => {
    expect(messagesNamed("ProtocolAuthenticationResult")).toEqual(["{}"]);
    expect(messagesNamed("CancelRpc")).toEqual(["{}"]);
    expect(messagesNamed("RpcFinished")).toEqual([
      RpcFinished.encode(rpcFinishedSuccess()),
      RpcFinished.encode(rpcFinishedFailure("Not logged in")),
      RpcFinished.encode({ failed: true }),
    ]);
    expect(messagesNamed("ProtocolOpenRequest")).toEqual([
      ProtocolOpenRequest.encode({
        participantId: "a@example.com",
        waveId: "example.com/w+abc",
        waveletIdPrefix: [],
        knownWavelet: [],
      }),
      ProtocolOpenRequest.encode({
        participantId: "a@example.com",
        waveId: "example.com/w+abc",
        waveletIdPrefix: ["", "conv", "user+"],
        knownWavelet: [
          { waveletId: "example.com/conv+root", hashedVersion: hashed(17, [1, 2, 3]) },
          { waveletId: "example.com/user+a@example.com", hashedVersion: hashed(0, []) },
        ],
      }),
    ]);
    const allBytes = Array.from({ length: 256 }, (_, i) => i);
    expect(messagesNamed("ProtocolSubmitResponse")).toEqual([
      ProtocolSubmitResponse.encode({
        operationsApplied: 3,
        hashedVersionAfterApplication: hashed(1237, allBytes.slice(0, 20)),
      }),
      ProtocolSubmitResponse.encode({
        operationsApplied: 0,
        errorMessage: "Author field on delta must match logged in user",
      }),
      ProtocolSubmitResponse.encode({ operationsApplied: -1 }),
    ]);
    expect(messagesNamed("ProfileResponse")).toEqual([
      ProfileResponse.encode({ profiles: [] }),
      ProfileResponse.encode({
        profiles: [
          {
            address: "john.smith@example.com",
            name: "John Smith",
            imageUrl: "/iniavatars/100x100/john.smith@example.com",
            profileUrl: "",
          },
          {
            address: "@example.com",
            name: "@example.com",
            imageUrl: "/static/images/unknown.jpg",
          },
        ],
      }),
    ]);
  });

  test("every component kind written from values", () => {
    const everyKind: ProtocolDocumentOperation = {
      component: [
        { annotationBoundary: { empty: true, end: [], change: [] } },
        {
          annotationBoundary: {
            end: ["style/color", "link"],
            change: [
              { key: "k", newValue: "new" },
              { key: "k2", oldValue: "old" },
              { key: "k3", oldValue: "", newValue: "" },
            ],
          },
        },
        { characters: STRINGS[5]! },
        { elementStart: { type: "line", attribute: [{ key: "t", value: "h1" }] } },
        { elementEnd: true },
        { retainItemCount: 2_147_483_647 },
        { deleteCharacters: STRINGS[3]! },
        { deleteElementStart: { type: "p", attribute: [] } },
        { deleteElementEnd: true },
        { replaceAttributes: { empty: true, oldAttribute: [], newAttribute: [] } },
        {
          replaceAttributes: {
            oldAttribute: [{ key: "a", value: "1" }],
            newAttribute: [{ key: "b", value: "2" }],
          },
        },
        { updateAttributes: { empty: true, attributeUpdate: [] } },
        { updateAttributes: { attributeUpdate: [{ key: "a", oldValue: "1" }] } },
      ],
    };
    const golden = messagesNamed("ProtocolDocumentOperation");
    expect(golden).toEqual([
      ProtocolDocumentOperation.encode(everyKind),
      ProtocolDocumentOperation.encode({ component: [] }),
    ]);
    expect(ProtocolDocumentOperation.parse(golden[0]!)).toEqual(everyKind);
  });

  test("int64 extremes in snapshots", () => {
    const golden = messagesNamed("WaveletSnapshot");
    expect(golden.length).toBe(LONGS.length);
    LONGS.forEach((time, i) => {
      const snapshot = WaveletSnapshot.parse(golden[i]!);
      expect(snapshot.lastModifiedTime).toBe(time);
      // Rust: time.wrapping_sub(86_400_000).
      expect(snapshot.creationTime).toBe(BigInt.asIntN(64, time - 86_400_000n));
      expect(snapshot.document.length).toBe(2);
      expect(snapshot.document[0]!.lastModifiedVersion).toBe(8n);
      expect(snapshot.document[0]!.lastModifiedTime).toBe(time);
      expect(snapshot.document[1]!.lastModifiedVersion).toBe(0n);
      expect(snapshot.document[1]!.lastModifiedTime).toBe(-1n);
      expect(snapshot.version).toEqual(hashed(42, [0xde, 0xad, 0xbe, 0xef]));
    });
  });

  test("search and attachment messages written from values", () => {
    const searchResponse: SearchResponse = {
      query: "in:inbox",
      totalResults: 2,
      digests: [
        {
          title: "Hello 世界 \u{1F600}",
          snippet: ' a "quoted" <b>snippet</b>\n',
          waveId: "example.com/w+abc",
          lastModified: 1_700_000_000_123n,
          unreadCount: 3,
          blipCount: 7,
          participants: ["bob@example.com", "@example.com"],
          author: "alice@example.com",
        },
        {
          title: "",
          snippet: "",
          waveId: "example.com/w+empty",
          lastModified: -1n,
          unreadCount: 0,
          blipCount: 0,
          participants: [],
          author: "nobody@example.com",
        },
      ],
    };
    const searchRequest: SearchRequest = {
      query: "in:inbox with:@ café",
      index: 20,
      numResults: 100,
    };
    expect(messagesNamed("SearchRequest")).toEqual([SearchRequest.encode(searchRequest)]);
    expect(SearchRequest.parse(messagesNamed("SearchRequest")[0]!)).toEqual(searchRequest);
    const searchGolden = messagesNamed("SearchResponse");
    expect(searchGolden).toEqual([
      SearchResponse.encode({ query: "", totalResults: -1, digests: [] }),
      SearchResponse.encode(searchResponse),
    ]);
    expect(SearchResponse.parse(searchGolden[1]!)).toEqual(searchResponse);

    const attachments: AttachmentsResponse = {
      attachment: [
        {
          attachmentId: "example.com/att+1",
          waveRef: "example.com/w+abc/~/conv+root",
          fileName: 'café "photo".png',
          mimeType: "image/png",
          size: 5_000_000_000n,
          creator: "alice@example.com",
          attachmentUrl: "/attachment/example.com/att+1",
          thumbnailUrl: "/thumbnail/example.com/att+1",
          imageMetadata: { width: 1920, height: 1080 },
          thumbnailMetadata: { width: 200, height: 112 },
          malware: false,
        },
        {
          attachmentId: "legacy",
          waveRef: "example.com/w+abc/~/conv+root",
          fileName: "notes.txt",
          mimeType: "text/plain",
          size: 0n,
          creator: "",
          attachmentUrl: "/attachment/legacy",
          thumbnailUrl: "/thumbnail/legacy",
          thumbnailMetadata: { width: 95, height: 60 },
        },
      ],
    };
    const attachmentGolden = messagesNamed("AttachmentsResponse");
    expect(attachmentGolden).toEqual([
      AttachmentsResponse.encode({ attachment: [] }),
      AttachmentsResponse.encode(attachments),
    ]);
    expect(AttachmentsResponse.parse(attachmentGolden[1]!)).toEqual(attachments);
  });
});

// Hand-written from the templates.

test("field encodings", () => {
  expect(ProtocolHashedVersion.encode(hashed(5, [0xab, 0x01]))).toBe('{"1":5.0,"2":"AB01"}');
  expect(ProtocolHashedVersion.encode(hashed(1_700_000_000_000, []))).toBe('{"1":1.7E12,"2":""}');
  // Repeated fields are always present; unset optional fields are absent.
  const update: ProtocolWaveletUpdate = {
    waveletName: "example.com/w+a/~/conv+root",
    appliedDelta: [],
    marker: true,
  };
  expect(ProtocolWaveletUpdate.encode(update)).toBe(
    '{"1":"example.com/w+a/~/conv+root","2":[],"6":true}',
  );
  // An optional field that is undefined or null is absent.
  expect(
    ProtocolWaveletUpdate.encode({
      ...update,
      channelId: undefined,
      snapshot: null as unknown as undefined,
    }),
  ).toBe('{"1":"example.com/w+a/~/conv+root","2":[],"6":true}');
  const document: DocumentSnapshot = {
    documentId: "b+1",
    documentOperation: { component: [] },
    author: "a@example.com",
    contributor: [],
    lastModifiedVersion: 7n,
    lastModifiedTime: 1_700_000_000_123n,
  };
  expect(DocumentSnapshot.encode(document)).toBe(
    '{"1":"b+1","2":{"1":[]},"3":"a@example.com","4":[],"5":[7,0],"6":[-807049093,395]}',
  );
});

test("int64 extremes", () => {
  const document = (version: bigint, time: bigint): DocumentSnapshot => ({
    documentId: "d",
    documentOperation: { component: [] },
    author: "a",
    contributor: [],
    lastModifiedVersion: version,
    lastModifiedTime: time,
  });
  const cases: readonly (readonly [bigint, string])[] = [
    [0n, "[0,0]"],
    [-1n, "[-1,-1]"],
    [2_147_483_647n, "[2147483647,0]"],
    [2_147_483_648n, "[-2147483648,0]"],
    [4_294_967_295n, "[-1,0]"],
    [4_294_967_296n, "[0,1]"],
    [-4_294_967_296n, "[0,-1]"],
    [I64_MAX, "[-1,2147483647]"],
    [I64_MIN, "[0,-2147483648]"],
  ];
  for (const [value, text] of cases) {
    const json = DocumentSnapshot.encode(document(value, value));
    expect(json).toBe(`{"1":"d","2":{"1":[]},"3":"a","4":[],"5":${text},"6":${text}}`);
    expect(DocumentSnapshot.parse(json)).toEqual(document(value, value));
  }
  // Beyond 64 bits, or not a bigint.
  for (const value of [I64_MAX + 1n, I64_MIN - 1n, 7 as unknown as bigint]) {
    const error = errorOf(() => DocumentSnapshot.encode(document(0n, value)));
    expect(error.detail).toEqual({
      kind: "invalidValue",
      path: "6",
      expected: "a bigint within the signed 64-bit range",
    });
  }
});

test("int64 helpers", () => {
  expect(int64ToNumber(0n)).toBe(0);
  expect(int64ToNumber(1_700_000_000_123n)).toBe(1_700_000_000_123);
  expect(int64ToNumber(-9_007_199_254_740_991n)).toBe(Number.MIN_SAFE_INTEGER);
  expect(int64ToNumber(9_007_199_254_740_991n)).toBe(Number.MAX_SAFE_INTEGER);
  expect(() => int64ToNumber(9_007_199_254_740_992n)).toThrow(JsonError);
  expect(() => int64ToNumber(I64_MIN)).toThrow(JsonError);
  expect(numberToInt64(-5)).toBe(-5n);
  expect(numberToInt64(Number.MAX_SAFE_INTEGER)).toBe(9_007_199_254_740_991n);
  for (const bad of [1.5, 2 ** 53, Number.NaN, Number.POSITIVE_INFINITY]) {
    expect(() => numberToInt64(bad)).toThrow(JsonError);
  }
});

test("envelope shape", () => {
  const text = encodeEnvelope(3, RpcFinished, rpcFinishedFailure('no "way"'));
  expect(text).toBe(
    '{"messageType":"RpcFinished","sequenceNumber":3,"message":{"1":true,"2":"no \\"way\\""}}',
  );
  const parsed = parseEnvelope(text);
  expect(parsed.messageType).toBe("RpcFinished");
  expect(parsed.sequenceNumber).toBe(3);
  expect(decodeBody(parsed, RpcFinished)).toEqual(rpcFinishedFailure('no "way"'));
  expect(decodeEnvelopeBody(parsed)).toEqual(rpcFinishedFailure('no "way"'));
  expect(codecForName(parsed.messageType)).toBe(RpcFinished);

  // An unknown message type parses, but has no codec.
  const unknown = parseEnvelope('{"messageType":"Nope","sequenceNumber":-2,"message":{}}');
  expect(unknown.sequenceNumber).toBe(-2);
  const error = errorOf(() => decodeEnvelopeBody(unknown));
  expect(error.detail).toEqual({ kind: "unknownType", name: "Nope" });
  expect(error.message).toBe("Unknown proto class: Nope");
  expect(findCodec("Nope")).toBeUndefined();

  // The sequence number is a signed 32-bit integer; required fields of the
  // body are checked.
  for (const bad of [1.5, 2 ** 31, Number.NaN]) {
    expect(errorOf(() => encodeEnvelope(bad, RpcFinished, rpcFinishedSuccess())).kind).toBe(
      "invalidValue",
    );
  }
  expect(errorOf(() => encodeEnvelope(1, RpcFinished, {} as RpcFinished)).detail).toEqual({
    kind: "missingField",
    field: "RpcFinished.failed",
  });
});

test("reading is lenient where the original is", () => {
  // Plain integers and exponent forms for int52, lower-case hex, unknown
  // keys, null for absent, unsigned low word.
  expect(ProtocolHashedVersion.parse('{"1":12,"2":"ab0F","99":{"x":1},"3":null}')).toEqual(
    hashed(12, [0xab, 0x0f]),
  );
  expect(ProtocolHashedVersion.parse('{"1":1.7E12,"2":""}')).toEqual(
    hashed(1_700_000_000_000, []),
  );
  expect(ProtocolHashedVersion.parse('{"1":-1.0,"2":""}')).toEqual(hashed(-1, []));
  expect(Object.is(ProtocolHashedVersion.parse('{"1":-0.0,"2":""}').version, 0)).toBe(true);
  const response = ProtocolSubmitResponse.parse('{"1":2.0,"2":null}');
  expect(response.operationsApplied).toBe(2);
  expect(response.errorMessage).toBeUndefined();
  expect("errorMessage" in response).toBe(false);
  const document = DocumentSnapshot.parse(
    '{"1":"d","2":{"1":[]},"3":"a@b.c","5":[4294967295,0],"6":[-1,-1]}',
  );
  expect(document.lastModifiedVersion).toBe(4_294_967_295n);
  expect(document.lastModifiedTime).toBe(-1n);
  // An absent or null repeated field reads as empty.
  expect(document.contributor).toEqual([]);
  expect(ProtocolWaveletUpdate.parse('{"1":"n","2":null}').appliedDelta).toEqual([]);
});

test("int52 values outside the safe integer range are refused", () => {
  // 2^53 - 1 is the largest version; 2^53 cannot be told from 2^53 + 1.
  expect(ProtocolHashedVersion.parse('{"1":9.007199254740991E15,"2":""}').version).toBe(
    Number.MAX_SAFE_INTEGER,
  );
  expect(ProtocolHashedVersion.encode(hashed(Number.MAX_SAFE_INTEGER, []))).toBe(
    '{"1":9.007199254740991E15,"2":""}',
  );
  expect(ProtocolHashedVersion.encode(hashed(Number.MIN_SAFE_INTEGER, []))).toBe(
    '{"1":-9.007199254740991E15,"2":""}',
  );
  for (const text of [
    '{"1":9.007199254740992E15,"2":""}',
    '{"1":9007199254740993,"2":""}',
    '{"1":-9007199254740992,"2":""}',
    '{"1":9223372036854775807,"2":""}',
  ]) {
    expect(errorOf(() => ProtocolHashedVersion.parse(text)).detail).toEqual({
      kind: "type",
      path: "1",
      expected: "an integral number within the safe integer range",
    });
  }
  for (const version of [2 ** 53, -(2 ** 53), 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    expect(errorOf(() => ProtocolHashedVersion.encode(hashed(version, []))).detail).toEqual({
      kind: "invalidValue",
      path: "1",
      expected: "a safe integer",
    });
  }
});

test("malformed input is an error", () => {
  const bad = [
    "",
    "nonsense",
    "[]",
    "3",
    '{"1":"five","2":""}',
    '{"1":5.5,"2":""}',
    '{"1":1e300,"2":""}',
    '{"1":1e400,"2":""}',
    '{"1":5,"2":"ABC"}',
    '{"1":5,"2":"zz"}',
    '{"1":5,"2":7}',
    '{"1":[5,0],"2":""}',
    // Missing required fields.
    '{"1":5}',
    "{}",
  ];
  for (const text of bad) {
    expect(() => ProtocolHashedVersion.parse(text), `accepted ${text}`).toThrow(JsonError);
  }
  expect(errorOf(() => ProtocolHashedVersion.parse("nonsense")).kind).toBe("syntax");
  expect(errorOf(() => ProtocolHashedVersion.parse("[]")).message).toBe(
    "field : expected an object (ProtocolHashedVersion)",
  );
  expect(errorOf(() => ProtocolHashedVersion.parse('{"1":5}')).message).toBe(
    "message missing required field: ProtocolHashedVersion.history_hash",
  );
  expect(errorOf(() => ProtocolHashedVersion.parse("{}")).detail).toEqual({
    kind: "missingField",
    field: "ProtocolHashedVersion.version",
  });
  for (const text of [
    '{"1":"d","2":{"1":[]},"3":"a","5":[1],"6":[0,0]}',
    '{"1":"d","2":{"1":[]},"3":"a","5":7,"6":[0,0]}',
    '{"1":"d","2":{"1":[]},"3":"a","5":[0,4294967296],"6":[0,0]}',
    '{"1":"d","2":{"1":[]},"3":"a","5":[4294967296,0],"6":[0,0]}',
    '{"1":"d","2":{"1":[]},"3":"a","5":[0,0,0],"6":[0,0]}',
    '{"1":"d","2":{"1":[]},"3":"a","5":[0.5,0],"6":[0,0]}',
    '{"1":"d","2":{"1":[]},"3":"a","5":[null,0],"6":[0,0]}',
    '{"1":"d","2":{"1":{}},"3":"a","5":[0,0],"6":[0,0]}',
    '{"1":"d","2":{"1":[{"5":2147483648}]},"3":"a","5":[0,0],"6":[0,0]}',
    '{"1":"d","2":{"1":[{"4":"yes"}]},"3":"a","5":[0,0],"6":[0,0]}',
    '{"1":"d","2":{"1":[null]},"3":"a","5":[0,0],"6":[0,0]}',
    '{"1":"d","2":[],"3":"a","5":[0,0],"6":[0,0]}',
  ]) {
    expect(() => DocumentSnapshot.parse(text), `accepted ${text}`).toThrow(JsonError);
  }
  for (const text of [
    '{"sequenceNumber":1,"message":{}}',
    '{"messageType":"X","message":{}}',
    '{"messageType":"X","sequenceNumber":"1","message":{}}',
    '{"messageType":"X","sequenceNumber":4294967296,"message":{}}',
    '{"messageType":7,"sequenceNumber":1,"message":{}}',
    "[1,2,3]",
  ]) {
    expect(() => parseEnvelope(text), `accepted ${text}`).toThrow(JsonError);
  }
  // A missing body is an error for a message with required fields only.
  const envelope = parseEnvelope('{"messageType":"X","sequenceNumber":1}');
  expect(envelope.message).toBeNull();
  expect(() => decodeBody(envelope, ProtocolAuthenticate)).toThrow(JsonError);
  // Required fields are checked in nested and repeated messages.
  expect(
    errorOf(() => ProtocolOpenRequest.parse('{"1":"a@b.c","2":"w","3":[],"4":[{"1":"x"}]}')).detail,
  ).toEqual({ kind: "missingField", field: "WaveletVersion.hashed_version" });
  expect(
    errorOf(() =>
      ProtocolOpenRequest.parse('{"1":"a@b.c","2":"w","3":[],"4":[{"1":"x","2":{"1":0}}]}'),
    ).detail,
  ).toEqual({ kind: "missingField", field: "ProtocolHashedVersion.history_hash" });
});

test("error paths name the field", () => {
  const error = errorOf(() =>
    ProtocolSubmitRequest.parse(
      '{"1":"n","2":{"1":{"1":0,"2":""},"2":"a@b.c","3":[{"3":{"1":"d","2":{"1":[{"5":"x"}]}}}],"4":[]}}',
    ),
  );
  expect(error.message).toBe("field 2.3.3.2.1.5: expected a 32-bit integer");
  expect(error.detail).toEqual({
    kind: "type",
    path: "2.3.3.2.1.5",
    expected: "a 32-bit integer",
  });
});

test("unpaired surrogates are rejected when reading", () => {
  for (const text of [
    '{"1":"\\ud83d"}',
    '{"1":"\\ude00"}',
    '{"1":"a\\ud83dz"}',
    '{"1":"\\ud83dA"}',
    '{"1":"\\ude00\\ud83d"}',
    // Unescaped in the text.
    `{"1":"a${String.fromCharCode(0xd83d)}z"}`,
  ]) {
    expect(errorOf(() => ProtocolAuthenticate.parse(text)).kind, `accepted ${text}`).toBe("syntax");
  }
  // Even in a part of the envelope that is not looked at, or in a key.
  expect(() =>
    parseEnvelope(
      '{"messageType":"ProtocolAuthenticate","sequenceNumber":1,"message":{"9":"\\udc00"}}',
    ),
  ).toThrow(JsonError);
  expect(errorOf(() => parseJson('{"a":[[{"\\udc00":1}]]}')).kind).toBe("syntax");
  // And in a value that did not come from parseJson.
  expect(errorOf(() => ProtocolAuthenticate.decode({ "1": "\ud83d" })).kind).toBe("syntax");
  // A proper pair is fine, escaped or not.
  expect(ProtocolAuthenticate.parse('{"1":"\u{1F600}"}').token).toBe("\u{1F600}");
  expect(ProtocolAuthenticate.parse('{"1":"\\ud83d\\ude00"}').token).toBe("\u{1F600}");
});

test("unpaired surrogates are refused when writing", () => {
  for (const token of ["\ud83d", "\ude00", "a\ud83dz", "\ud83dA", "\ude00\ud83d", "ok\ud83d"]) {
    expect(errorOf(() => ProtocolAuthenticate.encode({ token })).detail).toEqual({
      kind: "invalidValue",
      path: "1",
      expected: "a string without unpaired surrogates",
    });
  }
  const error = errorOf(() =>
    encodeEnvelope(1, ProtocolWaveletUpdate, {
      waveletName: "n",
      appliedDelta: [
        {
          hashedVersion: hashed(0, []),
          author: "a@b.c",
          operation: [{ addParticipant: "\udc00@b.c" }],
          addressPath: [],
        },
      ],
    }),
  );
  expect(error.message).toBe(
    "cannot encode field message.2.3.1: expected a string without unpaired surrogates",
  );
  expect(ProtocolAuthenticate.encode({ token: "\u{1F600}\u{10FFFF}" })).toBe(
    '{"1":"\u{1F600}\u{10FFFF}"}',
  );
});

test("string escapes follow Gson", () => {
  expect(writeString("")).toBe('""');
  expect(writeString("plain / <b>&amp;</b> = '")).toBe("\"plain / <b>&amp;</b> = '\"");
  expect(writeString('"\\')).toBe('"\\"\\\\"');
  expect(writeString("\t\b\n\r\f")).toBe('"\\t\\b\\n\\r\\f"');
  expect(writeString("\u0000\u0001\u000b\u001f\u007f")).toBe('"\\u0000\\u0001\\u000b\\u001f\u007f"');
  expect(writeString("a\u2028b\u2029c")).toBe('"a\\u2028b\\u2029c"');
  expect(writeString("caf\u00e9 \u4e2d\u6587 \u{1F600}")).toBe('"caf\u00e9 \u4e2d\u6587 \u{1F600}"');
  for (const s of STRINGS) {
    expect(JSON.parse(writeString(s))).toBe(s);
  }
});

test("double formatting follows Java", () => {
  const cases: readonly (readonly [number, string])[] = [
    [0, "0.0"],
    [-0, "0.0"],
    [-1, "-1.0"],
    [9_999_999, "9999999.0"],
    [10_000_000, "1.0E7"],
    [12_345_678, "1.2345678E7"],
    [-10_000_000, "-1.0E7"],
    [120_000_000_000, "1.2E11"],
    // 2^53 + 1 is held as 2^53, as in a Java double.
    [Number("9007199254740993"), "9.007199254740992E15"],
  ];
  for (const [value, text] of cases) {
    expect(formatJavaDouble(value)).toBe(text);
  }
  expect(() => formatJavaDouble(1.5)).toThrow(JsonError);
});

test("values of the wrong type are refused when writing", () => {
  const encode = (message: unknown): JsonError =>
    errorOf(() => (ProtocolSubmitResponse as MessageCodec<unknown>).encode(message));
  expect(encode({ operationsApplied: 1.5 }).detail).toEqual({
    kind: "invalidValue",
    path: "1",
    expected: "a 32-bit integer",
  });
  expect(encode({ operationsApplied: 2 ** 31 }).kind).toBe("invalidValue");
  expect(encode({ operationsApplied: 1, errorMessage: 7 }).kind).toBe("invalidValue");
  expect(encode({ operationsApplied: 1, hashedVersionAfterApplication: 7 }).kind).toBe(
    "invalidValue",
  );
  expect(
    encode({ operationsApplied: 1, hashedVersionAfterApplication: { version: 1 } }).detail,
  ).toEqual({ kind: "missingField", field: "ProtocolHashedVersion.history_hash" });
  expect(
    encode({
      operationsApplied: 1,
      hashedVersionAfterApplication: { version: 1, historyHash: "AB" },
    }).detail,
  ).toEqual({ kind: "invalidValue", path: "3.2", expected: "a Uint8Array" });
  expect(encode(null).kind).toBe("invalidValue");
  expect(encode({}).detail).toEqual({
    kind: "missingField",
    field: "ProtocolSubmitResponse.operations_applied",
  });
  // A repeated field must be an array.
  expect(
    errorOf(() =>
      (ProtocolWaveletUpdate as MessageCodec<unknown>).encode({ waveletName: "n" }),
    ).detail,
  ).toEqual({ kind: "invalidValue", path: "2", expected: "an array" });
});

test("required fields of search and attachment messages", () => {
  expect(errorOf(() => SearchResponse.parse('{"1":"q","2":0,"3":[{"1":"t"}]}')).detail).toEqual({
    kind: "missingField",
    field: "Digest.snippet",
  });
  expect(errorOf(() => SearchRequest.parse('{"1":"q","2":0}')).detail).toEqual({
    kind: "missingField",
    field: "SearchRequest.num_results",
  });
  expect(
    errorOf(() =>
      AttachmentsResponse.parse(
        '{"1":[{"1":"a","2":"w","3":"f","4":"m","5":[0,0],"6":"","7":"u","8":"t","9":{"1":640}}]}',
      ),
    ).detail,
  ).toEqual({ kind: "missingField", field: "ImageMetadata.height" });
});
