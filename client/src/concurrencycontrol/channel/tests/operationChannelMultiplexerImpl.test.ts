// Test for the multiplexer. This really tests the whole client end of the
// stack above a mock view channel.
//
// Ported from `OperationChannelMultiplexerImplTest` of Apache Wave (Apache
// License, Version 2.0).

import { beforeEach, describe, expect, it } from "vitest";

import type { WaveletData } from "../../../model/data";
import { IdFilter, ParticipantId, WaveId, WaveletId } from "../../../model/id";
import {
  TransformedWaveletDelta,
  WaveletDelta,
  type WaveletOperation,
} from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import {
  ChannelException,
  type CorruptionDetail,
  Recoverable,
  type UnsavedDataListenerFactory,
} from "../../common";
import {
  Accessibility,
  ImmediateExecutionScheduler,
  KnownWavelet,
  type KnownWaveletVersions,
  type OperationChannel,
  type OperationChannelMultiplexerListener,
  OperationChannelMultiplexerImpl,
  type Scheduler,
  type SchedulerCommand,
  ViewChannelImpl,
  type ViewChannelListener,
  WaveletIdMap,
  createEmptyWaveletSnapshot,
} from "..";
import {
  DeltaTestUtil,
  FAKE_HASHED_VERSION_FACTORY,
  bytes,
  emptySnapshot,
  expectChannelException,
} from "./helpers";
import { MockMuxListener } from "./mockMuxListener";
import { MockOperationChannelListener } from "./mockOperationChannelListener";
import { MockViewChannel, MockViewChannelFactory } from "./mockViewChannel";

/** IDs of wavelets for testing. */
const WAVELET_ID_1 = WaveletId.of("example.com", "w+1234");
const WAVELET_ID_2 = WaveletId.of("example.com", "w+5678");

/** ID of wave for testing. */
const WAVE_ID = WaveId.of("example.com", "waveId_1");

/** User name for testing. */
const USER_ID = ParticipantId.ofUnsafe("fred@example.com");

const NOSIG = bytes();
const SIG1 = bytes(1, 1, 1, 1);
const SIG2 = bytes(2, 2, 2, 2);
const SIG3 = bytes(3, 3, 3, 3);
const SIG4 = bytes(4, 4, 4, 4);
const SIG5 = bytes(5, 5, 5, 5);
const SIG6 = bytes(6, 6, 6, 6);
const SIG7 = bytes(7, 7, 7, 7);

const ALL_IDS = IdFilter.allIds();

function noKnownWavelets(): KnownWaveletVersions {
  return new WaveletIdMap();
}

const testUtil = new DeltaTestUtil(USER_ID);

function createSnapshot(
  waveletId: WaveletId,
  version: number,
  signature: Uint8Array,
): WaveletData {
  const hv = HashedVersion.of(version, signature);
  return emptySnapshot(WAVE_ID, waveletId, USER_ID, hv, 1273307837000);
}

/** Holds information about a single channel from a mux. */
class ConnectionInfo {
  readonly snapshot: WaveletData;
  readonly initialHashedVersion: HashedVersion;

  constructor(
    readonly waveletId: WaveletId,
    readonly initialVersion: number,
    readonly initialSignature: Uint8Array,
  ) {
    this.snapshot = createSnapshot(waveletId, initialVersion, initialSignature);
    this.initialHashedVersion = HashedVersion.of(
      initialVersion,
      initialSignature,
    );
  }
}

interface ConnectedChannel {
  readonly channel: OperationChannel;
  readonly listener: MockOperationChannelListener;
}

class FakeScheduler implements Scheduler {
  command: SchedulerCommand | null = null;

  reset(): void {
    // Do nothing
  }

  schedule(command: SchedulerCommand): boolean {
    this.command = command;
    return true;
  }
}

function createReconnect(
  connectVersion: number,
  connectSignature: Uint8Array,
): TransformedWaveletDelta {
  return new TransformedWaveletDelta(
    USER_ID,
    HashedVersion.of(connectVersion, connectSignature),
    0,
    [],
  );
}

function createKnownWavelet(
  snapshot: WaveletData,
  version: number,
  signature: Uint8Array,
  accessibility: Accessibility,
): KnownWavelet {
  const commitVersion = HashedVersion.of(version, signature);
  return new KnownWavelet(snapshot, commitVersion, accessibility);
}

function createServerDeltaList(
  version: number,
  numOps: number,
  signature: Uint8Array,
): TransformedWaveletDelta[] {
  return [
    testUtil.makeTransformedDelta(
      0,
      HashedVersion.of(version + numOps, signature),
      numOps,
    ),
  ];
}

function createOp(): WaveletOperation {
  return testUtil.noOp();
}

function createDelta(
  targetVersion: HashedVersion,
  ...ops: WaveletOperation[]
): WaveletDelta {
  return new WaveletDelta(USER_ID, targetVersion, ops);
}

function createKnownVersions(
  waveletId1: WaveletId,
  version1: number,
  hash1: Uint8Array,
  waveletId2?: WaveletId,
  version2?: number,
  hash2?: Uint8Array,
): KnownWaveletVersions {
  const versions: KnownWaveletVersions = new WaveletIdMap();
  versions.set(waveletId1, [HashedVersion.of(version1, hash1)]);
  if (
    waveletId2 !== undefined &&
    version2 !== undefined &&
    hash2 !== undefined
  ) {
    expect(waveletId1.equals(waveletId2)).toBe(false);
    versions.set(waveletId2, [HashedVersion.of(version2, hash2)]);
  }
  return versions;
}

function checkReceiveDelta(
  viewListener: ViewChannelListener,
  opChannel: OperationChannel,
  opChannelListener: MockOperationChannelListener,
  waveletId: WaveletId,
  version: number,
  numOps: number,
  signature: Uint8Array,
): void {
  // Receive delta from view channel, expect ops on op channel.
  const update = createServerDeltaList(version, numOps, signature);
  viewListener.onUpdate(waveletId, update, null, null);
  opChannelListener.checkOpsReceived(1);
  opChannelListener.clear();
  for (let i = 0; i < numOps; ++i) {
    expect(opChannel.receive()).not.toBeNull();
  }
  expect(opChannel.receive()).toBeNull();
}

/**
 * Sends an operation on an operation channel and expects the delta to be
 * submitted to the view.
 */
function checkSendDelta(
  viewChannel: MockViewChannel,
  opChannel: OperationChannel,
  initialVersion: HashedVersion,
  expectedWaveletId: WaveletId,
): WaveletDelta {
  const op = createOp();
  const delta = createDelta(initialVersion, op);
  viewChannel.expectSubmitDelta(expectedWaveletId, delta);
  opChannel.send(op);
  return delta;
}

