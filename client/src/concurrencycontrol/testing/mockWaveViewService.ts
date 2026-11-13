// A wave view service that records the calls made to it.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.testing.MockWaveViewService`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import type { IdFilter, WaveId, WaveletName } from "../../model/id";
import type { WaveletDelta } from "../../model/operation";
import type {
  CloseCallback,
  KnownWaveletVersions,
  OpenCallback,
  WaveViewService,
  WaveViewServiceSubmitCallback,
} from "../channel";
import { assertTrue } from "./testAssert";

export interface OpenArguments {
  readonly waveletFilter: IdFilter;
  readonly knownWavelets: KnownWaveletVersions;
  readonly callback: OpenCallback;
}

export interface SubmitArguments {
  readonly wavelet: WaveletName;
  readonly delta: WaveletDelta;
  readonly channelId: string;
  readonly callback: WaveViewServiceSubmitCallback;
}

export interface CloseArguments {
  readonly waveId: WaveId;
  readonly channelId: string;
  readonly callback: CloseCallback;
}

export class MockWaveViewService implements WaveViewService {
  readonly opens: OpenArguments[] = [];
  readonly submits: SubmitArguments[] = [];
  readonly closes: CloseArguments[] = [];

  lastOpen(): OpenArguments {
    const last = this.opens[this.opens.length - 1];
    assertTrue(last !== undefined, "No view open");
    return last;
  }

  lastSubmit(): SubmitArguments {
    const last = this.submits[this.submits.length - 1];
    assertTrue(last !== undefined, "No view submit");
    return last;
  }

  lastClose(): CloseArguments {
    const last = this.closes[this.closes.length - 1];
    assertTrue(last !== undefined, "No view close");
    return last;
  }

  viewOpen(
    waveletFilter: IdFilter,
    knownWavelets: KnownWaveletVersions,
    callback: OpenCallback,
  ): void {
    this.opens.push({ waveletFilter, knownWavelets, callback });
  }

  viewSubmit(
    wavelet: WaveletName,
    delta: WaveletDelta,
    channelId: string,
    callback: WaveViewServiceSubmitCallback,
  ): string | null {
    this.submits.push({ wavelet, delta, channelId, callback });
    return null;
  }

  viewClose(waveId: WaveId, channelId: string, callback: CloseCallback): void {
    this.closes.push({ waveId, channelId, callback });
  }

  debugGetProfilingInfo(_requestId: string | null): string {
    return "";
  }
}
