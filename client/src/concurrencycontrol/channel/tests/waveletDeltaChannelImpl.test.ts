// Tests the WaveletDeltaChannelImpl by mocking the back-end channel.
//
// Ported from `WaveletDeltaChannelImplTest` of Apache Wave (Apache License,
// Version 2.0).

import { beforeEach, describe, expect, it } from "vitest";

import type { WaveletData } from "../../../model/data";
import { ParticipantId, WaveId, WaveletId } from "../../../model/id";
import type { TransformedWaveletDelta, WaveletDelta } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { IllegalStateError, Recoverable, ResponseCode } from "../../common";
import {
  ClientMessage,
  type SubmitCallback,
  type WaveletChannel,
  WaveletDeltaChannelImpl,
  type WaveletDeltaChannelReceiver,
} from "..";
import {
  DeltaTestUtil,
  bytes,
  emptySnapshot,
  expectChannelException,
  expectVersionEquals,
} from "./helpers";

type ReceiverExpectation =
  | {
      readonly method: "CONNECT";
      readonly connectVersion: HashedVersion;
      readonly currentVersion: HashedVersion;
    }
  | { readonly method: "ACK"; readonly opsApplied: number; readonly version: HashedVersion }
  | { readonly method: "COMMIT"; readonly version: number }
  | { readonly method: "DELTA"; readonly delta: TransformedWaveletDelta }
  | { readonly method: "NACK"; readonly errorString: string | null; readonly version: number };

/**
 * A mock delta channel receiver which supports setting expectations of
 * method calls.
 */
class MockReceiver implements WaveletDeltaChannelReceiver {
  private readonly expectations: ReceiverExpectation[] = [];

  expectConnection(connectVersion: HashedVersion, currentVersion: HashedVersion): void {
    this.expectations.push({ method: "CONNECT", connectVersion, currentVersion });
  }

  expectAck(opsApplied: number, version: HashedVersion): void {
    this.expectations.push({ method: "ACK", opsApplied, version });
  }

  expectCommit(version: number): void {
    this.expectations.push({ method: "COMMIT", version });
  }

  expectDelta(delta: TransformedWaveletDelta): void {
    this.expectations.push({ method: "DELTA", delta });
  }

  expectNack(errorString: string | null, version: number): void {
    this.expectations.push({ method: "NACK", errorString, version });
  }

  checkExpectationsSatisfied(): void {
    expect(this.expectations.map((e) => e.method)).toEqual([]);
  }

  private next<M extends ReceiverExpectation["method"]>(
    method: M,
  ): Extract<ReceiverExpectation, { method: M }> {
    const expected = this.expectations.shift();
    if (expected === undefined) {
      return expect.fail(`Unexpected receiver call: ${method}`);
    }
    expect(expected.method).toBe(method);
    return expected as Extract<ReceiverExpectation, { method: M }>;
  }

  onConnection(connectVersion: HashedVersion, currentVersion: HashedVersion): void {
    const expected = this.next("CONNECT");
    expectVersionEquals(connectVersion, expected.connectVersion, "incorrect connectVersion");
    expectVersionEquals(currentVersion, expected.currentVersion, "incorrect currentVersion");
  }

  onAck(opsApplied: number, version: HashedVersion): void {
    const expected = this.next("ACK");
    expect(opsApplied, "incorrect opsApplied").toBe(expected.opsApplied);
    expectVersionEquals(version, expected.version, "incorrect version");
  }

  onCommit(version: number): void {
    const expected = this.next("COMMIT");
    expect(version, "incorrect version").toBe(expected.version);
  }

  onDelta(delta: TransformedWaveletDelta): void {
    const expected = this.next("DELTA");
    expect(delta.equals(expected.delta), "incorrect delta").toBe(true);
  }

  onNack(responseCode: ResponseCode, errorString: string | null, version: number): void {
    const expected = this.next("NACK");
    expect(responseCode !== ResponseCode.OK).toBe(true);
    expect(errorString, "incorrect error string").toBe(expected.errorString);
    expect(version, "incorrect version").toBe(expected.version);
  }
}

