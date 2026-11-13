// The JSON form of the protocol messages, as spoken on the client WebSocket
// and served by the fetch, profile, search and attachment-info endpoints.
//
// Port of `crates/resonator-model/src/proto/json.rs`. This is the encoding
// produced by the `proto` and `gson` templates of Apache Wave's `pst`
// message generator together with `GsonUtil`, `JsonLongHelper`, `Codec` and
// `Int52` of `org.waveprotocol.wave.communication`; Apache Wave is licensed
// under the Apache License, Version 2.0. The rules:
//
// - A message is a JSON object keyed by the decimal field number (`"1"`),
//   fields in declaration order.
// - An optional field is present only if set; a repeated field is always
//   present as an array, possibly empty; a required field is always present.
// - `string` is a JSON string, `bool` a JSON boolean, `int32` a JSON
//   integer, a nested message a JSON object.
// - `bytes` is a string of upper-case hexadecimal digits.
// - `int64` is a two-element array `[low, high]` of the signed 32-bit
//   halves, except for fields carrying the `int52` option, which are JSON
//   numbers written the way Java's `Double.toString` prints them (`7.0`,
//   `1.2E10`).
// - Strings are escaped as Gson's `JsonWriter` does without HTML safety:
//   `"` and `\`, the short escapes `\t \b \n \r \f`, `\u00xx` for the other
//   control characters, and `\u2028`, `\u2029`.
//
// The writer builds the text itself, so its output is byte for byte what
// the Rust (and Java) server writes.
//
// Representation of 64-bit integers in the message types:
// - `int64` fields without the `int52` option are `bigint` and must lie in
//   the signed 64-bit range; they round-trip exactly. Use `int64ToNumber` /
//   `numberToInt64` to convert to the model's numbers.
// - `int52` fields (`ProtocolHashedVersion.version`) are `number` and must
//   be safe integers (|n| <= 2^53 - 1). The writer refuses anything else and
//   so does the reader: a JSON number beyond that range cannot be told from
//   its neighbours once parsed, so it is an error rather than a silently
//   rounded version. (The Rust reader accepts the whole signed 64-bit range
//   there; the Java client holds a double.)
//
// Reading is as lenient as the original where that is harmless: unknown
// keys are ignored, `null` counts as absent, numeric fields accept any
// integral JSON number (`2.0`, `1.7E12`), hexadecimal digits may be lower
// case, and the low word of an `int64` may be given unsigned.
//
// Unpaired UTF-16 surrogates: the server cannot represent them and closes
// the connection on them. The writer refuses to encode a string holding one
// (`invalidValue`), and `parseJson` / `parseEnvelope` reject any text whose
// strings or keys hold one, anywhere, as a `syntax` error, like the Rust
// parser. String fields read from an already parsed value are checked too.
//
// Required fields are checked for the whole message tree on both encode and
// decode (`missingField`), like `isInitialized` of the Java runtime.

import { bytesToHex, tryHexToBytes } from "./hex";

/** The reason for a {@link JsonError}. */
export type JsonErrorDetail =
  /** The text is not JSON, or holds an unpaired surrogate. */
  | { readonly kind: "syntax"; readonly message: string }
  /**
   * A value read has the wrong JSON type or is out of range. `path` holds
   * the field numbers from the outermost message inwards, dot-separated.
   */
  | { readonly kind: "type"; readonly path: string; readonly expected: string }
  /** A required field is not set; `field` is `Message.proto_field_name`. */
  | { readonly kind: "missingField"; readonly field: string }
  /** The envelope names a message type that is not registered. */
  | { readonly kind: "unknownType"; readonly name: string }
  /** A value to be written cannot be encoded. `path` as for `type`. */
  | { readonly kind: "invalidValue"; readonly path: string; readonly expected: string };

function jsonErrorMessage(d: JsonErrorDetail): string {
  switch (d.kind) {
    case "syntax":
      return `invalid JSON: ${d.message}`;
    case "type":
      return `field ${d.path}: expected ${d.expected}`;
    case "missingField":
      return `message missing required field: ${d.field}`;
    case "unknownType":
      return `Unknown proto class: ${d.name}`;
    case "invalidValue":
      return `cannot encode field ${d.path}: expected ${d.expected}`;
  }
}

