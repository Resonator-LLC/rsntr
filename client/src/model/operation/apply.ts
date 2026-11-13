// Application of wavelet operations to wavelet data.
//
// Derived from `org.waveprotocol.wave.model.operation.wave` of Apache Wave,
// licensed under the Apache License, Version 2.0.

import { type BlipData, DataError, type WaveletData } from "../data";
import { DocInitialization, DocOp, OperationError, invert } from "../docop";
import { WaveletOperationContext } from "./context";
import { WaveletOperationError } from "./error";
import { BlipOperation, type UpdateContributorMethod, WaveletOperation } from "./waveOp";

function addVersion(version: number, increment: number): number {
  const result = version + increment;
  if (!Number.isSafeInteger(result) || result < 0) {
    throw new WaveletOperationError({ kind: "versionOutOfRange", version, increment });
  }
  return result;
}

/** Runs `f`, wrapping document and data errors in a {@link WaveletOperationError}. */
function wrapping<T>(f: () => T): T {
  try {
    return f();
  } catch (e) {
    if (e instanceof OperationError) {
      throw new WaveletOperationError({ kind: "document", error: e });
    }
    if (e instanceof DataError) {
      throw new WaveletOperationError({ kind: "data", error: e });
    }
    throw e;
  }
}

function missingDocument(docId: string): WaveletOperationError {
  return new WaveletOperationError({ kind: "missingDocument", docId });
}

function missingParticipant(participant: string): WaveletOperationError {
  return new WaveletOperationError({ kind: "missingParticipant", participant });
}

/**
 * `BlipOperation.update`: updates contributors and last-modified metadata of
 * a blip, if the operation is worthy. Returns the method that reverses the
 * contributor change.
 */
function updateBlip(
  blip: BlipData,
  blipOp: BlipOperation,
  method: UpdateContributorMethod,
  context: WaveletOperationContext,
  newWaveletVersion: number,
): UpdateContributorMethod {
  if (!blipOp.updatesBlipMetadata(blip.id)) {
    return "none";
  }
  const creator = context.creator;
  const present = blip.hasContributor(creator);
  let reverse: UpdateContributorMethod = "none";
  if (method === "add" && !present) {
    blip.addContributor(creator);
    reverse = "remove";
  } else if (method === "remove" && present) {
    blip.removeContributor(creator);
    reverse = "add";
  }
  blip.setLastModifiedVersion(newWaveletVersion);
  if (context.hasTimestamp()) {
    blip.setLastModifiedTime(context.timestamp);
  }
  return reverse;
}

/**
 * The document a blip operation targets, created empty if absent with the
 * operation's creator as author and only contributor.
 */
function targetBlip(
  op: WaveletOperation,
  target: WaveletData,
  blipId: string,
  newVersion: number,
): BlipData {
  const existing = target.document(blipId);
  if (existing !== null) {
    return existing;
  }
  const creator = op.context.creator;
  return wrapping(() =>
    target.createDocument(
      blipId,
      creator,
      [creator],
      DocInitialization.fromOp(DocOp.EMPTY),
      op.context.timestamp,
      newVersion,
    ),
  );
}

/**
 * `WaveletOperation.update`: moves the wavelet's timestamp, version and
 * hashed version as the context says.
 */
function update(op: WaveletOperation, target: WaveletData, newVersion: number): void {
  const context = op.context;
  if (context.hasTimestamp()) {
    target.setLastModifiedTime(context.timestamp);
  }
  target.setVersion(newVersion);
  if (context.hashedVersion !== null) {
    target.setHashedVersion(context.hashedVersion);
  }
}

function reverseContext(op: WaveletOperation, target: WaveletData): WaveletOperationContext {
  return WaveletOperationContext.withHashedVersion(
    op.context.creator,
    target.lastModifiedTime,
    -op.context.versionIncrement,
    target.hashedVersion,
  );
}

