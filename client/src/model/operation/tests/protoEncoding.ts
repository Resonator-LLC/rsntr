// A minimal protobuf encoder for the messages that carry wavelet operations
// and deltas (`ProtocolWaveletOperation`, `ProtocolWaveletDelta` of
// federation.protodevel). Test support only: it lets the golden test compare
// operations with the bytes written by the original implementation. The
// real wire code lives elsewhere.

import type { Attributes, DocOp, DocOpComponent } from "../../docop";
import type { ParticipantId } from "../../id";
import type { HashedVersion } from "../../version";
import type { WaveletOperation } from "..";

class Writer {
  private readonly out: number[] = [];

  private varint(value: number): void {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new Error(`cannot encode ${value}`);
    }
    let v = value;
    while (v >= 0x80) {
      this.out.push((v % 0x80) | 0x80);
      v = Math.floor(v / 0x80);
    }
    this.out.push(v);
  }

  int(field: number, value: number): this {
    this.varint(field * 8);
    this.varint(value);
    return this;
  }

  bool(field: number, value: boolean): this {
    return this.int(field, value ? 1 : 0);
  }

  bytes(field: number, value: Uint8Array | Writer): this {
    const bytes = value instanceof Writer ? value.out : value;
    this.varint(field * 8 + 2);
    this.varint(bytes.length);
    for (const b of bytes) {
      this.out.push(b);
    }
    return this;
  }

  string(field: number, value: string): this {
    return this.bytes(field, new TextEncoder().encode(value));
  }

  finish(): Uint8Array {
    return Uint8Array.from(this.out);
  }
}

function keyValuePairs(w: Writer, field: number, attributes: Attributes): void {
  for (const a of attributes.entries) {
    w.bytes(field, new Writer().string(1, a.name).string(2, a.value));
  }
}

function keyValueUpdate(key: string, oldValue: string | null, newValue: string | null): Writer {
  const w = new Writer().string(1, key);
  if (oldValue !== null) {
    w.string(2, oldValue);
  }
  if (newValue !== null) {
    w.string(3, newValue);
  }
  return w;
}

function elementStart(tag: string, attributes: Attributes): Writer {
  const w = new Writer().string(1, tag);
  keyValuePairs(w, 2, attributes);
  return w;
}

function component(c: DocOpComponent): Writer {
  const w = new Writer();
  switch (c.type) {
    case "annotationBoundary": {
      const a = new Writer();
      if (c.boundary.isEmpty()) {
        a.bool(1, true);
      } else {
        for (const key of c.boundary.endKeys) {
          a.string(2, key);
        }
        for (const change of c.boundary.changes) {
          a.bytes(3, keyValueUpdate(change.key, change.oldValue, change.newValue));
        }
      }
      return w.bytes(1, a);
    }
    case "characters":
      return w.string(2, c.characters);
    case "elementStart":
      return w.bytes(3, elementStart(c.tag, c.attributes));
    case "elementEnd":
      return w.bool(4, true);
    case "retain":
      return w.int(5, c.itemCount);
    case "deleteCharacters":
      return w.string(6, c.characters);
    case "deleteElementStart":
      return w.bytes(7, elementStart(c.tag, c.attributes));
    case "deleteElementEnd":
      return w.bool(8, true);
    case "replaceAttributes": {
      const r = new Writer();
      if (c.oldAttributes.isEmpty() && c.newAttributes.isEmpty()) {
        r.bool(1, true);
      } else {
        keyValuePairs(r, 2, c.oldAttributes);
        keyValuePairs(r, 3, c.newAttributes);
      }
      return w.bytes(9, r);
    }
    case "updateAttributes": {
      const u = new Writer();
      if (c.update.isEmpty()) {
        u.bool(1, true);
      } else {
        for (const update of c.update.updates) {
          u.bytes(2, keyValueUpdate(update.name, update.oldValue, update.newValue));
        }
      }
      return w.bytes(10, u);
    }
  }
}

function docOp(op: DocOp): Writer {
  const w = new Writer();
  for (const c of op.components) {
    w.bytes(1, component(c));
  }
  return w;
}

function operation(op: WaveletOperation): Writer {
  const w = new Writer();
  const kind = op.kind;
  switch (kind.type) {
    case "noOp":
      return w.bool(4, true);
    case "addParticipant":
      return w.string(1, kind.participant.address);
    case "removeParticipant":
      return w.string(2, kind.participant.address);
    case "blip":
      if (kind.op.kind.type === "content") {
        return w.bytes(3, new Writer().string(1, kind.blipId).bytes(2, docOp(kind.op.kind.op)));
      }
      break;
    case "versionUpdate":
      break;
  }
  throw new Error(`unsupported operation: ${op}`);
}

/** The bytes of the `ProtocolWaveletOperation` for `op`. */
export function encodeOperation(op: WaveletOperation): Uint8Array {
  return operation(op).finish();
}

/** The bytes of a `ProtocolWaveletDelta`. */
export function encodeDelta(
  author: ParticipantId,
  version: HashedVersion,
  ops: readonly WaveletOperation[],
): Uint8Array {
  const w = new Writer()
    .bytes(1, new Writer().int(1, version.version).bytes(2, version.historyHash))
    .string(2, author.address);
  for (const op of ops) {
    w.bytes(3, operation(op));
  }
  return w.finish();
}

export function toHex(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) {
    s += b.toString(16).padStart(2, "0");
  }
  return s;
}

export function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  }
  return out;
}