/** Failure to write a message as JSON or to read one from JSON. */
export class JsonError extends Error {
  readonly detail: JsonErrorDetail;

  constructor(detail: JsonErrorDetail) {
    super(jsonErrorMessage(detail));
    this.name = "JsonError";
    this.detail = detail;
  }

  get kind(): JsonErrorDetail["kind"] {
    return this.detail.kind;
  }
}

function expected(what: string): JsonError {
  return new JsonError({ kind: "type", path: "", expected: what });
}

function invalid(what: string): JsonError {
  return new JsonError({ kind: "invalidValue", path: "", expected: what });
}

/** Prefixes the path of a type or value error with the field it is in. */
function within(error: unknown, field: string): unknown {
  if (error instanceof JsonError) {
    const d = error.detail;
    if (d.kind === "type" || d.kind === "invalidValue") {
      const path = d.path === "" ? field : `${field}.${d.path}`;
      return new JsonError({ kind: d.kind, path, expected: d.expected });
    }
  }
  return error;
}

// Strings.

/** Whether `s` holds no unpaired UTF-16 surrogate. */
export function isWellFormed(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0xd800 || c > 0xdfff) {
      continue;
    }
    if (c >= 0xdc00) {
      return false;
    }
    const next = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
    if (next < 0xdc00 || next > 0xdfff) {
      return false;
    }
    i++;
  }
  return true;
}

// Characters that need an escape, and surrogates, which need a check.
const NEEDS_CARE = /["\\\u0000-\u001f\u2028\u2029\ud800-\udfff]/;

/**
 * `s` as a JSON string, escaped as Gson's `JsonWriter` does. Throws
 * {@link JsonError} (`invalidValue`) if `s` holds an unpaired surrogate.
 */
export function writeString(s: string): string {
  if (!NEEDS_CARE.test(s)) {
    return `"${s}"`;
  }
  if (!isWellFormed(s)) {
    throw invalid("a string without unpaired surrogates");
  }
  let out = '"';
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    let escape: string;
    switch (c) {
      case 0x22:
        escape = '\\"';
        break;
      case 0x5c:
        escape = "\\\\";
        break;
      case 0x09:
        escape = "\\t";
        break;
      case 0x08:
        escape = "\\b";
        break;
      case 0x0a:
        escape = "\\n";
        break;
      case 0x0d:
        escape = "\\r";
        break;
      case 0x0c:
        escape = "\\f";
        break;
      case 0x2028:
        escape = "\\u2028";
        break;
      case 0x2029:
        escape = "\\u2029";
        break;
      default:
        if (c >= 0x20) {
          continue;
        }
        escape = `\\u${c.toString(16).padStart(4, "0")}`;
    }
    out += s.slice(start, i) + escape;
    start = i + 1;
  }
  return `${out}${s.slice(start)}"`;
}

// Numbers.

const INT32_MIN = -0x8000_0000;
const INT32_MAX = 0x7fff_ffff;
const INT64_MIN = -(1n << 63n);
const INT64_MAX = (1n << 63n) - 1n;

/**
 * An integral value the way Java's `Double.toString` prints the `double`
 * holding it: `7.0`, `1.0E7`, `1.7E12`. `value` must be integral and
 * finite; it need not be a safe integer.
 */
export function formatJavaDouble(value: number): string {
  if (!Number.isInteger(value)) {
    throw invalid("an integral number");
  }
  if (Math.abs(value) < 10_000_000) {
    // Adding zero turns -0 into 0.
    return `${value + 0}.0`;
  }
  // Computerised scientific notation with the shortest digits that identify
  // the double.
  const scientific = value.toExponential();
  const e = scientific.indexOf("e");
  const mantissa = scientific.slice(0, e);
  // The exponent is positive here: drop its plus sign.
  const exponent = scientific.slice(e + 2);
  return `${mantissa}${mantissa.includes(".") ? "" : ".0"}E${exponent}`;
}

/**
 * A 64-bit integer as a number. Throws {@link JsonError} (`invalidValue`)
 * if it is not a safe integer.
 */
export function int64ToNumber(v: bigint): number {
  if (v < BigInt(Number.MIN_SAFE_INTEGER) || v > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw invalid(`a 64-bit integer within the safe integer range, not ${v}`);
  }
  return Number(v);
}

