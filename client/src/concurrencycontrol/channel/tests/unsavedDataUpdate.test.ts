// Tests that the CC stack provides correct unsaved data info.
//
// Ported from `UnsavedDataUpdateTest` of Apache Wave (Apache License,
// Version 2.0).

import { beforeEach, describe, expect, it } from "vitest";

import { IdFilter, ParticipantId, WaveId, WaveletId } from "../../../model/id";
import { WaveletOperation, WaveletOperationContext } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { ResponseCode, type UnsavedDataInfo, type UnsavedDataListener } from "../../common";
import { FakeWaveViewServiceUpdate, MockWaveViewService } from "../../testing";
import {
  Accessibility,
  ImmediateExecutionScheduler,
  type OperationChannel,
  OperationChannelMultiplexerImpl,
  type ViewChannelFactory,
  ViewChannelImpl,
  createEmptyWaveletSnapshot,
} from "..";
import { FAKE_HASHED_VERSION_FACTORY, bytes, emptySnapshot } from "./helpers";
import { MockMuxListener } from "./mockMuxListener";
import { MockOperationChannelListener } from "./mockOperationChannelListener";

class FakeUnsavedDataListener implements UnsavedDataListener {
  unacknowledgedSize = 0;
  uncommittedSize = 0;
  inFlight = 0;
  closeCalls = 0;
  lastAckVersion = 0;
  lastCommitVersion = 0;

  onUpdate(unsavedDataInfo: UnsavedDataInfo): void {
    this.unacknowledgedSize = unsavedDataInfo.estimateUnacknowledgedSize();
    this.uncommittedSize = unsavedDataInfo.estimateUncommittedSize();
    this.inFlight = unsavedDataInfo.inFlightSize();
    this.lastAckVersion = unsavedDataInfo.lastAckVersion();
    this.lastCommitVersion = unsavedDataInfo.lastCommitVersion();
  }

  onClose(_everythingCommitted: boolean): void {
    ++this.closeCalls;
  }
}

/** ID of wavelets for testing. */
const WAVELET_ID_1 = WaveletId.of("example.com", "conv+1");
const WAVELET_ID_2 = WaveletId.of("example.com", "conv+2");

const SIGNATURE1 = bytes(1, 1, 1, 1);
const SIGNATURE2 = bytes(2, 2, 2, 2);
const SIGNATURE3 = bytes(3, 3, 3, 3);

/** ID of wave for testing. */
const WAVE_ID = WaveId.of("example.com", "w+1");

/** Channel Id for testing. */
const CHANNEL_ID = "channelId_1";

/** User name for testing. */
const USER_NAME = ParticipantId.ofUnsafe("example@example.com");

const ALL_IDS = IdFilter.allIds();