/**
 * Acks a delta and checks that the fake version-incrementing op is received
 * from the operation channel.
 */
function checkAckDelta(
  viewChannel: MockViewChannel,
  opChannel: OperationChannel,
  opChannelListener: MockOperationChannelListener,
  ackedOps: number,
  version: number,
  signature: Uint8Array,
): void {
  viewChannel.ackSubmit(ackedOps, version, signature);
  opChannelListener.checkOpsReceived(1);
  opChannelListener.clear();
  expect(opChannel.receive()).not.toBeNull();
  opChannelListener.checkOpsReceived(0);
  opChannelListener.clear();
}

/** Receives a one-op delta and sends/acks another, checking expectations. */
function checkReceiveAndSend(
  viewListener: ViewChannelListener,
  viewChannel: MockViewChannel,
  ch: ConnectedChannel,
  waveletId: WaveletId,
  version: number,
): void {
  checkReceiveDelta(
    viewListener,
    ch.channel,
    ch.listener,
    waveletId,
    version,
    1,
    SIG2,
  );
  checkSendDelta(
    viewChannel,
    ch.channel,
    HashedVersion.of(version + 1, SIG2),
    waveletId,
  );
  checkAckDelta(viewChannel, ch.channel, ch.listener, 1, version + 2, SIG3);
}

/**
 * Sends a reconnect delta, by default with current version the same as the
 * reconnection version.
 */
function reconnectChannel(
  viewListener: ViewChannelListener,
  waveletId: WaveletId,
  connectVersion: number,
  connectSignature: Uint8Array,
  currentVersion: number = connectVersion,
  currentSignature: Uint8Array = connectSignature,
): void {
  const reconnect = createReconnect(connectVersion, connectSignature);
  const distinctVersion = HashedVersion.of(currentVersion, currentSignature);
  viewListener.onUpdate(
    waveletId,
    [reconnect],
    distinctVersion,
    distinctVersion,
  );
}

/** Reconnects a mux on a view, returning the reconnected view's listener. */
function reconnectView(
  view: MockViewChannel,
  ...channels: ConnectionInfo[]
): ViewChannelListener {
  const viewListener = view.takeListener();
  viewListener.onConnected();
  for (const chInfo of channels) {
    reconnectChannel(
      viewListener,
      chInfo.waveletId,
      chInfo.initialVersion,
      chInfo.initialSignature,
    );
  }
  viewListener.onOpenFinished();
  view.checkExpectationsSatisified();
  return viewListener;
}

type KnownWaveletDisconnectWhen =
  "BEFORE_VIEW_CONNECTED" | "AFTER_VIEW_CONNECTED" | "AFTER_RESYNC";
const KNOWN_WAVELET_DISCONNECT_ORDER: readonly KnownWaveletDisconnectWhen[] = [
  "BEFORE_VIEW_CONNECTED",
  "AFTER_VIEW_CONNECTED",
  "AFTER_RESYNC",
];

type NewWaveletDisconnectWhen =
  | "BEFORE_VIEW_CONNECTED"
  | "AFTER_VIEW_CONNECTED"
  | "AFTER_SEND_DELTA"
  | "AFTER_ACK_DELTA";
const NEW_WAVELET_DISCONNECT_ORDER: readonly NewWaveletDisconnectWhen[] = [
  "BEFORE_VIEW_CONNECTED",
  "AFTER_VIEW_CONNECTED",
  "AFTER_SEND_DELTA",
  "AFTER_ACK_DELTA",
];

/** `a.compareTo(b) >= 0` on enum constants. */
function atLeast<T>(order: readonly T[], a: T, b: T): boolean {
  return order.indexOf(a) >= order.indexOf(b);
}