/**
 * A number as a 64-bit integer. Throws {@link JsonError} (`invalidValue`)
 * if it is not a safe integer.
 */
export function numberToInt64(n: number): bigint {
  if (!Number.isSafeInteger(n)) {
    throw invalid(`a safe integer, not ${n}`);
  }
  return BigInt(n);
}

/** The value if it is an integral JSON number within `[min, max]`. */
function integralIn(value: unknown, min: number, max: number): number | null {
  if (typeof value === "number" && Number.isInteger(value) && value >= min && value <= max) {
    return value + 0;
  }
  return null;
}

// Field kinds: how a value of each protobuf type is written and read.

/** The protobuf type of a field that is not a message. */
export type ScalarKind = "string" | "bool" | "int32" | "int52" | "int64" | "bytes";

/** The type of a field: a scalar kind or the codec of a nested message. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type FieldKind = ScalarKind | MessageCodec<any>;

export type FieldLabel = "required" | "optional" | "repeated";

/** One field of a message. */
export interface FieldSpec<K extends string = string> {
  /** The protobuf field number, which is the JSON key. */
  readonly number: number;
  /** The property of the TypeScript message type. */
  readonly name: K;
  /** The field's name in the schema, used in `missingField` errors. */
  readonly protoName: string;
  readonly label: FieldLabel;
  readonly kind: FieldKind;
}

const LONG_EXPECTED = "an array of two 32-bit integers [low, high]";

function writeScalar(kind: ScalarKind, value: unknown): string {
  switch (kind) {
    case "string":
      if (typeof value !== "string") {
        throw invalid("a string");
      }
      return writeString(value);
    case "bool":
      if (typeof value !== "boolean") {
        throw invalid("a boolean");
      }
      return value ? "true" : "false";
    case "int32": {
      const n = integralIn(value, INT32_MIN, INT32_MAX);
      if (n === null) {
        throw invalid("a 32-bit integer");
      }
      return String(n);
    }
    case "int52":
      if (typeof value !== "number" || !Number.isSafeInteger(value)) {
        throw invalid("a safe integer");
      }
      return formatJavaDouble(value);
    case "int64": {
      if (typeof value !== "bigint" || value < INT64_MIN || value > INT64_MAX) {
        throw invalid("a bigint within the signed 64-bit range");
      }
      const low = Number(BigInt.asIntN(32, value));
      const high = Number(BigInt.asIntN(32, value >> 32n));
      return `[${low},${high}]`;
    }
    case "bytes":
      if (!(value instanceof Uint8Array)) {
        throw invalid("a Uint8Array");
      }
      return `"${bytesToHex(value)}"`;
  }
}

function readScalar(kind: ScalarKind, value: unknown): unknown {
  switch (kind) {
    case "string":
      if (typeof value !== "string") {
        throw expected("a string");
      }
      if (!isWellFormed(value)) {
        throw new JsonError({ kind: "syntax", message: "unpaired surrogate in a string" });
      }
      return value;
    case "bool":
      if (typeof value !== "boolean") {
        throw expected("a boolean");
      }
      return value;
    case "int32": {
      const n = integralIn(value, INT32_MIN, INT32_MAX);
      if (n === null) {
        throw expected("a 32-bit integer");
      }
      return n;
    }
    case "int52": {
      if (typeof value !== "number" || !Number.isInteger(value)) {
        throw expected("an integral number");
      }
      if (!Number.isSafeInteger(value)) {
        throw expected("an integral number within the safe integer range");
      }
      return value + 0;
    }
    case "int64": {
      if (!Array.isArray(value) || value.length !== 2) {
        throw expected(LONG_EXPECTED);
      }
      const low = integralIn(value[0], INT32_MIN, 0xffff_ffff);
      const high = integralIn(value[1], INT32_MIN, INT32_MAX);
      if (low === null || high === null) {
        throw expected(LONG_EXPECTED);
      }
      return (BigInt(high) << 32n) | BigInt(low >>> 0);
    }
    case "bytes": {
      const bytes = typeof value === "string" ? tryHexToBytes(value) : null;
      if (bytes === null) {
        throw expected("a string of hexadecimal digits");
      }
      return bytes;
    }
  }
}

