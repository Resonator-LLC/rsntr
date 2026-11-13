// Tests OperationChannelImpl by mocking the underlying WaveletDeltaChannel.
// These are really tests for the channel + concurrency control combination.
//
// Ported from `OperationChannelImplTest` of Apache Wave (Apache License,
// Version 2.0). The large test that is commented out in the original is not
// ported.

import { afterEach, describe, expect, it } from "vitest";

import { ParticipantId } from "../../../model/id";
import { TransformedWaveletDelta, type WaveletDelta } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { ConcurrencyControl } from "../../client";
import { ResponseCode } from "../../common";
import { Accessibility, OperationChannelImpl } from "..";
import { DeltaTestUtil, bytes, expectChannelException } from "./helpers";
import { MockOperationChannelListener } from "./mockOperationChannelListener";
import { MockWaveletDeltaChannel } from "./mockWaveletDeltaChannel";

const USER_ID = ParticipantId.ofUnsafe("test@example.com");
const UTIL = new DeltaTestUtil(USER_ID);
const SIG1 = bytes(1, 1, 1, 1);
const SIG2 = bytes(2, 2, 2, 2);
const SIG3 = bytes(3, 3, 3, 3);
const SIG4 = bytes(4, 4, 4, 4);
const SIG5 = bytes(5, 5, 5, 5);

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

