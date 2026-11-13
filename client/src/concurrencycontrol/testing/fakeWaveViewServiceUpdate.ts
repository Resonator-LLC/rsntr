// Fake WaveViewServiceUpdate implementation for providing canned responses.
//
// Derived from
// `org.waveprotocol.wave.concurrencycontrol.testing.FakeWaveViewServiceUpdate`
// of Apache Wave, licensed under the Apache License, Version 2.0.

import { WaveletData } from "../../model/data";
import { type ParticipantId, type WaveId, type WaveletId, WaveletName } from "../../model/id";
import type { TransformedWaveletDelta } from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { WaveViewServiceUpdate } from "../channel";
import { assertTrue } from "./testAssert";

export class FakeWaveViewServiceUpdate implements WaveViewServiceUpdate {
  channelId: string | null = null;
  waveletId: WaveletId | null = null;
  lastCommittedVersion: HashedVersion | null = null;
  currentVersion: HashedVersion | null = null;
  waveletSnapshot: WaveletData | null = null;
  deltaList: TransformedWaveletDelta[] = [];
  marker: boolean | null = null;

  setChannelId(channelId: string | null): this {
    this.channelId = channelId;
    return this;
  }

  setWaveletId(waveletId: WaveletId | null): this {
    this.waveletId = waveletId;
    return this;
  }

  setLastCommittedVersion(lastCommittedVersion: HashedVersion | null): this {
    this.lastCommittedVersion = lastCommittedVersion;
    return this;
  }

  setCurrentVersion(currentVersion: HashedVersion | null): this {
    this.currentVersion = currentVersion;
    return this;
  }

  /** Sets an empty wavelet as the snapshot. The wavelet id must be set first. */
  setWaveletSnapshot(
    waveId: WaveId,
    creator: ParticipantId,
    creationTime: number,
    hashedVersion: HashedVersion,
  ): this {
    assertTrue(this.waveletId !== null, "setWaveletSnapshot requires a wavelet id");
    this.waveletSnapshot = WaveletData.empty(
      WaveletName.of(waveId, this.waveletId),
      creator,
      hashedVersion,
      creationTime,
    );
    return this;
  }

  addDelta(delta: TransformedWaveletDelta): this {
    this.deltaList.push(delta);
    return this;
  }

  setMarker(marker: boolean | null): this {
    this.marker = marker;
    return this;
  }

  hasChannelId(): boolean {
    return this.channelId !== null;
  }

  getChannelId(): string | null {
    return this.channelId;
  }

  hasWaveletId(): boolean {
    return this.waveletId !== null;
  }

  getWaveletId(): WaveletId | null {
    return this.waveletId;
  }

  hasLastCommittedVersion(): boolean {
    return this.lastCommittedVersion !== null;
  }

  getLastCommittedVersion(): HashedVersion | null {
    return this.lastCommittedVersion;
  }

  hasCurrentVersion(): boolean {
    return this.currentVersion !== null;
  }

  getCurrentVersion(): HashedVersion | null {
    return this.currentVersion;
  }

  hasWaveletSnapshot(): boolean {
    return this.waveletSnapshot !== null;
  }

  getWaveletSnapshot(): WaveletData | null {
    return this.waveletSnapshot;
  }

  hasDeltas(): boolean {
    return this.deltaList.length !== 0;
  }

  getDeltaList(): readonly TransformedWaveletDelta[] {
    return this.deltaList;
  }

  hasMarker(): boolean {
    return this.marker !== null;
  }
}
