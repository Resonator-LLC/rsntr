// Transformation of concurrent wavelet operations and deltas.
//
// Derived from `org.waveprotocol.wave.model.operation.wave.Transform`,
// `org.waveprotocol.wave.concurrencycontrol.common.DeltaPair` and
// `org.waveprotocol.box.server.waveserver.WaveletContainerImpl` of Apache
// Wave, licensed under the Apache License, Version 2.0.

import { TransformError, transform as transformDocOps } from "../docop";
import type { ParticipantId } from "../id";
import type { HashedVersion } from "../version";
import { type TransformedWaveletDelta, WaveletDelta } from "./delta";
import { SubmitTransformError, WaveTransformError } from "./error";
import { BlipOperation, WaveletOperation } from "./waveOp";

/**
 * A client operation and a server operation, each transformed against the
 * other.
 */
export interface OperationPair<O> {
  readonly clientOp: O;
  readonly serverOp: O;
}

function sameParticipant(
  op: WaveletOperation,
  type: "addParticipant" | "removeParticipant",
  participant: ParticipantId,
): boolean {
  return op.kind.type === type && op.kind.participant.equals(participant);
}

/**
 * Transforms a pair of concurrent wavelet operations (`Transform.transform`).
 *
 * Operations on different documents, and all pairs not mentioned below,
 * transform to themselves. Content operations on the same document are
 * transformed as document operations. The same participant added by both,
 * or removed by both, turns both operations into no-ops. Contexts stay
 * with their sides.
 *
 * Throws {@link WaveTransformError}: `"removedAuthor"` if the server
 * operation removes the creator of the client operation, `"participant"` if
 * one side adds the participant the other removes, `"document"` if the
 * document operations do not transform. The first check is skipped when
 * both operations are blip operations, as in the original.
 */
export function transform(
  clientOp: WaveletOperation,
  serverOp: WaveletOperation,
): OperationPair<WaveletOperation> {
  const identity: OperationPair<WaveletOperation> = { clientOp, serverOp };
  const noOps = (): OperationPair<WaveletOperation> => ({
    clientOp: WaveletOperation.noOp(clientOp.context),
    serverOp: WaveletOperation.noOp(serverOp.context),
  });
  const involving = (participant: ParticipantId): never => {
    throw new WaveTransformError({ kind: "participant", participant: participant.address });
  };
  const client = clientOp.kind;
  const server = serverOp.kind;
  if (client.type === "blip" && server.type === "blip") {
    if (client.blipId !== server.blipId) {
      // Different blips don't conflict.
      return identity;
    }
    const pair = transformBlipOps(client.op, server.op);
    return {
      clientOp: WaveletOperation.blip(clientOp.context, client.blipId, pair.clientOp),
      serverOp: WaveletOperation.blip(serverOp.context, server.blipId, pair.serverOp),
    };
  }
  if (server.type === "removeParticipant") {
    const participant = server.participant;
    if (participant.equals(clientOp.context.creator)) {
      throw new WaveTransformError({ kind: "removedAuthor", author: participant.address });
    }
    if (sameParticipant(clientOp, "removeParticipant", participant)) {
      return noOps();
    }
    if (sameParticipant(clientOp, "addParticipant", participant)) {
      return involving(participant);
    }
    return identity;
  }
  if (server.type === "addParticipant") {
    const participant = server.participant;
    if (sameParticipant(clientOp, "addParticipant", participant)) {
      return noOps();
    }
    if (sameParticipant(clientOp, "removeParticipant", participant)) {
      return involving(participant);
    }
    return identity;
  }
  return identity;
}

/**
 * Transforms a pair of concurrent operations on the same document. Two
 * content operations are transformed as document operations, and (as in the
 * original) both results get the default contributor method; any other pair
 * transforms to itself.
 */
export function transformBlipOps(
  clientOp: BlipOperation,
  serverOp: BlipOperation,
): OperationPair<BlipOperation> {
  const client = clientOp.kind;
  const server = serverOp.kind;
  if (client.type === "content" && server.type === "content") {
    try {
      const pair = transformDocOps(client.op, server.op);
      return {
        clientOp: BlipOperation.content(pair.clientOp),
        serverOp: BlipOperation.content(pair.serverOp),
      };
    } catch (e) {
      if (e instanceof TransformError) {
        throw new WaveTransformError({ kind: "document", error: e });
      }
      throw e;
    }
  }
  return { clientOp, serverOp };
}

/**
 * A client delta and a server delta that apply at the same version
 * (`DeltaPair`). Immutable.
 */
export class DeltaPair {
  readonly client: readonly WaveletOperation[];
  readonly server: readonly WaveletOperation[];

  constructor(client: readonly WaveletOperation[], server: readonly WaveletOperation[]) {
    this.client = [...client];
    this.server = [...server];
  }