describe("OperationChannelMultiplexerImpl", () => {
  let mux: OperationChannelMultiplexerImpl;
  let viewFactory: MockViewChannelFactory;
  let muxListener: MockMuxListener;

  /** Number of calls of the unsaved data listener factory's destroy(). */
  let destroyedListeners: WaveletId[];

  beforeEach(() => {
    ViewChannelImpl.resetViewChannelsPerWave();
    ViewChannelImpl.setMaxViewChannelsPerWave(Number.MAX_SAFE_INTEGER);
    viewFactory = new MockViewChannel.Factory();
    destroyedListeners = [];
    const fakeListenerFactory: UnsavedDataListenerFactory = {
      // The original returns null here, which the port's factory type does
      // not allow.
      create: () => ({ onUpdate: () => {}, onClose: () => {} }),
      destroy: (waveletId) => {
        destroyedListeners.push(waveletId);
      },
    };
    mux = new OperationChannelMultiplexerImpl(
      WAVE_ID,
      viewFactory,
      createEmptyWaveletSnapshot,
      null,
      fakeListenerFactory,
      new ImmediateExecutionScheduler(),
      FAKE_HASHED_VERSION_FACTORY,
    );
    muxListener = new MockMuxListener();
  });

  function checkSendClobberingSnapshot(
    viewListener: ViewChannelListener,
    channelToClobber: OperationChannel,
    snapshotUpdate: WaveletData,
    committed: HashedVersion,
  ): OperationChannel {
    viewListener.onSnapshot(WAVELET_ID_1, snapshotUpdate, committed, null);
    muxListener.verifyOperationChannelRemoved(channelToClobber);
    return muxListener.verifyOperationChannelCreated(
      snapshotUpdate,
      Accessibility.READ_WRITE,
    );
  }

  /** Opens a new mux and returns the created view mock. */
  function openMux(idFilter: IdFilter = ALL_IDS): MockViewChannel {
    const viewChannel = viewFactory.expectCreate();
    viewChannel.expectOpen(idFilter, noKnownWavelets());
    mux.open(muxListener, idFilter);
    return viewChannel;
  }

  /** Opens a new mux with a known wavelet and returns the created view mock. */
  function openMuxWithKnownWavelet(
    knownSnapshot: WaveletData,
  ): MockViewChannel {
    const version = knownSnapshot.version;
    const signature = knownSnapshot.hashedVersion.historyHash;
    const view = viewFactory.expectCreate();
    const expectedSigs = createKnownVersions(WAVELET_ID_1, version, signature);
    view.expectOpen(ALL_IDS, expectedSigs);
    mux.open(muxListener, ALL_IDS, [
      createKnownWavelet(
        knownSnapshot,
        version,
        signature,
        Accessibility.READ_WRITE,
      ),
    ]);
    return view;
  }

  function createOperationChannel(
    waveletId: WaveletId,
    address: ParticipantId,
  ): ConnectedChannel {
    mux.createOperationChannel(waveletId, address);
    const channel = muxListener.verifyOperationChannelCreated(
      createSnapshot(WAVELET_ID_1, 0, NOSIG),
      Accessibility.READ_WRITE,
    );
    const channelListener = new MockOperationChannelListener();
    channel.setListener(channelListener);
    return { channel, listener: channelListener };
  }

  function connectChannelSnapshot(
    viewListener: ViewChannelListener,
    info: ConnectionInfo,
  ): ConnectedChannel {
    const snapshot = info.snapshot;
    viewListener.onSnapshot(
      snapshot.waveletId,
      snapshot,
      info.initialHashedVersion,
      null,
    );
    const channel = muxListener.verifyOperationChannelCreated(
      snapshot,
      Accessibility.READ_WRITE,
    );
    const listener = new MockOperationChannelListener();
    channel.setListener(listener);
    return { channel, listener };
  }

  function expectConnectedChannel(
    knownSnapshot: WaveletData,
    accessibility: Accessibility,
  ): ConnectedChannel {
    const channel = muxListener.verifyOperationChannelCreated(
      knownSnapshot,
      accessibility,
    );
    const listener = new MockOperationChannelListener();
    channel.setListener(listener);
    return { channel, listener };
  }

  function checkOpenFinished(): void {
    muxListener.verifyOpenFinished();
  }

  function triggerAndCheckOpenFinished(
    viewListener: ViewChannelListener,
  ): void {
    viewListener.onOpenFinished();
    checkOpenFinished();
  }

  /**
   * Sends a ChannelException to a view listener and expects a new view to
   * be opened to reconnect it. Returns the new mock view.
   */
  function failViewAndExpectReconnection(
    viewListenerToFail: ViewChannelListener,
    failingView: MockViewChannel,
    failureReason: string,
    expectedReconnectionSigs: KnownWaveletVersions,
  ): MockViewChannel {
    failingView.expectClose();
    const newView = viewFactory.expectCreate();
    newView.expectOpen(ALL_IDS, expectedReconnectionSigs);

    viewListenerToFail.onException(
      new ChannelException(failureReason, Recoverable.RECOVERABLE),
    );
    viewListenerToFail.onClosed();
    failingView.checkExpectationsSatisified();
    return newView;
  }

  /**
   * Reconnects a view with a single operation channel and checks that the
   * channel is usable and expectations are satisfied.
   */
  function reconnectViewAndCheckEverythingStillWorks(
    view: MockViewChannel,
    chInfo: ConnectionInfo,
    ch: ConnectedChannel,
  ): void {
    // Perform the reconnect.
    const viewListener2 = reconnectView(view, chInfo);
    ch.listener.checkOpsReceived(0);

    // Check everything still works.
    checkReceiveAndSend(
      viewListener2,
      view,
      ch,
      WAVELET_ID_1,
      chInfo.initialVersion,
    );
    view.checkExpectationsSatisified();
  }

  it("muxOpenOpensView", () => {
    // Connect to the server.
    const viewChannel = viewFactory.expectCreate();
    viewChannel.expectOpen(ALL_IDS, noKnownWavelets());
    mux.open(muxListener, ALL_IDS);
    viewChannel.takeListener().onConnected();

    viewChannel.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("receivedSnapshotOpensChannel", () => {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const viewChannel = openMux();
    const viewListener = viewChannel.takeListener();

    muxListener.verifyNoMoreInteractions();
    viewListener.onConnected();
    connectChannelSnapshot(viewListener, chInfo);
    triggerAndCheckOpenFinished(viewListener);

    viewChannel.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
    // Also proves that an expected (initial) snapshot doesn't clobber the channel
    expect(destroyedListeners.length).toBe(0);
  });

  it("receivedSnapshotClobbersExistingChannel", () => {
    const viewChannel = openMux();
    const viewListener = viewChannel.takeListener();

    muxListener.verifyNoMoreInteractions();
    viewListener.onConnected();
    const snapshotUpdate = createSnapshot(WAVELET_ID_1, 1, SIG1);
    let committed = HashedVersion.unsigned(0);
    viewListener.onSnapshot(WAVELET_ID_1, snapshotUpdate, committed, null);
    let ch = muxListener.verifyOperationChannelCreated(
      snapshotUpdate,
      Accessibility.READ_WRITE,
    );

    // The second snapshot may have a non-zero version.
    committed = HashedVersion.unsigned(1);
    ch = checkSendClobberingSnapshot(
      viewListener,
      ch,
      snapshotUpdate,
      committed,
    );
    expect(destroyedListeners.length).toBe(1);

    // Repeat the clobbering with a higher version.
    committed = HashedVersion.unsigned(2000);
    checkSendClobberingSnapshot(viewListener, ch, snapshotUpdate, committed);
    expect(destroyedListeners.length).toBe(2);
    expect(destroyedListeners.every((id) => id.equals(WAVELET_ID_1))).toBe(
      true,
    );
  });

  it("receivedSnapshotClobbersOnlyAppropriateChannel", () => {
    const viewChannel = openMux();
    const viewListener = viewChannel.takeListener();

    muxListener.verifyNoMoreInteractions();
    viewListener.onConnected();
    const snapshotUpdate = createSnapshot(WAVELET_ID_1, 1, SIG1);
    let committed = HashedVersion.unsigned(0);
    viewListener.onSnapshot(WAVELET_ID_1, snapshotUpdate, committed, null);
    const ch = muxListener.verifyOperationChannelCreated(
      snapshotUpdate,
      Accessibility.READ_WRITE,
    );

    // This snapshot should clobber wavelet 1's channel
    committed = HashedVersion.of(1, SIG1);
    checkSendClobberingSnapshot(viewListener, ch, snapshotUpdate, committed);
    muxListener.verifyNoMoreInteractions();

    // This snapshot should only create a new channel and nothing else
    viewListener.onSnapshot(WAVELET_ID_2, snapshotUpdate, committed, null);
    muxListener.verifyOperationChannelCreated(
      snapshotUpdate,
      Accessibility.READ_WRITE,
    );
    muxListener.verifyNoMoreInteractions();
  });

  it("opsReceivedAndChannelClobbered", () => {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const serverOps = 1;
    const finalSignature = SIG2;
    const viewChannel = openMux();
    const viewListener = viewChannel.takeListener();

    viewListener.onConnected();
    const ch = connectChannelSnapshot(viewListener, chInfo);
    triggerAndCheckOpenFinished(viewListener);

    // Receive a delta.
    checkReceiveDelta(
      viewListener,
      ch.channel,
      ch.listener,
      WAVELET_ID_1,
      chInfo.initialVersion,
      serverOps,
      finalSignature,
    );

    viewChannel.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();

    // Now receive a snapshot and it should clobber the existing channel
    const committed = HashedVersion.of(1000000, SIG3);
    const update = createSnapshot(
      WAVELET_ID_1,
      committed.version,
      committed.historyHash,
    );
    checkSendClobberingSnapshot(viewListener, ch.channel, update, committed);
  });

  it("opReceivedOnChannel", () => {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const serverOps = 1;
    const finalSignature = SIG2;
    const viewChannel = openMux();
    const viewListener = viewChannel.takeListener();

    viewListener.onConnected();
    const ch = connectChannelSnapshot(viewListener, chInfo);
    triggerAndCheckOpenFinished(viewListener);

    // Receive a delta.
    checkReceiveDelta(
      viewListener,
      ch.channel,
      ch.listener,
      WAVELET_ID_1,
      chInfo.initialVersion,
      serverOps,
      finalSignature,
    );

    viewChannel.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("channelSendSubmitsToView", () => {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const viewChannel = openMux();
    const viewListener = viewChannel.takeListener();

    viewListener.onConnected();
    const ch = connectChannelSnapshot(viewListener, chInfo);
    triggerAndCheckOpenFinished(viewListener);

    // Send an operation and check view submission
    const op = createOp();
    const delta = createDelta(chInfo.initialHashedVersion, op);
    viewChannel.expectSubmitDelta(WAVELET_ID_1, delta);
    ch.channel.send(op);

    viewChannel.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("ackResultsInChannelOp", () => {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const finalSignature = SIG2;
    const view = openMux();
    const viewListener = view.takeListener();

    viewListener.onConnected();
    const ch = connectChannelSnapshot(viewListener, chInfo);
    triggerAndCheckOpenFinished(viewListener);

    checkSendDelta(view, ch.channel, chInfo.initialHashedVersion, WAVELET_ID_1);
    checkAckDelta(view, ch.channel, ch.listener, 1, 2, finalSignature);

    view.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("multipleChannelsAreIndependent", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const chInfo2 = new ConnectionInfo(WAVELET_ID_2, 20, SIG2);

    const view = openMux();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshot.
    const ch1 = connectChannelSnapshot(viewListener, chInfo1);
    const ch2 = connectChannelSnapshot(viewListener, chInfo2);
    triggerAndCheckOpenFinished(viewListener);

    // Check channels receive ops independently.
    const serverOps1 = 5;
    checkReceiveDelta(
      viewListener,
      ch1.channel,
      ch1.listener,
      WAVELET_ID_1,
      chInfo1.initialVersion,
      serverOps1,
      SIG4,
    );
    ch2.listener.checkOpsReceived(0);

    const serverOps2 = 7;
    checkReceiveDelta(
      viewListener,
      ch2.channel,
      ch2.listener,
      WAVELET_ID_2,
      chInfo2.initialVersion,
      serverOps2,
      SIG5,
    );
    ch1.listener.checkOpsReceived(0);

    // Check channels send ops independently.
    checkSendDelta(
      view,
      ch1.channel,
      HashedVersion.of(chInfo1.initialVersion + serverOps1, SIG4),
      WAVELET_ID_1,
    );
    checkSendDelta(
      view,
      ch2.channel,
      HashedVersion.of(chInfo2.initialVersion + serverOps2, SIG5),
      WAVELET_ID_2,
    );

    // Check acks are received independently.
    const ackSignature1 = SIG6;
    const ackSignature2 = SIG7;
    const ackVersion1 = chInfo1.initialVersion + serverOps1 + 1;
    const ackVersion2 = chInfo2.initialVersion + serverOps2 + 1;
    checkAckDelta(
      view,
      ch1.channel,
      ch1.listener,
      1,
      ackVersion1,
      ackSignature1,
    );
    checkAckDelta(
      view,
      ch2.channel,
      ch2.listener,
      1,
      ackVersion2,
      ackSignature2,
    );

    view.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("muxCloseClosesViewAndChannels", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const chInfo2 = new ConnectionInfo(WAVELET_ID_2, 20, SIG2);

    const view = openMux();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    const ch1 = connectChannelSnapshot(viewListener, chInfo1);
    connectChannelSnapshot(viewListener, chInfo2);
    triggerAndCheckOpenFinished(viewListener);

    view.expectClose();
    mux.close();

    // Receive lagging delta from view channel, expect nothing.
    const update = createServerDeltaList(1, 1, SIG4);
    viewListener.onUpdate(chInfo1.waveletId, update, null, null);
    ch1.listener.checkOpsReceived(0);

    view.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("muxFailCallbackThrowsException", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);

    // As in the original, this listener is defined but the mux is opened
    // with the ordinary mock listener.
    const closingListener: OperationChannelMultiplexerListener = {
      onFailed: (_detail: CorruptionDetail) => {
        // Attempt to close the mux *during* the failure callback.
        mux.close();
      },
      onOpenFinished: () => {},
      onOperationChannelCreated: () => {},
      onOperationChannelRemoved: () => {},
    };
    void closingListener;

    const view = viewFactory.expectCreate();
    view.expectOpen(ALL_IDS, noKnownWavelets());
    mux.open(muxListener, ALL_IDS);

    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshot.
    connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    // Fail a channel, which will fail the mux.
    const update = createSnapshot(WAVELET_ID_1, 1, SIG1);
    expectChannelException(
      () => viewListener.onSnapshot(WAVELET_ID_1, update, null, null),
      "Expected exception on bad first message",
    );
  });

  /**
   * Not in the original: what `muxFailCallbackThrowsException` set out to
   * test. The listener closes the mux during the failure callback.
   */
  it("muxCloseDuringFailCallback (not in the original)", () => {
    const calls: string[] = [];
    const closingListener: OperationChannelMultiplexerListener = {
      onFailed: (_detail: CorruptionDetail) => {
        calls.push("onFailed");
        // Attempt to close the mux *during* the failure callback.
        mux.close();
      },
      onOpenFinished: () => calls.push("onOpenFinished"),
      onOperationChannelCreated: () => calls.push("onOperationChannelCreated"),
      onOperationChannelRemoved: () => calls.push("onOperationChannelRemoved"),
    };

    const view = viewFactory.expectCreate();
    view.expectOpen(ALL_IDS, noKnownWavelets());
    mux.open(closingListener, ALL_IDS);

    const viewListener = view.takeListener();
    viewListener.onConnected();
    const snapshot = createSnapshot(WAVELET_ID_1, 1, SIG1);
    viewListener.onSnapshot(
      WAVELET_ID_1,
      snapshot,
      HashedVersion.of(1, SIG1),
      null,
    );
    viewListener.onOpenFinished();

    // A non-recoverable failure shuts the mux down: the view is closed once
    // and the listener is told once.
    view.expectClose();
    viewListener.onException(
      new ChannelException("failed for testing", Recoverable.NOT_RECOVERABLE),
    );
    view.checkExpectationsSatisified();
    expect(calls).toEqual([
      "onOperationChannelCreated",
      "onOpenFinished",
      "onFailed",
    ]);
  });

  /**
   * Tests that the mux ignores known wavelets that don't match the wavelet
   * filter, hence will never receive updates from the server.
   */
  it("openWithKnownWaveletsIgnoresFilteredWavelets", () => {
    const knownVersion = 40;
    const knownSig = SIG1;
    const onlyWavelet1 = IdFilter.ofPrefixes(["w+1"]);
    const knownSnapshot1 = createSnapshot(WAVELET_ID_1, knownVersion, knownSig);
    const knownSnapshot2 = createSnapshot(WAVELET_ID_2, 0, NOSIG);

    const view = viewFactory.expectCreate();
    const expectedSigs = createKnownVersions(
      WAVELET_ID_1,
      knownVersion,
      knownSig,
    );
    view.expectOpen(onlyWavelet1, expectedSigs);

    mux.open(muxListener, onlyWavelet1, [
      createKnownWavelet(
        knownSnapshot1,
        knownVersion,
        knownSig,
        Accessibility.READ_WRITE,
      ),
      createKnownWavelet(knownSnapshot2, 0, NOSIG, Accessibility.READ_WRITE),
    ]);
    view.checkExpectationsSatisified();
  });

  it("openWithKnownWaveletWaitsForReconnection", () => {
    const knownVersion = 40;
    const knownSig = SIG1;
    const knownSnapshot = createSnapshot(WAVELET_ID_1, knownVersion, knownSig);

    const view = openMuxWithKnownWavelet(knownSnapshot);

    // The channel is "connected" though the underlying view isn't.
    const ch = expectConnectedChannel(knownSnapshot, Accessibility.READ_WRITE);
    checkOpenFinished();

    // Attempt to send a client op. Submission should be held until the view
    // connects and the channel receives the empty reconnection delta.
    const clientOp = createOp();
    const delta = createDelta(
      HashedVersion.of(knownVersion, knownSig),
      clientOp,
    );
    ch.channel.send(clientOp);
    ch.listener.checkOpsReceived(0);

    // Connect the underlying view.
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive the reconnection delta, expect the client delta submission.
    view.expectSubmitDelta(WAVELET_ID_1, delta);
    reconnectChannel(viewListener, WAVELET_ID_1, knownVersion, knownSig);
    // Don't expect this empty delta from the channel.
    ch.listener.checkOpsReceived(0);
    expect(ch.channel.receive()).toBeNull();

    checkAckDelta(view, ch.channel, ch.listener, 1, knownVersion + 1, SIG2);

    // No snapshot, but check we can receive and send deltas on the channel.
    checkReceiveAndSend(viewListener, view, ch, WAVELET_ID_1, knownVersion + 1);

    view.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  /**
   * Tests that the mux dies if the server doesn't respond with a
   * reconnection version for every reconnecting wavelet.
   */
  it("openWithKnownWaveletsFailsIfServerDoesntKnow", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const chInfo2 = new ConnectionInfo(WAVELET_ID_2, 20, SIG2);

    const expectedSigs = createKnownVersions(
      WAVELET_ID_1,
      chInfo1.initialVersion,
      chInfo1.initialSignature,
      WAVELET_ID_2,
      chInfo2.initialVersion,
      chInfo2.initialSignature,
    );
    const knownWavelets = [
      createKnownWavelet(
        chInfo1.snapshot,
        chInfo1.initialVersion,
        chInfo1.initialSignature,
        Accessibility.READ_WRITE,
      ),
      createKnownWavelet(
        chInfo2.snapshot,
        chInfo2.initialVersion,
        chInfo2.initialSignature,
        Accessibility.READ_WRITE,
      ),
    ];

    // Connect to the server.
    const view = viewFactory.expectCreate();
    view.expectOpen(ALL_IDS, expectedSigs);
    mux.open(muxListener, ALL_IDS, knownWavelets);

    // The mux appears "connected" though the underlying view isn't.
    expectConnectedChannel(chInfo1.snapshot, Accessibility.READ_WRITE);
    expectConnectedChannel(chInfo2.snapshot, Accessibility.READ_WRITE);
    checkOpenFinished();

    // Connect the underlying view.
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive one reconnection delta.
    const ch1reconnect = createServerDeltaList(
      chInfo1.initialVersion,
      0,
      chInfo1.initialSignature,
    );
    viewListener.onUpdate(
      WAVELET_ID_1,
      ch1reconnect,
      HashedVersion.of(chInfo1.initialVersion, chInfo1.initialSignature),
      null,
    );

    // Receive openFinished before reconnection delta for second wavelet,
    // expect turbulence.
    expectChannelException(
      () => viewListener.onOpenFinished(),
      "Expected a channel exception",
    );
  });

  /**
   * Tests that the mux survives if the server doesn't respond with a
   * reconnection version for an inaccessible wavelet.
   */
  it("openWithKnownWaveletsSucceedsIfServerDoesntKnowInaccessibleWavelet", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const chInfo2 = new ConnectionInfo(WAVELET_ID_2, 20, SIG2);

    // Only the accessible wavelet will resync.
    const expectedSigs = createKnownVersions(
      WAVELET_ID_1,
      chInfo1.initialVersion,
      chInfo1.initialSignature,
    );
    const knownWavelets = [
      createKnownWavelet(
        chInfo1.snapshot,
        chInfo1.initialVersion,
        chInfo1.initialSignature,
        Accessibility.READ_WRITE,
      ),
      createKnownWavelet(
        chInfo2.snapshot,
        chInfo2.initialVersion,
        chInfo2.initialSignature,
        Accessibility.INACCESSIBLE,
      ),
    ];

    // Connect to the server.
    const view = viewFactory.expectCreate();
    view.expectOpen(ALL_IDS, expectedSigs);
    mux.open(muxListener, ALL_IDS, knownWavelets);

    // The mux appears "connected" though the underlying view isn't.
    expectConnectedChannel(chInfo1.snapshot, Accessibility.READ_WRITE);
    const ch2 = expectConnectedChannel(
      chInfo2.snapshot,
      Accessibility.INACCESSIBLE,
    );
    checkOpenFinished();

    // Connect the underlying view.
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive a reconnection delta for the accessible wavelet.
    const ch1reconnect = createServerDeltaList(
      chInfo1.initialVersion,
      0,
      chInfo1.initialSignature,
    );
    viewListener.onUpdate(
      WAVELET_ID_1,
      ch1reconnect,
      HashedVersion.of(chInfo1.initialVersion, chInfo1.initialSignature),
      null,
    );

    // Receive openFinished in the absence of a reconnection delta for the
    // inaccessible wavelet, expect bliss.
    viewListener.onOpenFinished();

    // Check sending ops to the disconnected wavelet fails.
    const op = createOp();
    expectChannelException(
      () => ch2.channel.send(op),
      "Expected a channel exception",
    );

    view.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("newWaveletSuppressesSnapshot", () => {
    const view = openMux();
    const viewListener = view.takeListener();
    viewListener.onConnected();
    triggerAndCheckOpenFinished(viewListener);

    const ch = createOperationChannel(WAVELET_ID_1, USER_ID);

    // Send and ack first op.
    checkSendDelta(view, ch.channel, HashedVersion.unsigned(0), WAVELET_ID_1);
    view.checkExpectationsSatisified();
    checkAckDelta(view, ch.channel, ch.listener, 1, 1, SIG1);

    // Drop the empty snapshot sent by the server.
    const snapshot = createSnapshot(WAVELET_ID_1, 0, NOSIG);
    const committed = HashedVersion.unsigned(0);
    viewListener.onSnapshot(snapshot.waveletId, snapshot, committed, null);
    muxListener.verifyNoMoreInteractions();

    // Now CC should have sent the first client delta so it's acked.
    view.checkExpectationsSatisified();

    checkReceiveAndSend(viewListener, view, ch, WAVELET_ID_1, 1);

    view.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("muxReconnectsAfterDisconnect", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const chInfo2 = new ConnectionInfo(WAVELET_ID_2, 20, SIG2);

    const view = openMux();
    muxListener.verifyNoMoreInteractions();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    const ch1 = connectChannelSnapshot(viewListener, chInfo1);
    const ch2 = connectChannelSnapshot(viewListener, chInfo2);
    triggerAndCheckOpenFinished(viewListener);

    view.expectClose();
    const view2 = viewFactory.expectCreate();
    view2.expectOpen(
      ALL_IDS,
      createKnownVersions(WAVELET_ID_1, 1, SIG1, WAVELET_ID_2, 20, SIG2),
    );

    viewListener.onException(
      new ChannelException("failed for testing", Recoverable.RECOVERABLE),
    );

    const viewListener2 = reconnectView(view2, chInfo1, chInfo2);

    // Don't expect the mux nor any channel to know about reconnection.
    muxListener.verifyNoMoreInteractions();
    ch1.listener.checkOpsReceived(0);
    ch2.listener.checkOpsReceived(0);

    // Check we can still receive and send deltas.
    checkReceiveAndSend(
      viewListener2,
      view2,
      ch1,
      WAVELET_ID_1,
      chInfo1.initialVersion,
    );
    checkReceiveAndSend(
      viewListener2,
      view2,
      ch2,
      WAVELET_ID_2,
      chInfo2.initialVersion,
    );

    // If a message is received on the old view it should be ignored
    viewListener.onUpdate(
      WAVELET_ID_1,
      createServerDeltaList(1, 1, SIG3),
      null,
      null,
    );

    view.checkExpectationsSatisified();
    view2.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  it("muxRewindsAChannelAndReconnectsAtTheFork (not in the original)", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const view = openMux();
    const viewListener = view.takeListener();
    viewListener.onConnected();
    const ch1 = connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);
    const rewound = createServerDeltaList(1, 2, SIG2);
    viewListener.onUpdate(WAVELET_ID_1, rewound, null, null);
    ch1.listener.checkOpsReceived(1);
    ch1.listener.clear();
    while (ch1.channel.receive() !== null) {
      // The two server ops.
    }

    // The history goes back to version 1: the two ops are taken back and
    // the view reopens at version 1.
    view.expectClose();
    const view2 = viewFactory.expectCreate();
    view2.expectOpen(ALL_IDS, createKnownVersions(WAVELET_ID_1, 1, SIG1));
    viewListener.onReset!(WAVELET_ID_1, HashedVersion.of(1, SIG1), rewound);
    ch1.listener.checkOpsReceived(1);
    ch1.listener.clear();
    const inverse = [ch1.channel.receive(), ch1.channel.receive()];
    expect(inverse.map((op) => op?.context.versionIncrement)).toEqual([-1, -1]);
    expect(inverse[1]?.context.hashedVersion).toEqual(
      HashedVersion.of(1, SIG1),
    );
    expect(ch1.channel.receive()).toBeNull();

    const viewListener2 = reconnectView(view2, chInfo1);
    checkReceiveAndSend(
      viewListener2,
      view2,
      ch1,
      WAVELET_ID_1,
      chInfo1.initialVersion,
    );
    view.checkExpectationsSatisified();
    view2.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });

  /**
   * Helps test that a mux reconnects with known wavelets if the view
   * channel fails during reconnection.
   */
  function doTestMuxReconnectsKnownWavelet(
    when: KnownWaveletDisconnectWhen,
  ): void {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const view = openMuxWithKnownWavelet(chInfo.snapshot);
    const ch = expectConnectedChannel(
      chInfo.snapshot,
      Accessibility.READ_WRITE,
    );
    checkOpenFinished();

    const viewListener = view.takeListener();
    if (atLeast(KNOWN_WAVELET_DISCONNECT_ORDER, when, "AFTER_VIEW_CONNECTED")) {
      viewListener.onConnected();
    }

    if (atLeast(KNOWN_WAVELET_DISCONNECT_ORDER, when, "AFTER_RESYNC")) {
      // Receive reconnection delta and open finished.
      reconnectChannel(
        viewListener,
        WAVELET_ID_1,
        chInfo.initialVersion,
        chInfo.initialSignature,
      );
      viewListener.onOpenFinished();
      view.checkExpectationsSatisified();
    }

    // Fail view, expect reconnection.
    const view2 = failViewAndExpectReconnection(
      viewListener,
      view,
      "View failed after resync message",
      createKnownVersions(WAVELET_ID_1, 1, SIG1),
    );
    reconnectViewAndCheckEverythingStillWorks(view2, chInfo, ch);
    muxListener.verifyNoMoreInteractions();
  }

  /**
   * Tests that a mux reconnects with known wavelets if the view channel
   * fails before the channel id message is received.
   */
  it("muxReconnectsKnownWaveletBeforeViewConnected", () => {
    doTestMuxReconnectsKnownWavelet("BEFORE_VIEW_CONNECTED");
  });

  /**
   * Tests that a mux reconnects with known wavelets if the view channel
   * fails after the channel id but before the reconnection delta is
   * received.
   */
  it("muxReconnectsKnownWaveletAfterViewConnected", () => {
    doTestMuxReconnectsKnownWavelet("AFTER_VIEW_CONNECTED");
  });

  /**
   * Tests that a mux reconnects with known wavelets if the view channel
   * fails after the reconnection delta is received, but before any other
   * deltas.
   */
  it("muxReconnectsKnownWaveletAfterResync", () => {
    doTestMuxReconnectsKnownWavelet("AFTER_RESYNC");
  });

  /**
   * Helps test that the mux reconnects at version zero for a
   * locally-created channel.
   */
  function doTestMuxReconnectsNewWavelet(when: NewWaveletDisconnectWhen): void {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 0, NOSIG);
    const deltaSig = SIG1;
    const view = openMux();
    const viewListener = view.takeListener();

    if (atLeast(NEW_WAVELET_DISCONNECT_ORDER, when, "AFTER_VIEW_CONNECTED")) {
      viewListener.onConnected();
      triggerAndCheckOpenFinished(viewListener);
    }

    const ch = createOperationChannel(WAVELET_ID_1, USER_ID);

    let clientDelta: WaveletDelta | null = null;
    if (atLeast(NEW_WAVELET_DISCONNECT_ORDER, when, "AFTER_SEND_DELTA")) {
      clientDelta = checkSendDelta(
        view,
        ch.channel,
        chInfo.initialHashedVersion,
        chInfo.waveletId,
      );
    }
    if (atLeast(NEW_WAVELET_DISCONNECT_ORDER, when, "AFTER_ACK_DELTA")) {
      checkAckDelta(
        view,
        ch.channel,
        ch.listener,
        1,
        chInfo.initialVersion + 1,
        deltaSig,
      );
    }

    // Fail the view.
    // Zero is always a resync version, even if no delta was submitted.
    const resyncVersions = [HashedVersion.unsigned(0)];
    if (atLeast(NEW_WAVELET_DISCONNECT_ORDER, when, "AFTER_ACK_DELTA")) {
      resyncVersions.push(HashedVersion.of(1, deltaSig));
    }
    const expectedReconnectVersions: KnownWaveletVersions = new WaveletIdMap();
    expectedReconnectVersions.set(chInfo.waveletId, resyncVersions);
    const view2 = failViewAndExpectReconnection(
      viewListener,
      view,
      "View failed for testing",
      expectedReconnectVersions,
    );

    // Reconnect the view with no channels - the server never knew about or
    // lost the wavelet.
    const viewListener2 = view2.takeListener();
    viewListener2.onConnected();
    if (clientDelta !== null) {
      // Expect the client delta to be resubmitted.
      view2.expectSubmitDelta(chInfo.waveletId, clientDelta);
    }
    viewListener2.onOpenFinished();
    view2.checkExpectationsSatisified();

    if (atLeast(NEW_WAVELET_DISCONNECT_ORDER, when, "AFTER_SEND_DELTA")) {
      // (Re-)ack the resubmitted delta and check the channel works.
      checkAckDelta(
        view2,
        ch.channel,
        ch.listener,
        1,
        chInfo.initialVersion + 1,
        deltaSig,
      );
      checkReceiveAndSend(
        viewListener2,
        view2,
        ch,
        chInfo.waveletId,
        chInfo.initialVersion + 1,
      );
    } else {
      // Check we can send on the channel.
      checkReceiveAndSend(
        viewListener2,
        view2,
        ch,
        chInfo.waveletId,
        chInfo.initialVersion,
      );
    }
  }

  /**
   * Tests that a mux reconnects with a new version-zero channel if the view
   * channel fails before it opens.
   */
  it("muxReconnectsNewWaveletBeforeViewConnected", () => {
    doTestMuxReconnectsNewWavelet("BEFORE_VIEW_CONNECTED");
  });

  /**
   * Tests that the mux reconnects at version zero when the server fails
   * before the client sends any ops on a wavelet.
   */
  it("muxReconnectsNewWaveletAfterViewConnected", () => {
    doTestMuxReconnectsNewWavelet("AFTER_VIEW_CONNECTED");
  });

  /**
   * Tests that the mux reconnects at version zero when the server fails and
   * loses the client's version-zero delta before acknowledging it,
   * forgetting the wavelet entirely.
   */
  it("muxReconnectsNewWaveletAfterFirstSend", () => {
    doTestMuxReconnectsNewWavelet("AFTER_SEND_DELTA");
  });

  /**
   * Tests that the mux reconnects at version zero when the server fails and
   * loses the client's version-zero delta, forgetting the wavelet entirely.
   */
  it("muxReconnectsNewWaveletWhenServerLosesFirstDelta", () => {
    doTestMuxReconnectsNewWavelet("AFTER_ACK_DELTA");
  });

  function muxWithScheduler(
    scheduler: Scheduler,
  ): OperationChannelMultiplexerImpl {
    return new OperationChannelMultiplexerImpl(
      WAVE_ID,
      viewFactory,
      createEmptyWaveletSnapshot,
      null,
      null,
      scheduler,
      FAKE_HASHED_VERSION_FACTORY,
    );
  }

  it("muxReconnectUsingScheduler", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const scheduler = new FakeScheduler();

    mux = muxWithScheduler(scheduler);

    const view = openMux();
    muxListener.verifyNoMoreInteractions();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    const ch = connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    // Send but don't ack delta.
    const delta = checkSendDelta(
      view,
      ch.channel,
      chInfo1.initialHashedVersion,
      WAVELET_ID_1,
    );

    // No schedule yet
    expect(scheduler.command).toBeNull();

    // Reconnect channel.
    view.expectClose();
    viewListener.onClosed();

    // Check we've scheduled something.
    expect(scheduler.command).not.toBeNull();

    // scheduler call back
    const view2 = viewFactory.expectCreate();
    view2.expectOpen(ALL_IDS, createKnownVersions(WAVELET_ID_1, 1, SIG1));
    scheduler.command!.execute();

    // Signal reconnected
    const viewListener2 = view2.takeListener();
    viewListener2.onConnected();

    // Expect retransmit of the sent delta after reconnect.
    view2.expectSubmitDelta(WAVELET_ID_1, delta);
    reconnectChannel(
      viewListener2,
      WAVELET_ID_1,
      chInfo1.initialVersion,
      chInfo1.initialSignature,
    );
    viewListener2.onOpenFinished();
    view.checkExpectationsSatisified();
    view2.checkExpectationsSatisified();
  });

  /**
   * Not in the original: a reconnection scheduled before the mux is closed
   * must not open another view when it eventually runs.
   */
  it("scheduledReconnectIsDroppedAfterClose (not in the original)", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const scheduler = new FakeScheduler();
    mux = muxWithScheduler(scheduler);

    const view = openMux();
    const viewListener = view.takeListener();
    viewListener.onConnected();
    connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    view.expectClose();
    viewListener.onClosed();
    expect(scheduler.command).not.toBeNull();

    // Closing the mux closes the (already closed) view again.
    view.expectClose();
    mux.close();
    view.checkExpectationsSatisified();

    // No view creation is expected: the factory fails the test on one.
    scheduler.command!.execute();
    muxListener.verifyNoMoreInteractions();
  });

  it("muxReconnectUsingScheduleResetWithDelta", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);
    const scheduler = new FakeScheduler();
    let resets = 0;
    scheduler.reset = () => {
      resets++;
    };

    mux = muxWithScheduler(scheduler);

    let view = openMux();
    muxListener.verifyNoMoreInteractions();
    let viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    // No schedule yet
    expect(scheduler.command).toBeNull();
    // A snapshot does not reset the scheduler.
    expect(resets).toBe(0);

    // Reconnect 20 times.
    for (let i = 0; i < 20; i++) {
      // Reconnect channel.
      view.expectClose();
      viewListener.onClosed();
      view.checkExpectationsSatisified();

      // scheduler call back
      view = viewFactory.expectCreate();
      view.expectOpen(ALL_IDS, createKnownVersions(WAVELET_ID_1, 1, SIG1));
      expect(scheduler.command).not.toBeNull();
      scheduler.command!.execute();
      scheduler.command = null;

      viewListener = view.takeListener();
      viewListener.onConnected();

      // Reconnection message
      reconnectChannel(
        viewListener,
        WAVELET_ID_1,
        chInfo1.initialVersion,
        chInfo1.initialSignature,
      );
      // The reconnection delta resets the scheduler.
      expect(resets).toBe(i + 1);
      viewListener.onOpenFinished();
    }
  });

  it("muxReconnectsAfterDisconnectWithOutstandingSubmit", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);

    const view = openMux();
    muxListener.verifyNoMoreInteractions();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    const ch = connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    // Send but don't ack delta.
    const delta = checkSendDelta(
      view,
      ch.channel,
      chInfo1.initialHashedVersion,
      WAVELET_ID_1,
    );

    // Reconnect channel.
    const view2 = failViewAndExpectReconnection(
      viewListener,
      view,
      "View failed with outstanding submit",
      createKnownVersions(WAVELET_ID_1, 1, SIG1),
    );

    // Expect retransmit of the sent delta after reconnect.
    view2.expectSubmitDelta(WAVELET_ID_1, delta);
    const viewListener2 = reconnectView(view2, chInfo1);
    muxListener.verifyNoMoreInteractions(); // No callback on reconnection.

    checkAckDelta(
      view2,
      ch.channel,
      ch.listener,
      1,
      chInfo1.initialVersion + 1,
      SIG1,
    );
    checkReceiveAndSend(
      viewListener2,
      view2,
      ch,
      WAVELET_ID_1,
      chInfo1.initialVersion + 1,
    );

    // If the submit is then acked, it should be ignored.
    view.ackSubmit(1, chInfo1.initialVersion + 1, SIG5);

    view.checkExpectationsSatisified();
    view2.checkExpectationsSatisified();
    ch.listener.checkOpsReceived(0);
    muxListener.verifyNoMoreInteractions();
  });

  it("muxReconnectsAfterSubmitFailure", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);

    const view = openMux();
    muxListener.verifyNoMoreInteractions();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    const ch = connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    // Send but don't ack delta.
    const delta = checkSendDelta(
      view,
      ch.channel,
      chInfo1.initialHashedVersion,
      WAVELET_ID_1,
    );

    // Fail the submission, expecting reconnection.
    const view2 = failViewAndExpectReconnection(
      viewListener,
      view,
      "View failed with outstanding submit",
      createKnownVersions(WAVELET_ID_1, 1, SIG1),
    );

    // Expect retransmit of the sent delta after reconnect.
    view2.expectSubmitDelta(WAVELET_ID_1, delta);
    const viewListener2 = reconnectView(view2, chInfo1);
    muxListener.verifyNoMoreInteractions(); // No callback on reconnection.

    checkAckDelta(
      view2,
      ch.channel,
      ch.listener,
      1,
      chInfo1.initialVersion + 1,
      SIG2,
    );

    // If the first view later disconnects, it should be ignored
    viewListener.onException(
      new ChannelException("failed for testing", Recoverable.RECOVERABLE),
    );

    checkReceiveAndSend(
      viewListener2,
      view2,
      ch,
      WAVELET_ID_1,
      chInfo1.initialVersion + 1,
    );

    view.checkExpectationsSatisified();
    view2.checkExpectationsSatisified();
    ch.listener.checkOpsReceived(0);
    muxListener.verifyNoMoreInteractions();
  });

  it("muxFailsAfterChannelCorrupt", () => {
    const chInfo1 = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);

    const view = openMux();
    muxListener.verifyNoMoreInteractions();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    connectChannelSnapshot(viewListener, chInfo1);
    triggerAndCheckOpenFinished(viewListener);

    // Receive a message that should not be received.
    const update = createSnapshot(WAVELET_ID_1, 1, SIG1);
    // As in the original, the close expected here is not checked: the mux
    // only closes once the view channel reports the exception to it.
    view.expectClose();
    const e = expectChannelException(
      () => viewListener.onSnapshot(WAVELET_ID_1, update, null, null),
      "Expected exception corruption",
    );
    expect(e.recoverable).toBe(Recoverable.NOT_RECOVERABLE);
    expect(e.waveId !== null && e.waveId.equals(WAVE_ID)).toBe(true);
    expect(e.waveletId !== null && e.waveletId.equals(WAVELET_ID_1)).toBe(true);
  });

  it("muxReconnectsAgainAfterReconnectFailure", () => {
    const chInfo = new ConnectionInfo(WAVELET_ID_1, 1, SIG1);

    const view = openMux();
    muxListener.verifyNoMoreInteractions();
    const viewListener = view.takeListener();
    viewListener.onConnected();

    // Receive initial snapshots.
    connectChannelSnapshot(viewListener, chInfo);
    triggerAndCheckOpenFinished(viewListener);

    // Disconnect, expect reconnection.
    view.expectClose();
    const view2 = viewFactory.expectCreate();
    view2.expectOpen(ALL_IDS, createKnownVersions(WAVELET_ID_1, 1, SIG1));
    viewListener.onClosed();
    const viewListener2 = view2.takeListener();

    // Disconnect, expect reconnection again.
    view2.expectClose();
    const view3 = viewFactory.expectCreate();
    view3.expectOpen(ALL_IDS, createKnownVersions(WAVELET_ID_1, 1, SIG1));
    viewListener2.onClosed();

    view2.checkExpectationsSatisified();
    view3.checkExpectationsSatisified();
    muxListener.verifyNoMoreInteractions();
  });
});