/** A mock wavelet channel which allows setting expectations for method calls. */
class MockWaveletChannel implements WaveletChannel {
  private readonly expectations: WaveletDelta[] = [];
  private readonly awaitingAck: SubmitCallback[] = [];

  expectSubmit(delta1: WaveletDelta): void {
    this.expectations.push(delta1);
  }

  checkExpectationsSatisfied(): void {
    expect(this.expectations.length).toBe(0);
  }

  submit(delta: WaveletDelta, callback: SubmitCallback): void {
    const e = this.expectations.shift();
    if (e === undefined) {
      return expect.fail("Unexpected submit");
    }
    expect(e.equals(delta)).toBe(true);
    this.awaitingAck.push(callback);
  }

  private nextCallback(): SubmitCallback {
    const callback = this.awaitingAck.shift();
    if (callback === undefined) {
      return expect.fail("No submit awaiting a response");
    }
    return callback;
  }

  ackSubmit(opsApplied: number, version: number, signature: Uint8Array): void {
    this.nextCallback().onSuccess(
      opsApplied,
      HashedVersion.of(version, signature),
      ResponseCode.OK,
      null,
    );
  }

  nackSubmit(reason: string, error: ResponseCode, version: number, signature: Uint8Array): void {
    this.nextCallback().onSuccess(0, HashedVersion.of(version, signature), error, reason);
  }

  failSubmit(reason: string): void {
    this.nextCallback().onFailure(reason);
  }

  debugGetProfilingInfo(): string | null {
    return null;
  }
}

const USER_ID = ParticipantId.ofUnsafe("test@example.com");
const UTIL = new DeltaTestUtil(USER_ID);
const WAVE_ID = WaveId.of("example.com", "waveid");
const WAVELET_ID = WaveletId.of("example.com", "waveletid");

function sig(v: number): Uint8Array {
  return bytes(v & 0xff, v & 0xff, v & 0xff, v & 0xff);
}