/** Implements {@link WaveletOperation.apply}. */
export function applyOperation(op: WaveletOperation, target: WaveletData): void {
  const context = op.context;
  const newVersion = addVersion(target.version, context.versionIncrement);
  const kind = op.kind;
  switch (kind.type) {
    case "noOp":
      break;
    case "addParticipant": {
      const { participant, position } = kind;
      const added =
        position === null
          ? target.addParticipant(participant)
          : wrapping(() => target.addParticipantAt(participant, position));
      if (!added) {
        throw new WaveletOperationError({
          kind: "duplicateParticipant",
          participant: participant.toString(),
        });
      }
      break;
    }
    case "removeParticipant":
      if (!target.removeParticipant(kind.participant)) {
        throw missingParticipant(kind.participant.toString());
      }
      break;
    case "blip": {
      const blip = targetBlip(op, target, kind.blipId, newVersion);
      const blipKind = kind.op.kind;
      if (blipKind.type === "content") {
        wrapping(() => blip.content.consume(blipKind.op));
        updateBlip(blip, kind.op, blipKind.method, context, newVersion);
      }
      break;
    }
    case "versionUpdate":
      if (kind.docId !== null) {
        const blip = target.document(kind.docId);
        if (blip === null) {
          throw missingDocument(kind.docId);
        }
        blip.setLastModifiedVersion(kind.fixedDocVersion ?? newVersion);
      }
      break;
  }
  update(op, target, newVersion);
}

/** Implements {@link WaveletOperation.applyAndReturnReverse}. */
export function applyOperationAndReturnReverse(
  op: WaveletOperation,
  target: WaveletData,
): WaveletOperation[] {
  const context = op.context;
  const increment = context.versionIncrement;
  const newVersion = addVersion(target.version, increment);
  const kind = op.kind;
  let reverse: WaveletOperation;
  switch (kind.type) {
    case "noOp": {
      reverse = WaveletOperation.noOp(reverseContext(op, target));
      applyOperation(op, target);
      return [reverse];
    }
    case "addParticipant": {
      reverse = WaveletOperation.removeParticipant(reverseContext(op, target), kind.participant);
      applyOperation(op, target);
      return [reverse];
    }
    case "removeParticipant": {
      const position = target.indexOfParticipant(kind.participant);
      if (position < 0) {
        throw missingParticipant(kind.participant.toString());
      }
      reverse = WaveletOperation.addParticipant(
        reverseContext(op, target),
        kind.participant,
        position,
      );
      applyOperation(op, target);
      return [reverse];
    }
    case "blip": {
      const blip = targetBlip(op, target, kind.blipId, newVersion);
      // No hashed version in the reverse context of a blip op.
      const context2 = new WaveletOperationContext(
        context.creator,
        blip.lastModifiedTime,
        -increment,
      );
      const blipKind = kind.op.kind;
      let reverseOp: BlipOperation;
      if (blipKind.type === "content") {
        // Metadata first, then content.
        const reverseMethod = updateBlip(blip, kind.op, blipKind.method, context, newVersion);
        reverseOp = wrapping(() => {
          blip.content.consume(blipKind.op);
          return BlipOperation.content(invert(blipKind.op), reverseMethod);
        });
      } else {
        reverseOp = BlipOperation.SUBMIT;
      }
      reverse = WaveletOperation.blip(context2, kind.blipId, reverseOp);
      break;
    }
    case "versionUpdate": {
      const oldHashedVersion = target.hashedVersion;
      let oldDocVersion: number | null = null;
      if (kind.docId !== null) {
        const blip = target.document(kind.docId);
        if (blip === null) {
          throw missingDocument(kind.docId);
        }
        oldDocVersion = blip.setLastModifiedVersion(kind.fixedDocVersion ?? newVersion);
      }
      reverse = WaveletOperation.versionUpdate(
        context.creator,
        -increment,
        oldHashedVersion,
        kind.docId,
        oldDocVersion,
      );
      break;
    }
  }
  update(op, target, newVersion);
  return [reverse];
}