  /**
   * Transforms each delta against the other, so that applying the client
   * delta and then the transformed server delta gives the same state as
   * the server delta and then the transformed client delta.
   *
   * Identical deltas (same operations by the same creators) nullify each
   * other: the client delta becomes empty and each server operation
   * becomes a version update.
   */
  transform(): DeltaPair {
    if (DeltaPair.areSame(this.client, this.server)) {
      return new DeltaPair(
        [],
        this.server.map((op) =>
          op.createVersionUpdateOp(op.context.versionIncrement, op.context.hashedVersion),
        ),
      );
    }
    const [client, server] = transformOpLists(this.client, this.server);
    return new DeltaPair(client, server);
  }

  /** Whether two deltas have equal operations by the same creators. */
  static areSame(a: Iterable<WaveletOperation>, b: Iterable<WaveletOperation>): boolean {
    const as = [...a];
    const bs = [...b];
    return (
      as.length === bs.length &&
      as.every((op, i) => {
        const other = bs[i]!;
        return op.context.creator.equals(other.context.creator) && op.equals(other);
      })
    );
  }
}

/**
 * Transforms two operation lists that apply at the same state against each
 * other, returning the transformed client and server lists. Each server
 * operation is carried through the client operations in turn, so every
 * pairwise transform is between operations on the same state.
 */
export function transformOpLists(
  clientOps: Iterable<WaveletOperation>,
  serverOps: Iterable<WaveletOperation>,
): [client: WaveletOperation[], server: WaveletOperation[]] {
  const outClient = [...clientOps];
  const outServer: WaveletOperation[] = [];
  for (const serverOp of serverOps) {
    let newServerOp = serverOp;
    for (let i = 0; i < outClient.length; i++) {
      const pair = transform(outClient[i]!, newServerOp);
      outClient[i] = pair.clientOp;
      newServerOp = pair.serverOp;
    }
    outServer.push(newServerOp);
  }
  return [outClient, outServer];
}

/**
 * Transforms client operations against server operations the way the
 * server's `WaveletContainerImpl.transformOps` does.
 *
 * Quirk kept from the original: each client operation is transformed
 * against the server operations as given, not against the server operations
 * transformed by the preceding client operations. For a client delta of
 * several operations this differs from {@link transformOpLists} when an
 * earlier client operation affects how the server operations apply to a
 * later one; the later one is then transformed against a server operation
 * for a different document state, which fails or gives a wrong result.
 */
export function transformOps(
  clientOps: Iterable<WaveletOperation>,
  serverOps: Iterable<WaveletOperation>,
): WaveletOperation[] {
  const server = [...serverOps];
  return [...clientOps].map((clientOp) => {
    let c = clientOp;
    for (const s of server) {
      c = transform(c, s).clientOp;
    }
    return c;
  });
}

/**
 * Transforms a submitted delta to apply at `currentVersion`
 * (`WaveletContainerImpl.maybeTransformSubmittedDelta`).
 *
 * `serverDeltas` must be the history from the delta's target version to
 * `currentVersion`; it is not looked at when the delta already targets the
 * current version.
 *
 * If the delta transforms away, or duplicates a server delta by the same
 * author, it is returned with the target version at which that happened
 * instead of the current version, so the caller can treat resubmission as
 * idempotent: a result whose target version is not `currentVersion` must
 * not be applied.
 *
 * Throws {@link SubmitTransformError}.
 */
export function transformSubmittedDelta(
  delta: WaveletDelta,
  currentVersion: HashedVersion,
  serverDeltas: readonly TransformedWaveletDelta[],
): WaveletDelta {
  let targetVersion = delta.targetVersion;
  if (targetVersion.equals(currentVersion)) {
    // Submitted against head.
    return delta;
  }
  if (targetVersion.version === currentVersion.version) {
    throw new SubmitTransformError({
      kind: "invalidHash",
      expected: currentVersion,
      target: targetVersion,
    });
  }
  const historyError = (target: HashedVersion): SubmitTransformError =>
    new SubmitTransformError({ kind: "history", target, current: currentVersion });
  if (serverDeltas.length === 0) {
    throw historyError(targetVersion);
  }
  const author = delta.author;
  let clientOps: WaveletOperation[] = [...delta.ops];
  for (const serverDelta of serverDeltas) {
    const duplicate =
      author.equals(serverDelta.author) &&
      clientOps.length === serverDelta.ops.length &&
      clientOps.every((op, i) => op.equals(serverDelta.ops[i]!));
    if (clientOps.length === 0 || duplicate) {
      return new WaveletDelta(author, targetVersion, clientOps);
    }
    // Deliberate fix: Java's `transformOps` transforms each client op
    // against the untransformed server ops, which rejects valid multi-op
    // deltas. `transformOps` keeps the original behaviour.
    try {
      clientOps = transformOpLists(clientOps, serverDelta.ops)[0];
    } catch (e) {
      if (e instanceof WaveTransformError) {
        throw new SubmitTransformError({ kind: "transform", error: e });
      }
      throw e;
    }
    targetVersion = serverDelta.resultingVersion;
  }
  if (!targetVersion.equals(currentVersion)) {
    throw historyError(targetVersion);
  }
  return new WaveletDelta(author, targetVersion, clientOps);
}