describe("WaveletDeltaChannelImpl", () => {
  let waveletChannel: MockWaveletChannel;
  let receiver: MockReceiver;
  let deltaChannel: WaveletDeltaChannelImpl; // channel under test

  /** Tracks state for sendDelta. */
  let sendState: string;

  beforeEach(() => {
    waveletChannel = new MockWaveletChannel();
    receiver = new MockReceiver();
    deltaChannel = new WaveletDeltaChannelImpl(waveletChannel);
    deltaChannel.reset(receiver);
    sendState = "uninitialized";
  });

  // --- Helper methods --- //

  /**
   * Connects the channel by sending an initial message with a wavelet
   * snapshot. The message includes a committed version, which is
   * synthesized to match the snapshot version if not given.
   */
  function connectChannel(
    snapshot: WaveletData,
    committed: number = snapshot.version,
    commitSignature: Uint8Array = snapshot.hashedVersion.historyHash,
  ): void {
    const commitVersion = HashedVersion.of(committed, commitSignature);
    deltaChannel.onWaveletSnapshot(snapshot, commitVersion, null);
  }

  /**
   * Reconnects the channel at the current version by sending an initial
   * message with a version and signature.
   */
  function reconnectChannel(
    reconnect: readonly TransformedWaveletDelta[],
    committed: number,
    commitSignature: Uint8Array,
  ): void {
    const commitVersion = HashedVersion.of(committed, commitSignature);
    deltaChannel.onWaveletUpdate(reconnect, commitVersion, null);
  }

  /** Connects the channel at a version and checks expectations. */
  function checkedConnectChannel(currentVersion: number): void {
    const signature = sig(currentVersion);
    const signedVersion = HashedVersion.of(currentVersion, sig(currentVersion));
    const wavelet = buildSnapshot(currentVersion, signature);
    receiver.expectConnection(signedVersion, signedVersion);
    receiver.expectCommit(currentVersion);
    connectChannel(wavelet);
    receiver.checkExpectationsSatisfied();
  }

  /** Reconnects the channel and checks expectations. */
  function checkedReconnectChannel(
    list: readonly TransformedWaveletDelta[],
    committed: number,
    commitSignature: Uint8Array,
  ): void {
    const first = list[0]!;
    const signedVersion = HashedVersion.of(
      first.appliedAtVersion,
      first.resultingVersion.historyHash,
    );
    receiver.expectConnection(signedVersion, signedVersion);
    receiver.expectCommit(committed);
    reconnectChannel(list, committed, commitSignature);
  }

  /** Simulates channel termination. */
  function closeChannel(): void {
    deltaChannel.reset(null);
  }

  /** Sends a delta on a connected channel. */
  function submitDeltaOnConnectedChannel(delta1: WaveletDelta): void {
    waveletChannel.expectSubmit(delta1);
    sendState = "initial";
    sendDelta(delta1);
    expect(sendState).toBe("sending");
  }

  /**
   * Sends a delta on a channel. The sendState is set to "sending" when the
   * channel takes the delta.
   */
  function sendDelta(delta: WaveletDelta): void {
    deltaChannel.send({
      takeMessage: () => {
        expect(sendState).toBe("initial");
        sendState = "sending";
        return new ClientMessage(delta, false);
      },
    });
  }

  /** Acknowledges a delta on a connected channel. */
  function ackDeltaOnConnectedChannel(
    ackedVersion: number,
    opsApplied: number,
    signature: Uint8Array,
  ): void {
    waveletChannel.ackSubmit(opsApplied, ackedVersion, signature);
  }

  /** Negatively acknowledges a delta on a connected channel. */
  function nackDeltaOnConnectedChannel(
    ackedVersion: number,
    signature: Uint8Array,
    reason: string,
    error: ResponseCode,
  ): void {
    waveletChannel.nackSubmit(reason, error, ackedVersion, signature);
  }

  /**
   * Receives a delta message on a connected channel. `delta` is null for no
   * delta and `committed` -1 to omit the last committed version.
   */
  function receiveUpdateOnConnectedChannel(
    delta: TransformedWaveletDelta | null,
    committed = -1,
    commitSignature: Uint8Array = bytes(),
  ): void {
    let commitVersion: HashedVersion | null = null;
    const deltas: TransformedWaveletDelta[] = [];
    if (delta !== null) {
      deltas.push(delta);
    }
    if (committed !== -1) {
      commitVersion = HashedVersion.of(committed, commitSignature);
    }
    deltaChannel.onWaveletUpdate(deltas, commitVersion, null);
  }

  /** Builds a minimal snapshot message. */
  function buildSnapshot(version: number, signature: Uint8Array): WaveletData {
    const hv = HashedVersion.of(version, signature);
    return emptySnapshot(WAVE_ID, WAVELET_ID, ParticipantId.ofUnsafe("creator@gwave.com"), hv, 0);
  }

  /** Builds a minimal reconnection initial delta. */
  function buildReconnect(version: number, signature: Uint8Array): TransformedWaveletDelta[] {
    return [UTIL.makeTransformedDelta(0, HashedVersion.of(version, signature), 0)];
  }

  /** Builds a client delta with numOps ops. */
  function buildDelta(targetVersion: number, numOps: number): WaveletDelta {
    return UTIL.makeDelta(HashedVersion.unsigned(targetVersion), 123457890, numOps);
  }

  /** Builds a server delta with numOps ops. */
  function buildServerDelta(initialVersion: number, numOps: number): TransformedWaveletDelta {
    return UTIL.makeTransformedDelta(
      1234567890,
      HashedVersion.unsigned(initialVersion + numOps),
      numOps,
    );
  }

  /** Tests that resetting a new channel does nothing. */
  it("resetNewChannel", () => {
    deltaChannel.reset(null);
    waveletChannel.checkExpectationsSatisfied();
  });

  /** Tests that sending on an unconnected channel fails. */
  it("sendBeforeConnectFails", () => {
    expect(() => sendDelta(buildDelta(1, 1))).toThrow(IllegalStateError);
    waveletChannel.checkExpectationsSatisfied();
  });

  /** Tests that a channel with no receivers quietly drops messages. */
  it("channelWithNoReceiverDropsMessages", () => {
    const initialVersion = 57;
    const signature = sig(1);
    const wavelet = buildSnapshot(initialVersion, signature);
    const delta = buildServerDelta(initialVersion, 3);

    deltaChannel.reset(null); // Clear receiver.
    connectChannel(wavelet);
    receiveUpdateOnConnectedChannel(delta, initialVersion, signature);
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a last committed version sent with the connect message is
   * delivered.
   */
  it("connectReceivesCommittedVersion", () => {
    const currentVersion = 57;
    const signature = sig(1);
    const committedVersion = 50;
    const committedSignature = sig(2);
    const signedVersion = HashedVersion.of(currentVersion, signature);
    const wavelet = buildSnapshot(currentVersion, signature);

    receiver.expectConnection(signedVersion, signedVersion);
    receiver.expectCommit(committedVersion);
    connectChannel(wavelet, committedVersion, committedSignature);
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that the last committed version sent with a reconnect initial
   * delta is delivered.
   */
  it("reconnectReceivesCommittedVersion", () => {
    const currentVersion = 57;
    const signature = sig(1);
    const committedVersion = 50;
    const committedSignature = sig(2);
    const signedVersion = HashedVersion.of(currentVersion, signature);
    const reconnect = buildReconnect(currentVersion, signature);

    // Expect connection.
    receiver.expectConnection(signedVersion, signedVersion);
    receiver.expectCommit(committedVersion);
    reconnectChannel(reconnect, committedVersion, committedSignature);
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that when the client terminates the channel the receiver is not
   * notified.
   */
  it("clientResetTerminatesSilently", () => {
    checkedConnectChannel(57);
    deltaChannel.reset(null);
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a delta sent down the channel is received and the
   * acknowledgment delivered.
   */
  it("submitDelta", () => {
    const currentVersion = 57;
    const ops1 = 7;
    const delta1 = buildDelta(currentVersion, ops1);
    const signature1 = sig(1);
    const delta2 = buildDelta(currentVersion + ops1, 2);
    const errorMsg2 = "SERVER_ERROR";

    checkedConnectChannel(currentVersion);

    // Send delta1.
    submitDeltaOnConnectedChannel(delta1);

    // All ops are acked.
    receiver.expectAck(ops1, HashedVersion.of(currentVersion + ops1, signature1));
    ackDeltaOnConnectedChannel(currentVersion + ops1, ops1, signature1);

    // Send delta2.
    submitDeltaOnConnectedChannel(delta2);

    // Nack with a randomly injected error, as if something just went wrong
    // server-side.
    receiver.expectNack(errorMsg2, currentVersion + ops1);
    nackDeltaOnConnectedChannel(
      currentVersion + ops1,
      signature1,
      errorMsg2,
      ResponseCode.INTERNAL_ERROR,
    );

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
    waveletChannel.checkExpectationsSatisfied();
  });

  /** Tests that a delta submitted after a reset has sequence number 1. */
  it("submitAfterResetRestartsSequence", () => {
    const currentVersion = 57;
    const ops1 = 7;
    const delta1 = buildDelta(currentVersion, ops1);
    const signature1 = sig(1);
    const delta2 = buildDelta(currentVersion + ops1, 2);

    checkedConnectChannel(currentVersion);

    // Send delta1.
    submitDeltaOnConnectedChannel(delta1);

    // All ops are acked.
    receiver.expectAck(ops1, HashedVersion.of(currentVersion + ops1, signature1));
    ackDeltaOnConnectedChannel(currentVersion + ops1, ops1, signature1);

    deltaChannel.reset(receiver);
    checkedReconnectChannel(
      buildReconnect(currentVersion + ops1, signature1),
      currentVersion + ops1,
      signature1,
    );

    // Send delta2.
    submitDeltaOnConnectedChannel(delta2);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a delta (with commit version) received from the server is
   * delivered.
   */
  it("receiveDelta", () => {
    const initialVersion = 57;
    const commitSig = sig(1);
    const delta = buildServerDelta(initialVersion, 7);

    checkedConnectChannel(initialVersion);

    // Receive and deliver delta.
    receiver.expectCommit(initialVersion);
    receiver.expectDelta(delta);
    receiveUpdateOnConnectedChannel(delta, initialVersion, commitSig);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a last-committed-version message with no deltas is correctly
   * delivered.
   */
  it("receiveLastCommittedVersion", () => {
    const initialVersion = 57;
    const committedVersion = 50;
    const committedSignature = sig(1);

    checkedConnectChannel(initialVersion);

    receiver.expectCommit(committedVersion);
    receiveUpdateOnConnectedChannel(null, committedVersion, committedSignature);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a submit delta interleaves properly with received deltas
   * when the ACK is received in sequence with server deltas.
   */
  it("synchronizedAckDelta", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Acknowledge all ops immediately.
    const versionAfterClient = versionAfterServer1 + clientOps;
    const ackedSignature = sig(1);
    receiver.expectAck(clientOps, HashedVersion.of(versionAfterClient, ackedSignature));
    ackDeltaOnConnectedChannel(versionAfterClient, clientOps, ackedSignature);

    // Receive a second server delta.
    const serverOps2 = 3;
    const delta2 = buildServerDelta(versionAfterClient, serverOps2);
    receiver.expectDelta(delta2);
    receiveUpdateOnConnectedChannel(delta2);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a submit delta interleaves properly with received deltas
   * when the ACK is received after a subsequent server delta.
   */
  it("lateAckDelta", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const sigAfterServer1 = sig(1);
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Receive a second server delta, logically after the client ops.
    const serverOps2 = 3;
    const versionAfterClient = versionAfterServer1 + clientOps;
    const sigAfterClient = sig(2);
    const delta2 = buildServerDelta(versionAfterClient, serverOps2);
    // Don't expect the delta yet.
    receiveUpdateOnConnectedChannel(delta2);
    receiver.checkExpectationsSatisfied();

    // Receive commit message for a received delta while there are queued
    // messages; the message jumps the queue to be delivered immediately.
    receiver.expectCommit(versionAfterServer1);
    receiveUpdateOnConnectedChannel(null, versionAfterServer1, sigAfterServer1);
    receiver.checkExpectationsSatisfied();

    // Receive a commit message for the outstanding submit delta. This
    // message is delayed until the ack.
    receiveUpdateOnConnectedChannel(null, versionAfterClient, sigAfterClient);

    // Receive the ack and expect the pending delta and commit (in the order received).
    const ackedSignature = sig(1);
    receiver.expectAck(clientOps, HashedVersion.of(versionAfterClient, ackedSignature));
    receiver.expectCommit(versionAfterClient);
    receiver.expectDelta(delta2);
    ackDeltaOnConnectedChannel(versionAfterClient, clientOps, ackedSignature);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a submit delta interleaves properly with received deltas
   * when the ACK is received before a preceding server delta.
   */
  it("earlyAckDelta", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Submit delta.
    const clientOps = 5;
    const clientDelta = buildDelta(initialVersion, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Acknowledge the submitted delta against a future version.
    const versionAfterServer1 = initialVersion + serverOps1;
    const versionAfterClient = versionAfterServer1 + clientOps;
    const ackedSignature = sig(1);
    // Don't expect ack yet.
    ackDeltaOnConnectedChannel(versionAfterClient, clientOps, ackedSignature);

    // Receive the server delta and then expect the pending ack.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiver.expectAck(clientOps, HashedVersion.of(versionAfterClient, ackedSignature));
    receiveUpdateOnConnectedChannel(delta1);

    // Receive a second server delta, logically after the client ops.
    const serverOps2 = 3;
    const delta2 = buildServerDelta(versionAfterClient, serverOps2);
    receiver.expectDelta(delta2);
    receiveUpdateOnConnectedChannel(delta2);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that the channel can handle receiving an ack for fewer ops than
   * were submitted, as happens when the server transforms some away.
   *
   * Only the synchronized case is tested.
   */
  it("shrunkAckDelta", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Acknowledge immediately.
    const ackedOps = 0;
    const versionAfterClient = versionAfterServer1 + ackedOps;
    const ackedSignature = sig(1);
    receiver.expectAck(ackedOps, HashedVersion.of(versionAfterClient, ackedSignature));
    ackDeltaOnConnectedChannel(versionAfterClient, ackedOps, ackedSignature);

    // Receive a second server delta.
    const serverOps2 = 3;
    const delta2 = buildServerDelta(versionAfterClient, serverOps2);
    receiver.expectDelta(delta2);
    receiveUpdateOnConnectedChannel(delta2);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that a submit delta interleaves properly with received deltas
   * when ops are NACKed before the next server delta.
   */
  it("synchronizedNackDelta", () => {
    const initialVersion = 57;
    const serverOps1 = 7;
    const delta1 = buildServerDelta(initialVersion, serverOps1);

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const sigAfterServer1 = sig(1);
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Nack delta immediately.
    const error = "error";
    receiver.expectNack(error, versionAfterServer1);
    nackDeltaOnConnectedChannel(
      versionAfterServer1,
      sigAfterServer1,
      error,
      ResponseCode.BAD_REQUEST,
    );

    // Receive a second server delta.
    const serverOps2 = 3;
    const delta2 = buildServerDelta(versionAfterServer1, serverOps2);
    receiver.expectDelta(delta2);
    receiveUpdateOnConnectedChannel(delta2);

    // Try another submit.
    const versionAfterServer2 = versionAfterServer1 + serverOps2;
    const clientDelta2 = buildDelta(versionAfterServer2, clientOps);
    submitDeltaOnConnectedChannel(clientDelta2);

    // Ack.
    const versionAfterClient2 = versionAfterServer2 + clientOps;
    const ackedSignature2 = sig(2);
    receiver.expectAck(clientOps, HashedVersion.of(versionAfterClient2, ackedSignature2));
    ackDeltaOnConnectedChannel(versionAfterClient2, clientOps, ackedSignature2);

    // Close.
    closeChannel();
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that receiving an acknowledged delta submitted by this client is
   * detected as a server-side error.
   */
  it("reflectedSubmittedDeltaAfterAckIsError", () => {
    const currentVersion = 57;
    const ops1 = 7;
    const signature1 = sig(1);
    const delta1 = buildDelta(currentVersion, ops1);

    checkedConnectChannel(currentVersion);

    // Send delta1.
    submitDeltaOnConnectedChannel(delta1);

    // All ops are acked.
    receiver.expectAck(ops1, HashedVersion.of(currentVersion + ops1, signature1));
    ackDeltaOnConnectedChannel(currentVersion + ops1, ops1, signature1);

    // Receive the delta (erroneously). Expect termination to be reported.
    expectChannelException(
      () => receiveUpdateOnConnectedChannel(buildServerDelta(currentVersion, ops1)),
      "ChannelException expected",
    );
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that receiving an ack for a delta already received by this client
   * is detected as a server-side error.
   */
  it("ackForReceivedDeltaIsError", () => {
    const currentVersion = 57;
    const ops1 = 7;
    const signature1 = sig(1);
    const delta1 = buildDelta(currentVersion, ops1);

    checkedConnectChannel(currentVersion);

    // Send delta1.
    submitDeltaOnConnectedChannel(delta1);

    // Receive the delta (erroneously), but we can't detect it's an error yet.
    const serverDelta1 = buildServerDelta(currentVersion, ops1);
    receiver.expectDelta(serverDelta1);
    receiveUpdateOnConnectedChannel(serverDelta1);

    // Now receive the ack for the delta. Expect failure.
    expectChannelException(
      () => ackDeltaOnConnectedChannel(currentVersion + ops1, ops1, signature1),
      "ChannelException expected",
    );
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that an ack received for a delta submitted before a channel
   * reconnects is dropped.
   */
  it("ackAfterReconnectIgnored", () => {
    const initialVersion = 57;
    const initialSignature = sig(4);

    checkedConnectChannel(initialVersion);

    // Submit delta.
    const clientOps = 5;
    const clientDelta = buildDelta(initialVersion, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Reset channel.
    deltaChannel.reset(receiver);
    const reconnect = buildReconnect(initialVersion, initialSignature);
    checkedReconnectChannel(reconnect, 0, bytes());

    // Acknowledge outstanding submit.
    const versionAfterClient = initialVersion + clientOps;
    const ackedSignature = sig(1);
    // Don't expect the ack at the receiver.
    ackDeltaOnConnectedChannel(versionAfterClient, clientOps, ackedSignature);
    receiver.checkExpectationsSatisfied();
  });

  it("nackTooOldIsRecoverable", () => {
    const initialVersion = 0;
    checkedConnectChannel(initialVersion);

    // Submit delta.
    const submitVersion = 0;
    const signature = sig(1);
    const clientOps = 1;
    const clientDelta = buildDelta(submitVersion, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Nack delta immediately with TOO_OLD.
    const e = expectChannelException(
      () => nackDeltaOnConnectedChannel(submitVersion, signature, "too old", ResponseCode.TOO_OLD),
      "Expected an exception",
    );
    expect(e.recoverable).toBe(Recoverable.RECOVERABLE);
  });

  /**
   * Tests that the delta channel detects a gap in the op stream and throws
   * an exception.
   */
  it("missingDeltaKillsChannel", () => {
    const initialVersion = 57;
    const signature = sig(1);
    const ops = 7;
    const delta1 = buildServerDelta(initialVersion, ops);

    checkedConnectChannel(initialVersion);

    // Receive and deliver delta.
    receiver.expectCommit(initialVersion);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1, initialVersion, signature);

    // Receive delta with a version number too high.
    const delta2 = buildServerDelta(initialVersion + ops + 1, 1);
    expectChannelException(
      () => receiveUpdateOnConnectedChannel(delta2, initialVersion, signature),
      "Expected a ChannelException",
    );
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that the delta channel detects a gap in the op stream even when
   * there is an outstanding submission.
   */
  it("missingDeltaWithLateAckKillsChannel", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Receive a second server delta that's after our submission,
    // will be queued.
    const versionAfterClient = versionAfterServer1 + clientOps;
    const serverOps2 = 3;
    const delta2 = buildServerDelta(versionAfterClient, serverOps2);
    receiveUpdateOnConnectedChannel(delta2);

    // Receive a third server delta that's skipped a version.
    const versionAfterServer2 = versionAfterClient + serverOps2;
    const delta3 = buildServerDelta(versionAfterServer2 + 1, 1);
    expectChannelException(
      () => receiveUpdateOnConnectedChannel(delta3),
      "Expected a ChannelException",
    );
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that the delta channel detects a gap in the op stream even when
   * there is an outstanding delta submission if the ack couldn't possibly
   * account for the version gap.
   */
  it("missingDeltaBeyondLateAckKillsChannel", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Receive a second server delta, too far ahead of the client ops.
    const versionAfterClient = versionAfterServer1 + clientOps;
    const delta2 = buildServerDelta(versionAfterClient + 1, 1);
    expectChannelException(
      () => receiveUpdateOnConnectedChannel(delta2),
      "Expected a ChannelException",
    );
    receiver.checkExpectationsSatisfied();
  });

  /**
   * Tests that the delta channel detects a gap in the op stream when a
   * received ack doesn't account for the version gap.
   */
  it("missingDeltaBeyondShortAckKillsChannel", () => {
    const initialVersion = 57;
    const serverOps1 = 7;

    checkedConnectChannel(initialVersion);

    // Receive server delta.
    const delta1 = buildServerDelta(initialVersion, serverOps1);
    receiver.expectDelta(delta1);
    receiveUpdateOnConnectedChannel(delta1);

    // Submit delta.
    const versionAfterServer1 = initialVersion + serverOps1;
    const clientOps = 5;
    const clientDelta = buildDelta(versionAfterServer1, clientOps);
    submitDeltaOnConnectedChannel(clientDelta);

    // Receive a second server delta that initially looks ok.
    const versionAfterClient = versionAfterServer1 + clientOps;
    const delta2 = buildServerDelta(versionAfterClient, 1);
    receiveUpdateOnConnectedChannel(delta2);

    // Receive ack for fewer than the number of ops than we
    // sent (some were transformed away).
    const ackedSignature = sig(3);
    receiver.expectAck(clientOps - 1, HashedVersion.of(versionAfterClient - 1, ackedSignature));
    expectChannelException(
      () => ackDeltaOnConnectedChannel(versionAfterClient - 1, clientOps - 1, ackedSignature),
      "Expected a ChannelException",
    );
    receiver.checkExpectationsSatisfied();
  });
});