function writeValue(kind: FieldKind, value: unknown): string {
  return typeof kind === "string" ? writeScalar(kind, value) : kind.writeJson(value);
}

function readValue(kind: FieldKind, value: unknown): unknown {
  return typeof kind === "string" ? readScalar(kind, value) : kind.readJson(value);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * The JSON codec of one message type. `T` is the TypeScript message type:
 * required fields are plain properties, optional fields optional
 * properties (absent or `undefined`; `null` is also taken as absent when
 * writing), repeated fields arrays.
 */
export class MessageCodec<T> {
  /**
   * The message's simple name, which is the `messageType` of the RPC
   * envelope.
   */
  readonly name: string;
  /** The fields in declaration order. */
  readonly fields: readonly FieldSpec[];

  constructor(name: string, fields: readonly FieldSpec[]) {
    this.name = name;
    this.fields = fields;
  }

  /** The JSON text of `message`, after checking its required fields. */
  encode(message: T): string {
    this.checkInitialized(message);
    return this.writeJson(message);
  }

  /**
   * Reads a message from parsed JSON (see {@link parseJson}) and checks its
   * required fields.
   */
  decode(json: unknown): T {
    const message = this.readJson(json);
    this.checkInitialized(message);
    return message;
  }

  /** Reads a message from JSON text and checks its required fields. */
  parse(text: string): T {
    return this.decode(parseJson(text));
  }

  /**
   * Fails with `missingField` if a required field of the message or of a
   * message nested in it is not set.
   */
  checkInitialized(message: T): void {
    if (!isObject(message)) {
      throw invalid(`a message object (${this.name})`);
    }
    const record: Record<string, unknown> = message;
    for (const field of this.fields) {
      if (field.label === "required" && record[field.name] == null) {
        throw new JsonError({ kind: "missingField", field: `${this.name}.${field.protoName}` });
      }
    }
    for (const field of this.fields) {
      const value = record[field.name];
      if (field.label !== "repeated" && typeof field.kind !== "string" && value != null) {
        field.kind.checkInitialized(value);
      }
    }
    for (const field of this.fields) {
      const value = record[field.name];
      if (field.label === "repeated" && typeof field.kind !== "string" && Array.isArray(value)) {
        for (const element of value) {
          field.kind.checkInitialized(element);
        }
      }
    }
  }

  /** The JSON text of `message`. Does not check required fields. */
  writeJson(message: T): string {
    if (!isObject(message)) {
      throw invalid(`a message object (${this.name})`);
    }
    const record: Record<string, unknown> = message;
    let out = "{";
    let first = true;
    for (const field of this.fields) {
      const value = record[field.name];
      if (field.label !== "repeated" && value == null) {
        continue;
      }
      out += first ? `"${field.number}":` : `,"${field.number}":`;
      first = false;
      try {
        if (field.label === "repeated") {
          if (!Array.isArray(value)) {
            throw invalid("an array");
          }
          out += "[";
          for (let i = 0; i < value.length; i++) {
            if (i > 0) {
              out += ",";
            }
            out += writeValue(field.kind, value[i]);
          }
          out += "]";
        } else {
          out += writeValue(field.kind, value);
        }
      } catch (error) {
        throw within(error, String(field.number));
      }
    }
    return `${out}}`;
  }

  /**
   * Reads the fields present in parsed JSON. Does not check required
   * fields, so the result may lack properties `T` declares.
   */
  readJson(json: unknown): T {
    if (!isObject(json)) {
      throw expected(`an object (${this.name})`);
    }
    const message: Record<string, unknown> = {};
    for (const field of this.fields) {
      const key = String(field.number);
      const value = Object.hasOwn(json, key) ? json[key] : undefined;
      try {
        if (field.label === "repeated") {
          const elements: unknown[] = [];
          if (value != null) {
            if (!Array.isArray(value)) {
              throw expected("an array");
            }
            for (const element of value) {
              elements.push(readValue(field.kind, element));
            }
          }
          message[field.name] = elements;
        } else if (value != null) {
          message[field.name] = readValue(field.kind, value);
        }
      } catch (error) {
        throw within(error, key);
      }
    }
    return message as T;
  }
}

function camelToSnake(name: string): string {
  return name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

/**
 * A field of a message definition. `protoName` defaults to the snake_case
 * form of `name`.
 */
export function field<K extends string>(
  number: number,
  name: K,
  label: FieldLabel,
  kind: FieldKind,
  protoName: string = camelToSnake(name),
): FieldSpec<K> {
  return { number, name, protoName, label, kind };
}

/** Defines the codec of a message type; fields in declaration order. */
export function defineMessage<T>(
  name: string,
  fields: readonly FieldSpec<Extract<keyof T, string>>[],
): MessageCodec<T> {
  return new MessageCodec<T>(name, fields);
}

// Parsing.

/**
 * Parses JSON text. Throws {@link JsonError} (`syntax`) if the text is not
 * JSON or if any string or key in it holds an unpaired surrogate.
 */
export function parseJson(text: string): unknown {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new JsonError({
      kind: "syntax",
      message: error instanceof Error ? error.message : String(error),
    });
  }
  // Iterative, so that deep nesting cannot exhaust the stack.
  const pending: unknown[] = [value];
  while (pending.length > 0) {
    const item = pending.pop();
    if (typeof item === "string") {
      if (!isWellFormed(item)) {
        throw new JsonError({ kind: "syntax", message: "unpaired surrogate in a string" });
      }
    } else if (Array.isArray(item)) {
      for (const element of item) {
        if (typeof element === "string" || (typeof element === "object" && element !== null)) {
          pending.push(element);
        }
      }
    } else if (typeof item === "object" && item !== null) {
      for (const [key, element] of Object.entries(item)) {
        if (!isWellFormed(key)) {
          throw new JsonError({ kind: "syntax", message: "unpaired surrogate in a key" });
        }
        if (typeof element === "string" || (typeof element === "object" && element !== null)) {
          pending.push(element);
        }
      }
    }
  }
  return value;
}

// The RPC envelope of `WebSocketChannel.MessageWrapper`.

/**
 * A parsed RPC envelope: `{"messageType": ..., "sequenceNumber": ...,
 * "message": ...}`.
 */
export interface Envelope {
  readonly messageType: string;
  /** A signed 32-bit integer. */
  readonly sequenceNumber: number;
  /** The message body, still as parsed JSON; null if absent. */
  readonly message: unknown;
}

/**
 * The envelope text for a message: `messageType`, `sequenceNumber` and
 * `message`, in that order. Checks the message's required fields.
 */
export function encodeEnvelope<T>(
  sequenceNumber: number,
  codec: MessageCodec<T>,
  message: T,
): string {
  if (integralIn(sequenceNumber, INT32_MIN, INT32_MAX) === null) {
    throw within(invalid("a 32-bit integer"), "sequenceNumber");
  }
  let body: string;
  try {
    body = codec.encode(message);
  } catch (error) {
    throw within(error, "message");
  }
  return `{"messageType":${writeString(codec.name)},"sequenceNumber":${sequenceNumber + 0},"message":${body}}`;
}

/**
 * Parses an envelope. Fails on unpaired surrogates anywhere in the text.
 * The body is not interpreted: see {@link decodeBody}.
 */
export function parseEnvelope(text: string): Envelope {
  const value = parseJson(text);
  if (!isObject(value)) {
    throw expected("an envelope object");
  }
  const messageType = Object.hasOwn(value, "messageType") ? value["messageType"] : undefined;
  if (typeof messageType !== "string") {
    throw within(expected("a string"), "messageType");
  }
  const sequenceNumber = integralIn(
    Object.hasOwn(value, "sequenceNumber") ? value["sequenceNumber"] : undefined,
    INT32_MIN,
    INT32_MAX,
  );
  if (sequenceNumber === null) {
    throw within(expected("a 32-bit integer"), "sequenceNumber");
  }
  const message = Object.hasOwn(value, "message") ? (value["message"] ?? null) : null;
  return { messageType, sequenceNumber, message };
}

/**
 * The body of an envelope as a message of the codec's type, with required
 * fields checked. Does not compare the envelope's `messageType` with the
 * codec's name.
 */
export function decodeBody<T>(envelope: Envelope, codec: MessageCodec<T>): T {
  return codec.decode(envelope.message);
}