describe("OperationChannelImpl", () => {
  let deltaChannel: MockWaveletDeltaChannel;
  let cc: ConcurrencyControl;
  let operationChannel: OperationChannelImpl;
  const listener = new MockOperationChannelListener();

  afterEach(() => {
    listener.clear();
  });

  ///////////////////// UTILITY METHODS FOR TESTS //////////////////////

  /**
   * Returns a delta with the requested number of ops, version number and
   * signature.
   */
  function createRandomTransformedDelta(
    targetVersion: number,
    opCount: number,
    hash: Uint8Array,
  ): TransformedWaveletDelta {
    return UTIL.makeTransformedDelta(0, HashedVersion.of(targetVersion + opCount, hash), opCount);
  }

  /** Connects the channel at the provided version. */
  function connectChannel(version: number, signature: Uint8Array): void {
    const signatureInfo = HashedVersion.of(version, signature);
    deltaChannel = new MockWaveletDeltaChannel();
    cc = new ConcurrencyControl(null, signatureInfo);
    operationChannel = new OperationChannelImpl(null, deltaChannel, cc, Accessibility.READ_WRITE);
    operationChannel.setListener(listener);
    operationChannel.onConnection(signatureInfo, signatureInfo);
  }

  /**
   * Fails and reconnects the channel at the provided connection version and
   * current version (by default the connection version). The channel is
   * expected to provide a distinct version matching the connect version. If
   * expectRetransmission is not null expects the channel to attempt to send
   * that message on reconnection.
   */
  function reconnectChannel(
    connectVersion: number,
    connectSignature: Uint8Array,
    currentVersion: number = connectVersion,
    currentSignature: Uint8Array = connectSignature,
    expectRetransmission: WaveletDelta | null = null,
  ): void {
    const connectHashedVersion = HashedVersion.of(connectVersion, connectSignature);
    const currentHashedVersion = HashedVersion.of(currentVersion, currentSignature);

    // Simulate failure elsewhere.
    operationChannel.reset();

    // Check reconnect versions provided by the channel include the
    // version we'll reconnect at.
    const reconnectVersions = operationChannel.getReconnectVersions();
    expect(reconnectVersions.length > 0).toBe(true);
    let matchedSignature = false;
    for (const rcv of reconnectVersions) {
      if (connectVersion === rcv.version && sameBytes(connectSignature, rcv.historyHash)) {
        matchedSignature = true;
      }
    }
    expect(matchedSignature, "No matching signature provided").toBe(true);

    // Simulate reconnection message from delta channel.
    if (expectRetransmission !== null) {
      deltaChannel.expectSend(expectRetransmission);
    }
    operationChannel.onConnection(connectHashedVersion, currentHashedVersion);
  }

  function sendAndCheckRandomOp(
    opChannel: OperationChannelImpl,
    currentVersion: number,
    signature: Uint8Array,
  ): WaveletDelta {
    const delta = UTIL.makeDelta(HashedVersion.of(currentVersion, signature), 0, 1);
    deltaChannel.expectSend(delta);
    opChannel.send(delta.ops[0]!);
    return delta;
  }

  function checkExpectationsSatisfied(): void {
    deltaChannel.checkExpectationsSatisfied();
  }

  ///////////////////// TESTS //////////////////////

  /** Test a simple connect. */
  it("connectSuccess", () => {
    connectChannel(10, SIG1);
    listener.checkOpsReceived(0);
    checkExpectationsSatisfied();
  });

  /** Test a simple connect then successful disconnection. */
  it("connectThenDisconnectSuccess", () => {
    connectChannel(10, SIG1);
    listener.checkOpsReceived(0);

    // Disconnect
    operationChannel.reset();
    checkExpectationsSatisfied();
  });

  /** Test simple reconnect. */
  it("simpleReconnect", () => {
    connectChannel(10, SIG1);
    listener.checkOpsReceived(0);

    // Now disconnect server-side, forcing a reconnect.
    reconnectChannel(10, SIG1);

    // No new messages.
    listener.checkOpsReceived(0);
    checkExpectationsSatisfied();
  });

  /** Test receiving a simple delta that was committed. */
  it("simpleReceive", () => {
    const initialVersion = 42;
    connectChannel(initialVersion, SIG1);

    operationChannel.onDelta(createRandomTransformedDelta(initialVersion, 1, SIG2));
    listener.checkOpsReceived(1);
    listener.clear();

    expect(operationChannel.receive()).not.toBeNull();
    listener.checkOpsReceived(0);

    operationChannel.onCommit(initialVersion + 1);
    checkExpectationsSatisfied();
  });

  it("simpleSendAndAck", () => {
    const initialVersion = 42;
    connectChannel(initialVersion, SIG1);

    sendAndCheckRandomOp(operationChannel, initialVersion, SIG1);
    listener.clear();

    const signature = HashedVersion.of(43, SIG2);
    operationChannel.onAck(1, signature);
    // Listener should receive a version update op.
    listener.checkOpsReceived(1);
    const op = operationChannel.receive();
    expect(op).not.toBeNull();
    const context = op!.context;
    expect(context.versionIncrement).toBe(1);
    expect(context.hashedVersion !== null && context.hashedVersion.equals(signature)).toBe(true);
    checkExpectationsSatisfied();
  });

  /**
   * Test simple NACK and onDelta.
   * Nack is not supposed to happen, so it should be death.
   */
  it("simpleNackAndReceive", () => {
    const initialVersion = 42;
    connectChannel(initialVersion, SIG1);

    sendAndCheckRandomOp(operationChannel, initialVersion, SIG1);
    listener.clear();

    expectChannelException(
      () => operationChannel.onNack(ResponseCode.OK, null, 43),
      "Should have thrown ChannelException",
    );
    checkExpectationsSatisfied();
  });

  it("sendToInaccessibleChanneFails", () => {
    const connectSig = HashedVersion.unsigned(0);
    deltaChannel = new MockWaveletDeltaChannel();
    cc = new ConcurrencyControl(null, connectSig);
    operationChannel = new OperationChannelImpl(null, deltaChannel, cc, Accessibility.READ_ONLY);
    operationChannel.setListener(listener);
    operationChannel.onConnection(connectSig, connectSig);

    const e = expectChannelException(
      () => sendAndCheckRandomOp(operationChannel, connectSig.version, connectSig.historyHash),
      "Expected a channel exception",
    );
    expect(e.responseCode).toBe(ResponseCode.NOT_AUTHORIZED);
  });

  it("reconnectAfterCommit", () => {
    const initialVersion = 42;
    connectChannel(initialVersion, SIG1);

    sendAndCheckRandomOp(operationChannel, 42, SIG1);
    operationChannel.onAck(1, HashedVersion.of(43, SIG2));

    // Take the fake op resulting from ack that updates version info.
    listener.checkOpsReceived(1);
    expect(operationChannel.receive()).not.toBeNull();
    listener.clear();

    operationChannel.onCommit(43); // Causes cc to remove old operations from memory

    reconnectChannel(43, SIG2);

    // Receive next delta.
    const delta = createRandomTransformedDelta(43, 1, SIG3);
    operationChannel.onDelta(delta);
    listener.checkOpsReceived(1);
    expect(operationChannel.receive()).not.toBeNull();
    expect(operationChannel.receive()).toBeNull();
    checkExpectationsSatisfied();
  });

  /**
   * Test reconnect where there was an unACKed delta during the failure
   * which was not received by the server, expect the client to immediately
   * resend the delta.
   */
  it("reconnectWithPendingAckNotRecievedByServer", () => {
    const initialVersion = 42;
    const ackSignature = SIG2;
    connectChannel(initialVersion, SIG1);

    const delta = sendAndCheckRandomOp(operationChannel, 42, SIG1);

    // The server's version is still 42 when it responds. Expect a retransmission.
    reconnectChannel(initialVersion, SIG1, initialVersion, SIG1, delta);

    // Now ack.
    listener.checkOpsReceived(0);
    operationChannel.onAck(1, HashedVersion.of(43, ackSignature));

    // There should be a fake op resulting from ack that updates version info
    listener.checkOpsReceived(1);
    checkExpectationsSatisfied();
  });

  /**
   * Test reconnect where there was an unACKed delta during the failure
   * which was received and ACKed by the server, but this was unseen by the
   * client. On reconnect, the server responds with that delta and a new
   * one, the client should receive only the new one.
   */
  it("reconnectWithPendingAckAndNewDeltaSentByServer", () => {
    const initialVersion = 42;
    const initialSignature = SIG1;
    connectChannel(initialVersion, initialSignature);

    const delta = sendAndCheckRandomOp(operationChannel, 42, SIG1);

    // Reconnect at the version before that delta, but with a later
    // current version including it.
    const afterVersion = 44;
    const afterSignature = SIG3;
    reconnectChannel(initialVersion, initialSignature, afterVersion, afterSignature, null);

    // Receive delta that is the client's own un-acked op.
    const serverDelta = TransformedWaveletDelta.fromDelta(HashedVersion.of(43, SIG2), 0, delta);
    operationChannel.onDelta(serverDelta);

    // It should implicitly cause an ack, so read that fake op
    // to update version info, plus a VersionUpdateOp.
    listener.checkOpsReceived(2);
    expect(operationChannel.receive()).not.toBeNull();
    listener.clear();

    // Receive another op
    const rdelta = createRandomTransformedDelta(43, 1, SIG4);
    operationChannel.onDelta(rdelta);

    // Recovery should be complete since the client has caught up with the
    // server, and there should be one readable delta.
    listener.checkOpsReceived(1);
    expect(operationChannel.receive()).not.toBeNull();
    checkExpectationsSatisfied();
  });

  /**
   * Test reconnect where there was an ACKed and an unACKed delta, the
   * server loses state and accepted a delta from another client assigning
   * the same version number. Expect failure.
   */
  it("reconnectFailureWithAckAndPendingAckWithCollidingVersionNumber", () => {
    const initialVersion = 42;
    const initialSignature = SIG1;
    const ackSignature = SIG2;
    connectChannel(initialVersion, initialSignature);

    // Send an op and ack it.
    sendAndCheckRandomOp(operationChannel, 42, SIG1);
    operationChannel.onAck(1, HashedVersion.of(43, ackSignature));

    // Take the fake op resulting from ack that updates version info.
    listener.checkOpsReceived(1);
    expect(operationChannel.receive()).not.toBeNull();
    listener.clear();

    // Send an op, then fail & reconnect.
    sendAndCheckRandomOp(operationChannel, 43, ackSignature);
    reconnectChannel(43, ackSignature, 47, SIG3, null);

    // Receive a different delta to the first one the client sent.
    expectChannelException(
      () => operationChannel.onDelta(createRandomTransformedDelta(42, 5, SIG4)),
      "Should have thrown ChannelException",
    );
    checkExpectationsSatisfied();
  });

  /**
   * Tests reconnection where the server does not provide a matching
   * signature. Expect failure.
   */
  it("reconnectionFailure", () => {
    const initialVersion = 42;
    const initialSignature = SIG1;
    connectChannel(initialVersion, initialSignature);

    // Simulate failure
    operationChannel.reset();

    // Server presents unknown reconnect version
    const reconnectVersion = HashedVersion.of(66, SIG2);
    expectChannelException(
      () => operationChannel.onConnection(reconnectVersion, reconnectVersion),
      "Should have thrown ChannelException",
    );
  });

  /**
   * Test reconnect where client sends a delta to the server and the server
   * forgets that delta on reconnection. Then check life can continue as
   * normal after reconnection.
   */
  it("reconnectWherePendingAckCollisionCanBeRecovered", () => {
    const initialVersion = 42;
    const initialSignature = SIG1;
    connectChannel(initialVersion, initialSignature);

    // Receive a delta to reset the recovery mechanism so the next
    // recovery will be at version 43.
    const signature43 = SIG2;
    operationChannel.onDelta(createRandomTransformedDelta(42, 1, signature43));
    expect(operationChannel.receive()).not.toBeNull();
    listener.clear();

    // Send a delta.
    sendAndCheckRandomOp(operationChannel, 43, signature43);

    // Reconnect at 43.
    reconnectChannel(43, signature43, 44, SIG3, null);

    // Now receive a different delta to the un-acked delta1.
    const delta2 = createRandomTransformedDelta(43, 1, SIG4);
    operationChannel.onDelta(delta2);
    listener.checkOpsReceived(1);
    expect(operationChannel.receive()).not.toBeNull();
    listener.clear();

    // Push another delta to the channel, but don't expect a delta channel
    // send yet.
    const delta3 = UTIL.makeDelta(HashedVersion.of(45, SIG5), 0, 1);
    operationChannel.send(delta3.ops[0]!);
    listener.checkOpsReceived(0);

    // Ack the first op, and expect a send for the pending op3.
    deltaChannel.expectSend(delta3);
    operationChannel.onAck(1, HashedVersion.of(45, SIG5));
    checkExpectationsSatisfied();
  });

  /** Check that the client sends the correct last known signatures. */
  it("reconnectWhereAnAckWasNotCommittedCanBeRecovered", () => {
    const initialVersion = 42;
    connectChannel(initialVersion, SIG1);

    // Send op and expect an Ack.
    const signature43 = SIG2;
    const signature44 = SIG3;
    sendAndCheckRandomOp(operationChannel, 42, SIG1);
    operationChannel.onAck(1, HashedVersion.of(43, signature43));

    // Send another op and expect an Ack.
    const delta2 = sendAndCheckRandomOp(operationChannel, 43, signature43);
    operationChannel.onAck(1, HashedVersion.of(44, signature44));

    // send commit for first Ack.
    operationChannel.onCommit(43);

    // Check that we reconnect, the server went down and has a LCV of 43.
    // Channel will resend delta2.
    reconnectChannel(43, signature43, 43, signature43, delta2);
    deltaChannel.checkExpectationsSatisfied();
    checkExpectationsSatisfied();
  });
});