describe("UnsavedDataUpdate", () => {
  let mux: OperationChannelMultiplexerImpl;
  let viewFactory: ViewChannelFactory;
  let waveViewService: MockWaveViewService;
  let muxListener: MockMuxListener;
  let fakeUnsavedDataListener: FakeUnsavedDataListener;

  beforeEach(() => {
    ViewChannelImpl.resetViewChannelsPerWave();
    ViewChannelImpl.setMaxViewChannelsPerWave(Number.MAX_SAFE_INTEGER);
    waveViewService = new MockWaveViewService();
    viewFactory = ViewChannelImpl.factory(waveViewService);
    fakeUnsavedDataListener = new FakeUnsavedDataListener();
    mux = new OperationChannelMultiplexerImpl(
      WAVE_ID,
      viewFactory,
      createEmptyWaveletSnapshot,
      null,
      {
        create: () => fakeUnsavedDataListener,
        destroy: () => {},
      },
      new ImmediateExecutionScheduler(),
      FAKE_HASHED_VERSION_FACTORY,
    );
    muxListener = new MockMuxListener();
  });

  function createAddParticipantOp(participantName = "thedude@google.com"): WaveletOperation {
    return WaveletOperation.addParticipant(
      new WaveletOperationContext(USER_NAME, -1, 0),
      ParticipantId.ofUnsafe(participantName),
    );
  }

  function createOperationChannel(waveletId: WaveletId, address: ParticipantId): OperationChannel {
    mux.createOperationChannel(waveletId, address);
    return muxListener.verifyOperationChannelCreated(
      emptySnapshot(
        WAVE_ID,
        waveletId,
        address,
        HashedVersion.unsigned(0),
        1273307837000 /* arbitrary time */,
      ),
      Accessibility.READ_WRITE,
    );
  }

  function assertUnsavedDataInfo(
    unacknowledged: number,
    uncommitted: number,
    inFlight: number,
    lastAckVersion: number,
    lastCommitVersion: number,
  ): void {
    expect({
      unacknowledged: fakeUnsavedDataListener.unacknowledgedSize,
      uncommitted: fakeUnsavedDataListener.uncommittedSize,
      inFlight: fakeUnsavedDataListener.inFlight,
      lastAckVersion: fakeUnsavedDataListener.lastAckVersion,
      lastCommitVersion: fakeUnsavedDataListener.lastCommitVersion,
    }).toEqual({ unacknowledged, uncommitted, inFlight, lastAckVersion, lastCommitVersion });
  }

  /** Create a wave, send Op which is unacked and expect unsaved data. */
  it("creatingWave", () => {
    // Connect to the server
    mux.open(muxListener, ALL_IDS);
    muxListener.verifyNoMoreInteractions();
    const channel = createOperationChannel(WAVELET_ID_1, USER_NAME);
    channel.send(createAddParticipantOp());

    // Expect the delta channel to not be ready, so we expect an open call,
    // but no success on open and no submit call going through.
    expect(waveViewService.opens.length).toBe(1);
    expect(waveViewService.submits.length).toBe(0);
    muxListener.verifyNoMoreInteractions();

    // We do expect unsaved data.
    assertUnsavedDataInfo(1, 1, 0, 0, 0);

    // Now bail out of the connection with an op pending, should close.
    mux.close();
    expect(fakeUnsavedDataListener.closeCalls).toBe(1);
  });

  /**
   * Open a wave; send an Op; wait for the ack; wait for the commit with no
   * outstanding ops.
   */
  it("commitNotification", () => {
    // Connect to the server
    mux.open(muxListener, ALL_IDS);
    expect(waveViewService.opens.length).toBe(1);
    const openCallback = waveViewService.lastOpen().callback;

    // Pretend we got the initial snapshot at version 1 signature1.
    muxListener.verifyNoMoreInteractions();
    openCallback.onUpdate(new FakeWaveViewServiceUpdate().setChannelId(CHANNEL_ID));
    const update = new FakeWaveViewServiceUpdate()
      .setWaveletId(WAVELET_ID_1)
      .setWaveletSnapshot(WAVE_ID, USER_NAME, 0, HashedVersion.of(1, SIGNATURE1))
      .setLastCommittedVersion(HashedVersion.unsigned(0));
    openCallback.onUpdate(update);
    const channel = muxListener.verifyOperationChannelCreated(
      update.getWaveletSnapshot()!,
      Accessibility.READ_WRITE,
    );
    openCallback.onUpdate(new FakeWaveViewServiceUpdate().setMarker(false));
    muxListener.verifyOpenFinished();

    channel.send(createAddParticipantOp());
    expect(waveViewService.submits.length).toBe(1);
    assertUnsavedDataInfo(1, 1, 1, 0, 0);

    // Server ack's the previous add participant op.
    const v2 = HashedVersion.of(2, SIGNATURE2);
    const submitCallback = waveViewService.lastSubmit().callback;
    submitCallback.onSuccess(v2, 1, null, ResponseCode.OK);
    assertUnsavedDataInfo(0, 1, 0, 2, 0);

    // Server sends commit for the add participant op.
    openCallback.onUpdate(
      new FakeWaveViewServiceUpdate().setWaveletId(WAVELET_ID_1).setLastCommittedVersion(v2),
    );

    // Now we expect to be told everything is committed.
    assertUnsavedDataInfo(0, 0, 0, 2, 2);

    // Send another op, now it should be uncommitted.
    channel.send(createAddParticipantOp());
    expect(waveViewService.submits.length).toBe(2);
    assertUnsavedDataInfo(1, 1, 1, 2, 2);

    // Now bail out of the connection with an op pending, should close.
    mux.close();
    expect(waveViewService.closes.length).toBe(1);
    waveViewService.lastClose().callback.onSuccess();
    expect(fakeUnsavedDataListener.closeCalls).toBe(1);
    muxListener.verifyNoMoreInteractions();
  });

  /**
   * Open a wave; send an Op; wait for the ack; send another Op; get ack;
   * get LCV for first ack; get a LCV for second ack.
   */
  it("lastCommittedVersionUpdate", () => {
    // Connect to the server
    mux.open(muxListener, ALL_IDS);
    expect(waveViewService.opens.length).toBe(1);
    const openCallback = waveViewService.lastOpen().callback;

    // Pretend we got the initial snapshot at version 1 signature1.
    muxListener.verifyNoMoreInteractions();
    openCallback.onUpdate(new FakeWaveViewServiceUpdate().setChannelId(CHANNEL_ID));
    const update = new FakeWaveViewServiceUpdate()
      .setWaveletId(WAVELET_ID_1)
      .setWaveletSnapshot(WAVE_ID, USER_NAME, 0, HashedVersion.of(1, SIGNATURE1))
      .setLastCommittedVersion(HashedVersion.unsigned(0));
    openCallback.onUpdate(update);
    const channel = muxListener.verifyOperationChannelCreated(
      update.getWaveletSnapshot()!,
      Accessibility.READ_WRITE,
    );
    openCallback.onUpdate(new FakeWaveViewServiceUpdate().setMarker(false));
    muxListener.verifyOpenFinished();

    // Send Op, get ack.
    channel.send(createAddParticipantOp());
    expect(waveViewService.submits.length).toBe(1);
    const submitCallback1 = waveViewService.lastSubmit().callback;
    const v2 = HashedVersion.of(2, SIGNATURE2);
    submitCallback1.onSuccess(v2, 1, null, ResponseCode.OK);
    assertUnsavedDataInfo(0, 1, 0, 2, 0);

    // Send another Op, get ack.
    channel.send(createAddParticipantOp());
    expect(waveViewService.submits.length).toBe(2);
    const submitCallback2 = waveViewService.lastSubmit().callback;
    const v3 = HashedVersion.of(3, SIGNATURE3);
    submitCallback2.onSuccess(v3, 1, null, ResponseCode.OK);
    assertUnsavedDataInfo(0, 2, 0, 3, 0);

    // Server sends commit for the first addParticipant op.
    openCallback.onUpdate(
      new FakeWaveViewServiceUpdate().setWaveletId(WAVELET_ID_1).setLastCommittedVersion(v2),
    );

    // Now we expect to be told that NOT everything is committed.
    assertUnsavedDataInfo(0, 1, 0, 3, 2);

    // Server sends commit for the second addParticipant op.
    openCallback.onUpdate(
      new FakeWaveViewServiceUpdate().setWaveletId(WAVELET_ID_1).setLastCommittedVersion(v3),
    );

    // Now we expect to be told that everything is committed.
    assertUnsavedDataInfo(0, 0, 0, 3, 3);

    mux.close();
    expect(waveViewService.closes.length).toBe(1);
    waveViewService.lastClose().callback.onSuccess();
    expect(fakeUnsavedDataListener.closeCalls).toBe(1);
    muxListener.verifyNoMoreInteractions();
  });

  it("muxCloseClosesAllUnsavedDataUpdaters", () => {
    mux.open(muxListener, ALL_IDS);
    const listener1 = new MockOperationChannelListener();
    const listener2 = new MockOperationChannelListener();
    const ch1 = createOperationChannel(WAVELET_ID_1, USER_NAME);
    const ch2 = createOperationChannel(WAVELET_ID_2, USER_NAME);
    ch1.setListener(listener1);
    ch2.setListener(listener2);
    mux.close();
    expect(fakeUnsavedDataListener.closeCalls).toBe(2);
  });
});
