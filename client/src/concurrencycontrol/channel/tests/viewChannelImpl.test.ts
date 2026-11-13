// Unit test for ViewChannelImpl.
//
// Ported from `ViewChannelImplTest` of Apache Wave (Apache License,
// Version 2.0).

import { beforeEach, describe, expect, it } from "vitest";

import type { WaveletData } from "../../../model/data";
import { IdFilter, ParticipantId, WaveId, WaveletId } from "../../../model/id";
import { TransformedWaveletDelta, WaveletDelta } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { type ChannelException, IllegalStateError, ResponseCode } from "../../common";
import { FakeWaveViewServiceUpdate, MockWaveViewService } from "../../testing";
import {
  type KnownWaveletVersions,
  type SubmitCallback,
  ViewChannelImpl,
  type ViewChannelListener,
  WaveletIdMap,
} from "..";
import { bytes } from "./helpers";

type ListenerCall =
  | "ON_CONNECTED"
  | "ON_CLOSED"
  | "ON_EXCEPTION"
  | "ON_OPEN_FINISHED"
  | "ON_UPDATE"
  | "ON_SNAPSHOT";

interface ListenerCallContext {
  readonly method: ListenerCall;
  readonly waveletId: WaveletId | null;
  readonly snapshot: WaveletData | null;
  readonly deltas: readonly TransformedWaveletDelta[] | null;
  readonly lastCommittedVersion: HashedVersion | null;
  readonly currentSignedVersion: HashedVersion | null;
}

function call(method: ListenerCall, rest: Partial<ListenerCallContext> = {}): ListenerCallContext {
  return {
    method,
    waveletId: null,
    snapshot: null,
    deltas: null,
    lastCommittedVersion: null,
    currentSignedVersion: null,
    ...rest,
  };
}

/** Mock to test that calls back from the ViewChannel are as expected. */
class MockViewChannelListener implements ViewChannelListener {
  private readonly methodCalls: ListenerCallContext[] = [];

  onConnected(): void {
    this.methodCalls.push(call("ON_CONNECTED"));
  }

  onClosed(): void {
    this.methodCalls.push(call("ON_CLOSED"));
  }

  onException(_e: ChannelException): void {
    this.methodCalls.push(call("ON_EXCEPTION"));
  }

  onOpenFinished(): void {
    this.methodCalls.push(call("ON_OPEN_FINISHED"));
  }

  onSnapshot(
    waveletId: WaveletId,
    wavelet: WaveletData,
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    this.methodCalls.push(
      call("ON_SNAPSHOT", {
        waveletId,
        snapshot: wavelet,
        lastCommittedVersion,
        currentSignedVersion,
      }),
    );
  }

  onUpdate(
    waveletId: WaveletId,
    deltas: readonly TransformedWaveletDelta[],
    lastCommittedVersion: HashedVersion | null,
    currentSignedVersion: HashedVersion | null,
  ): void {
    this.methodCalls.push(
      call("ON_UPDATE", { waveletId, deltas, lastCommittedVersion, currentSignedVersion }),
    );
  }

  /** The container message is not checked as it's not important. */
  expectedCall(method: ListenerCall, waveletId?: WaveletId): void {
    const context = this.methodCalls[0];
    if (context === undefined) {
      return expect.fail(`Expected a call to ${method}, there was none`);
    }
    expect(context.method).toBe(method);
    if (waveletId !== undefined) {
      expect(context.waveletId !== null && context.waveletId.equals(waveletId)).toBe(true);
    }
    this.methodCalls.shift();
  }

  expectedNothing(): void {
    expect(this.methodCalls.map((c) => c.method)).toEqual([]);
  }

  clear(): void {
    this.methodCalls.length = 0;
  }
}

type SubmitCall = "ON_SUCCESS" | "ON_FAILURE";

interface SubmitCallContext {
  readonly method: SubmitCall;
  readonly opsApplied: number;
  readonly version: HashedVersion;
  readonly error: string | null;
}

/** Mock to test that calls back from the ViewChannel are as expected. */
class MockSubmitListener implements SubmitCallback {
  private readonly methodCalls: SubmitCallContext[] = [];

  onSuccess(
    opsApplied: number,
    version: HashedVersion,
    _responseCode: ResponseCode,
    errorMessage: string | null,
  ): void {
    this.methodCalls.push({ method: "ON_SUCCESS", error: errorMessage, opsApplied, version });
  }

  onFailure(reason: string): void {
    this.methodCalls.push({
      method: "ON_FAILURE",
      error: reason,
      opsApplied: 0,
      version: HashedVersion.unsigned(0),
    });
  }

  private first(): SubmitCallContext {
    const context = this.methodCalls[0];
    if (context === undefined) {
      return expect.fail("Expected a submit callback, there was none");
    }
    return context;
  }

