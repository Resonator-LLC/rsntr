// Smoke tests for the fakes and mocks of this package (the original has no
// tests for them).

import { describe, expect, it } from "vitest";

import { WaveletData } from "../../../model/data";
import { IdFilter, ParticipantId, WaveId, WaveletId, WaveletName } from "../../../model/id";
import {
  WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
} from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { Accessibility, KnownWavelet } from "../../channel";
import {
  FakeClock,
  FakeOperationChannel,
  FakeOperationChannelMultiplexer,
  FakeWaveViewServiceUpdate,
  MockExpectationError,
  MockOperationChannel,
  MockWaveViewService,
  deltasAreEqual,
} from "..";

const USER = ParticipantId.ofUnsafe("user@example.com");
const OTHER = ParticipantId.ofUnsafe("other@example.com");
const WAVE_ID = WaveId.of("example.com", "w+1");
const WAVELET_ID = WaveletId.of("example.com", "conv+root");

function noOp(): WaveletOperation {
  return WaveletOperation.noOp(new WaveletOperationContext(USER, 0, 1));
}

function addParticipant(): WaveletOperation {
  return WaveletOperation.addParticipant(new WaveletOperationContext(USER, 0, 1), OTHER);
}

describe("concurrencycontrol testing", () => {
  it("deltasAreEqual compares versions and ops", () => {
    const v1 = HashedVersion.unsigned(1);
    const a = new WaveletDelta(USER, v1, [noOp()]);
    expect(deltasAreEqual(a, a)).toBe(true);
    expect(deltasAreEqual(a, null)).toBe(false);
    expect(deltasAreEqual(null, null)).toBe(true);
    expect(deltasAreEqual(a, new WaveletDelta(OTHER, HashedVersion.unsigned(1), [noOp()]))).toBe(
      true,
    );
    expect(deltasAreEqual(a, new WaveletDelta(USER, HashedVersion.unsigned(2), [noOp()]))).toBe(
      false,
    );
    expect(deltasAreEqual(a, new WaveletDelta(USER, v1, [addParticipant()]))).toBe(false);
    expect(deltasAreEqual(a, new WaveletDelta(USER, v1, [noOp(), noOp()]))).toBe(false);
  });

  it("FakeClock returns the set time", () => {
    const clock = new FakeClock(5);
    expect(clock.getCurrentTime()).toBe(5);
    clock.setCurrentTime(7);
    expect(clock.getCurrentTime()).toBe(7);
  });

  it("MockOperationChannel checks its expectations in order", () => {
    const channel = new MockOperationChannel();
    const op = noOp();
    channel.expectSend(op);
    channel.expectPeek(op);
    channel.expectReceive(op);
    channel.expectReceive(null);
    channel.send(noOp());
    expect(channel.peek()).toBe(op);
    expect(channel.receive()).toBe(op);
    expect(channel.receive()).toBeNull();
    channel.checkExpectationsSatisfied();

    channel.expectSend(op);
    expect(() => channel.checkExpectationsSatisfied()).toThrow(MockExpectationError);
    expect(() => channel.send(addParticipant())).toThrow(MockExpectationError);
    expect(() => channel.receive()).toThrow(MockExpectationError);
    channel.expectPeek(null);
    expect(() => channel.receive()).toThrow(MockExpectationError);
  });

  it("FakeOperationChannelMultiplexer announces known wavelets", () => {
    const snapshot = WaveletData.empty(
      WaveletName.of(WAVE_ID, WAVELET_ID),
      USER,
      HashedVersion.unsigned(0),
      0,
    );
    const calls: string[] = [];
    const listener = {
      onOperationChannelCreated: (channel: unknown, data: WaveletData, a: Accessibility) => {
        expect(channel).toBeInstanceOf(FakeOperationChannel);
        expect(data).toBe(snapshot);
        calls.push(`created ${a}`);
      },
      onOperationChannelRemoved: () => calls.push("removed"),
      onOpenFinished: () => calls.push("openFinished"),
      onFailed: () => calls.push("failed"),
    };
    const mux = new FakeOperationChannelMultiplexer();
    mux.open(listener, IdFilter.allIds());
    expect(calls).toEqual([]);
    mux.open(listener, IdFilter.allIds(), [
      new KnownWavelet(snapshot, HashedVersion.unsigned(0), Accessibility.READ_ONLY),
    ]);
    expect(calls).toEqual(["created READ_ONLY", "openFinished"]);
    expect(() => mux.createOperationChannel(WAVELET_ID, USER)).toThrow();
    mux.close();
  });

  it("FakeWaveViewServiceUpdate reports what was set", () => {
    const empty = new FakeWaveViewServiceUpdate();
    expect([
      empty.hasChannelId(),
      empty.hasWaveletId(),
      empty.hasLastCommittedVersion(),
      empty.hasCurrentVersion(),
      empty.hasWaveletSnapshot(),
      empty.hasDeltas(),
      empty.hasMarker(),
    ]).toEqual([false, false, false, false, false, false, false]);
    expect(() =>
      new FakeWaveViewServiceUpdate().setWaveletSnapshot(WAVE_ID, USER, 0, HashedVersion.unsigned(3)),
    ).toThrow(MockExpectationError);

    const v3 = HashedVersion.unsigned(3);
    const update = new FakeWaveViewServiceUpdate()
      .setWaveletId(WAVELET_ID)
      .setWaveletSnapshot(WAVE_ID, USER, 12, v3)
      .setLastCommittedVersion(v3)
      .setCurrentVersion(v3)
      .setMarker(false);
    expect(update.hasMarker()).toBe(true);
    const snapshot = update.getWaveletSnapshot()!;
    expect(snapshot.waveletId.equals(WAVELET_ID)).toBe(true);
    expect(snapshot.version).toBe(3);
    expect(snapshot.creationTime).toBe(12);
    expect(update.getLastCommittedVersion()).toBe(v3);
    expect(update.getCurrentVersion()).toBe(v3);
  });

  it("MockWaveViewService records calls", () => {
    const service = new MockWaveViewService();
    expect(() => service.lastOpen()).toThrow(MockExpectationError);
    expect(() => service.lastSubmit()).toThrow(MockExpectationError);
    expect(() => service.lastClose()).toThrow(MockExpectationError);
    const closeCallback = { onSuccess: () => {}, onFailure: () => {} };
    service.viewClose(WAVE_ID, "ch1", closeCallback);
    expect(service.closes.length).toBe(1);
    expect(service.lastClose().channelId).toBe("ch1");
    expect(service.lastClose().callback).toBe(closeCallback);
    expect(service.debugGetProfilingInfo(null)).toBe("");
  });
});
