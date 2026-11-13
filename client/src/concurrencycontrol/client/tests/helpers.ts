// Shared helpers for the ported concurrency control tests: small stand-ins
// for `DeltaTestUtil` and `DocProviders.POJO` of the Apache Wave test tree
// (Apache License, Version 2.0).

import { expect } from "vitest";

import {
  Attributes,
  type DocInitialization,
  type DocOp,
  DocOpBuilder,
  composeInitialization,
  resultingDocumentLength,
} from "../../../model/docop";
import { ParticipantId } from "../../../model/id";
import {
  TransformedWaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
} from "../../../model/operation";
import { HashedVersion } from "../../../model/version";

/** Predictable signature at the given version. */
export function genSignature(version: number): HashedVersion {
  return HashedVersion.of(version, new Uint8Array([version & 0xff]));
}

/** Asserts element-wise equality of two operation lists, contexts included. */
export function expectOpsEqual(
  actual: Iterable<WaveletOperation>,
  expected: Iterable<WaveletOperation>,
): void {
  const a = [...actual];
  const e = [...expected];
  expect(a.length).toBe(e.length);
  a.forEach((op, i) => {
    const other = e[i] as WaveletOperation;
    expect(op.equals(other), `op ${i}: ${op} vs ${other}`).toBe(true);
    // Stricter than the original, whose `equals` ignores the context.
    expect(op.context.equals(other.context), `context ${i}: ${op} vs ${other}`).toBe(true);
  });
}

/** Runs `f` and returns what it throws; fails if it does not throw. */
export function thrownBy(f: () => unknown): unknown {
  try {
    f();
  } catch (e) {
    return e;
  }
  throw new Error("expected the call to throw");
}

/**
 * A document that applies operations, standing in for the `SuperSink` the
 * original tests get from `DocProviders.POJO.parse`.
 */
export class TestDoc {
  private content: DocInitialization;

  private constructor(content: DocInitialization) {
    this.content = content;
  }

  /** Parses XML made of attribute-less elements and plain text. */
  static parse(xml: string): TestDoc {
    const builder = new DocOpBuilder();
    const token = /<(\/?)([A-Za-z][A-Za-z0-9]*)>|([^<]+)/g;
    let consumed = 0;
    for (let m = token.exec(xml); m !== null; m = token.exec(xml)) {
      if (m.index !== consumed) {
        break;
      }
      consumed = token.lastIndex;
      if (m[3] !== undefined) {
        builder.characters(m[3]);
      } else if (m[1] === "/") {
        builder.elementEnd();
      } else {
        builder.elementStart(m[2] as string, Attributes.EMPTY);
      }
    }
    if (consumed !== xml.length) {
      throw new Error(`Unsupported test XML: ${xml}`);
    }
    return new TestDoc(builder.buildInitialization());
  }

  /**
   * Inserts characters and returns the operation that did it
   * (`consumeAndReturnInvertible(Nindo.insertCharacters(offset, chars))`).
   */
  insertCharacters(offset: number, chars: string): DocOp {
    const length = resultingDocumentLength(this.content);
    const builder = new DocOpBuilder();
    if (offset > 0) {
      builder.retain(offset);
    }
    builder.characters(chars);
    if (length - offset > 0) {
      builder.retain(length - offset);
    }
    const op = builder.build();
    this.consume(op);
    return op;
  }

  /** Applies an operation; throws the docop `OperationError` if it does not apply. */
  consume(op: DocOp): void {
    this.content = composeInitialization(this.content, op);
  }

  asOperation(): DocInitialization {
    return this.content;
  }
}

/** Makes operations and deltas by one author (`DeltaTestUtil`). */
export class DeltaTestUtil {
  readonly author: ParticipantId;

  constructor(author: string | ParticipantId) {
    this.author = typeof author === "string" ? ParticipantId.newUnchecked(author) : author;
  }

  getAuthor(): ParticipantId {
    return this.author;
  }

  private context(resultingVersion: HashedVersion | null = null): WaveletOperationContext {
    return new WaveletOperationContext(this.author, 0, 1, resultingVersion);
  }

  /** A deletion of characters in the document "blip id". */
  delete(posStart: number, characters: string, remaining: number): WaveletOperation {
    const op = new DocOpBuilder()
      .retain(posStart)
      .deleteCharacters(characters)
      .retain(remaining)
      .build();
    return WaveletOperation.blipContent(this.context(), "blip id", op);
  }

  /** Wraps an op with a delta. */
  delta(targetVersion: number, op: WaveletOperation): TransformedWaveletDelta {
    return TransformedWaveletDelta.cloneOperations(
      this.author,
      HashedVersion.unsigned(targetVersion + 1),
      0,
      [op],
    );
  }

  noOp(): WaveletOperation {
    return WaveletOperation.noOp(this.context());
  }

  addParticipant(participant: ParticipantId): WaveletOperation {
    return WaveletOperation.addParticipant(this.context(), participant);
  }

  removeParticipant(participant: ParticipantId): WaveletOperation {
    return WaveletOperation.removeParticipant(this.context(), participant);
  }

  /**
   * A docop that is empty, i.e. does nothing to the document, which must
   * also be empty.
   */
  noOpDocOp(blipId: string): WaveletOperation {
    return WaveletOperation.blipContent(this.context(), blipId, new DocOpBuilder().build());
  }

  /** An insertion of characters in the document "blip id". */
  insert(
    pos: number,
    text: string,
    remaining: number,
    resultingVersion: HashedVersion | null,
  ): WaveletOperation {
    const builder = new DocOpBuilder().retain(pos).characters(text);
    if (remaining > 0) {
      builder.retain(remaining);
    }
    return WaveletOperation.blipContent(this.context(resultingVersion), "blip id", builder.build());
  }
}