  expectedCallWithError(method: SubmitCall, error: string | null): void {
    const context = this.first();
    expect(context.method).toBe(method);
    expect(context.error).toBe(error);
    this.methodCalls.shift();
  }

  expectedCallWithVersion(method: SubmitCall, opsApplied: number, version: HashedVersion): void {
    const context = this.first();
    expect(context.method).toBe(method);
    expect(context.opsApplied).toBe(opsApplied);
    expect(context.version.equals(version)).toBe(true);
    this.methodCalls.shift();
  }
}

/** Wavelet id to use in the tests. */
const WAVELET_ID = WaveletId.of("example.com", "waveletId_1");

/** Channel Id to be used in the tests. */
const CHANNEL_ID = "channelId_1";

/** The original uses deltas without an author, which the model does not allow. */
const AUTHOR = ParticipantId.ofUnsafe("nobody@example.com");

const MAX_INT = Number.MAX_SAFE_INTEGER;

describe("ViewChannelImpl", () => {
  //
  // Fields used in most or all tests.
  //
  let channel: ViewChannelImpl;
  let viewOpenListener: MockViewChannelListener;
  let waveViewService: MockWaveViewService;

  beforeEach(() => {
    const waveId = WaveId.of("example.com", "waveid");
    ViewChannelImpl.resetViewChannelsPerWave();
    ViewChannelImpl.setMaxViewChannelsPerWave(MAX_INT);
    waveViewService = new MockWaveViewService();
    viewOpenListener = new MockViewChannelListener();
    channel = new ViewChannelImpl(waveId, waveViewService);
  });

  /** Opens the channel from the client side only. */
  function halfOpen(): void {
    const knownWavelets: KnownWaveletVersions = new WaveletIdMap();
    channel.open(viewOpenListener, IdFilter.allIds(), knownWavelets);
  }

  /** Simulates the server responding with the channel id. */
  function respondWithChannelId(): void {
    waveViewService
      .lastOpen()
      .callback.onUpdate(new FakeWaveViewServiceUpdate().setChannelId(CHANNEL_ID));
  }

  /** Simulates the server responding with the open-finished marker. */
  function respondWithMarker(waveEmpty: boolean): void {
    waveViewService.lastOpen().callback.onUpdate(new FakeWaveViewServiceUpdate().setMarker(waveEmpty));
  }

  /** Simulates the server sending a streaming update for a wavelet. */
  function respondWithEmptyUpdate(waveletId: WaveletId): void {
    waveViewService
      .lastOpen()
      .callback.onUpdate(
        new FakeWaveViewServiceUpdate()
          .setWaveletId(waveletId)
          .setLastCommittedVersion(HashedVersion.unsigned(0))
          .addDelta(new TransformedWaveletDelta(AUTHOR, HashedVersion.unsigned(0), 0, [])),
      );
  }

  function respondToSubmit(
    version: HashedVersion,
    opsApplied: number,
    error: string | null,
    response: ResponseCode,
  ): void {
    waveViewService.lastSubmit().callback.onSuccess(version, opsApplied, error, response);
  }

  function respondToSubmitWithFailure(): void {
    waveViewService.lastSubmit().callback.onFailure("WAVE_SERVER_ERROR");
  }

  /**
   * Opens the channel, and simulates the server responding with a channel
   * and a marker.
   */
  function open(): void {
    halfOpen();
    respondWithChannelId();
    respondWithMarker(false);
  }

  function close(): void {
    channel.close();
  }

  function terminateOpenRpcWithSuccess(): void {
    waveViewService.lastOpen().callback.onSuccess(null);
  }

  function terminateOpenRpcWithError(): void {
    waveViewService.lastOpen().callback.onSuccess("Server error for testing");
  }

  function terminateOpenRpcWithFailure(status: string): void {
    waveViewService.lastOpen().callback.onFailure(status);
  }

  function emptyDelta(): WaveletDelta {
    return new WaveletDelta(AUTHOR, HashedVersion.unsigned(0), []);
  }

  /**
   * Test that when everything is ok, we can connect, submit, create wavelet
   * and disconnect. This is not supposed to be a thorough test.
   */
  it("sunnyDayScenario", () => {
    open();
    viewOpenListener.expectedCall("ON_CONNECTED");
    viewOpenListener.expectedCall("ON_OPEN_FINISHED");

    // pretend an update with update wavelet. Note we don't add any data in
    // the wavelet because it's not really relevant for the test.
    respondWithEmptyUpdate(WAVELET_ID);
    viewOpenListener.expectedCall("ON_UPDATE", WAVELET_ID);

    // Submit a delta and check that we have the right channel id remembered.
    const submitListener = new MockSubmitListener();
    channel.submitDelta(WAVELET_ID, emptyDelta(), submitListener);
    expect(waveViewService.submits.length).toBe(1);
    expect(waveViewService.lastSubmit().channelId).toBe(CHANNEL_ID);

    // Return a success message on the submitted delta
    const hash = bytes(1, 2, 3, 4);
    respondToSubmit(HashedVersion.of(2, hash), 1, null, ResponseCode.OK);
    submitListener.expectedCallWithVersion("ON_SUCCESS", 1, HashedVersion.of(2, hash));

    // Check disconnect
    channel.close();
    expect(waveViewService.closes.length).toBe(1);
    waveViewService.lastClose().callback.onSuccess();
  });

  /** Tests that open synchronously calls the ViewOpen rpc on its wave service. */
  it("openIssuesViewOpenRpc", () => {
    open();
    expect(waveViewService.opens.length).toBe(1);
  });

  /**
   * Tests that a channel fails if it does not receive a channel id in the
   * first message.
   */
  it("initialUpdateWithoutAChannelIdFails", () => {
    halfOpen();

    // Receive an update with no channel id.
    waveViewService.lastOpen().callback.onUpdate(new FakeWaveViewServiceUpdate());
    viewOpenListener.expectedCall("ON_EXCEPTION");
  });

  /** Tests that a channel fails if it receives a message before it is opened. */
  it("messageBeforeOpenThrowsException", () => {
    expect(() => channel.onUpdate(new FakeWaveViewServiceUpdate())).toThrow(IllegalStateError);
  });

  it("successBeforeOpenThrowsException", () => {
    expect(() => channel.onSuccess("for testing")).toThrow(IllegalStateError);
  });

  it("failureBeforeOpenThrowsException", () => {
    expect(() => channel.onFailure("for testing")).toThrow(IllegalStateError);
  });

  /** Tests a channel fails if it receives success before channel id. */
  it("successBeforeChannelIdFails", () => {
    halfOpen();
    channel.onSuccess("for testing");
    viewOpenListener.expectedCall("ON_EXCEPTION");
  });

  /** Tests a channel closes if it receives failure before channel id. */
  it("failureBeforeChannelIdClosesChannel", () => {
    halfOpen();
    channel.onFailure("for testing");
    viewOpenListener.expectedCall("ON_CLOSED");
  });

  /** Tests that an update with the end-marker triggers the open-finished callback. */
  it("markerTriggersOpenFinished", () => {
    halfOpen();
    respondWithChannelId();
    viewOpenListener.clear();
    respondWithMarker(false);
    viewOpenListener.expectedCall("ON_OPEN_FINISHED");
  });

  /** Tests that the channel id triggers the connect callback. */
  it("channelIdTriggersConnectCallback", () => {
    halfOpen();
    respondWithChannelId();
    viewOpenListener.expectedCall("ON_CONNECTED");
  });

  /**
   * Tests that updates that arrive before the end-marker are passed on as
   * updates, and that when the end-marker eventually arrives, open-finished
   * is triggered then.
   */
  it("updatesBeforeOpenFinishedStillTriggersOpenFinished", () => {
    halfOpen();
    respondWithChannelId();
    viewOpenListener.clear();
    respondWithEmptyUpdate(WAVELET_ID);
    viewOpenListener.expectedCall("ON_UPDATE", WAVELET_ID);
    respondWithMarker(false);
    viewOpenListener.expectedCall("ON_OPEN_FINISHED");
  });

  /** Tests that closing the channel after a full open issues a ViewClose rpc. */
  it("closeAfterChannelIdCallsCloseRpc", () => {
    open();
    close();
    expect(waveViewService.closes.length).toBe(1);
    waveViewService.lastClose().callback.onSuccess();
  });

  /**
   * Tests that closing the channel before the server has responded with a
   * channel id does not issue a ViewClose rpc.
   */
  it("closeWithoutChannelIdDoesNotCallCloseRpc", () => {
    halfOpen();
    close();
    expect(waveViewService.closes.length).toBe(0);
  });

  /**
   * Tests that closing the channel before the server has responded with a
   * channel id causes a ViewClose rpc to be sent as soon as a channel id
   * arrives later.
   */
  it("closeWithoutChannelIdCallsCloseRpcIfChannelIdArrives", () => {
    halfOpen();
    close();
    expect(waveViewService.closes.length).toBe(0);
    respondWithChannelId();
    // Have got channel id, so we should now have issued a close.
    expect(waveViewService.closes.length).toBe(1);
    expect(waveViewService.lastClose().channelId).toBe(CHANNEL_ID);
    waveViewService.lastClose().callback.onSuccess();
  });

  /**
   * Tests that closing the channel prevents updates that arrive later from
   * being passed to the channel listener.
   */
  it("closeMasksFutureUpdatesFromOpenListener", () => {
    open();
    close();
    viewOpenListener.clear();
    respondWithEmptyUpdate(WAVELET_ID);
    viewOpenListener.expectedNothing();
  });

  it("closeTriggersCloseCallback", () => {
    open();
    viewOpenListener.clear();
    close();
    // The underlying service is expected to have the following behaviour.
    terminateOpenRpcWithSuccess();
    viewOpenListener.expectedCall("ON_CLOSED");
  });

  it("openRpcTerminationAfterUpdatesAndCloseTriggersCloseCallback", () => {
    open();
    respondWithEmptyUpdate(WAVELET_ID);
    viewOpenListener.clear();
    close();
    // The service should cause the open rpc to terminate successfully.
    terminateOpenRpcWithSuccess();
    viewOpenListener.expectedCall("ON_CLOSED");
  });

  it("openRpcTerminationWithoutCloseTriggersFailureAndClose", () => {
    open();
    viewOpenListener.clear();
    terminateOpenRpcWithSuccess();
    viewOpenListener.expectedCall("ON_CLOSED");
  });

  it("openRpcOnFailureAndThenTerminationWithoutCloseTriggersOneFailureAndClose", () => {
    open();
    viewOpenListener.clear();
    terminateOpenRpcWithFailure("WAVE_SERVER_ERROR");
    viewOpenListener.expectedCall("ON_CLOSED");
    terminateOpenRpcWithError();
    viewOpenListener.expectedNothing();
  });

  it("multipleClose", () => {
    open();
    viewOpenListener.clear();
    close();
    viewOpenListener.expectedCall("ON_CLOSED");
    close();
    viewOpenListener.expectedNothing();
  });

  it("closeWithoutOpen", () => {
    close();
    viewOpenListener.expectedNothing();
  });

  it("openAfterOpenThrowsException", () => {
    open();
    // The original passes nulls: the state check comes first.
    expect(() =>
      channel.open(
        null as unknown as ViewChannelListener,
        null as unknown as IdFilter,
        null as unknown as KnownWaveletVersions,
      ),
    ).toThrow(IllegalStateError);
  });

  it("submitDeltaWithErrorMessage", () => {
    open();

    // submit a delta
    const submitListener = new MockSubmitListener();
    channel.submitDelta(WAVELET_ID, emptyDelta(), submitListener);

    // Success with error the submit delta.
    const errorMessage = "Bad things happened on the server";
    respondToSubmit(HashedVersion.of(2, bytes(1, 2, 3, 4)), 1, errorMessage, ResponseCode.OK);
    submitListener.expectedCallWithError("ON_SUCCESS", errorMessage);
  });

  /**
   * Tests that a failure of a ViewSubmit rpc causes the channel to call the
   * failure callback registered on delta submission.
   */
  it("deltaSubmissionFailureCallsSubmissionFailureCallback", () => {
    open();

    // Submit a delta.
    const listener = new MockSubmitListener();
    channel.submitDelta(WAVELET_ID, emptyDelta(), listener);

    // Fail the submit delta, and expect that the submit listener got the error.
    respondToSubmitWithFailure();
    listener.expectedCallWithError("ON_FAILURE", "WAVE_SERVER_ERROR");
  });

  it("submitDeltaOnClosedChannelThrowsIllegalStateException", () => {
    open();
    close();

    // submit a delta should fail
    expect(() => channel.submitDelta(WAVELET_ID, emptyDelta(), new MockSubmitListener())).toThrow(
      IllegalStateError,
    );
  });

  it("openAfterCloseThrowsIllegalStateException", () => {
    open();
    close();

    // Opening the channel again should fail.
    expect(() => halfOpen()).toThrow(IllegalStateError);
  });

  it("failedOpenCallsListenerFailure", () => {
    halfOpen();

    // fail the open
    terminateOpenRpcWithFailure("WAVE_SERVER_ERROR");
    viewOpenListener.expectedCall("ON_CLOSED");
  });

  it("cannotCreateTooManyChannels", () => {
    ViewChannelImpl.setMaxViewChannelsPerWave(4);
    const waveId = WaveId.of("example.com", "toomanywaveid");
    for (let i = 0; i < 4; i++) {
      channel = new ViewChannelImpl(waveId, waveViewService);
    }
    expect(() => new ViewChannelImpl(waveId, waveViewService)).toThrow(IllegalStateError);
  });

  it("closingOneChannelMakesRoomForAnother", () => {
    const waveId = WaveId.of("example.com", "makeroomwaveid");
    ViewChannelImpl.setMaxViewChannelsPerWave(4);
    for (let i = 0; i < 4; i++) {
      channel = new ViewChannelImpl(waveId, waveViewService);
    }
    channel.close(); // Close the last channel, making room for another.
    channel = new ViewChannelImpl(waveId, waveViewService);
  });
});
