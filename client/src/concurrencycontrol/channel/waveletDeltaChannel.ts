// A channel of deltas for a single wavelet, which serialises submissions
// and delivers server messages in version order.
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.channel.WaveletDeltaChannel`
// of Apache Wave, licensed under the Apache License, Version 2.0. The nested
// types of the original are top-level here: `WaveletDeltaChannelReceiver`
// (`Receiver`), `WaveletDeltaChannelTransmitter` (`Transmitter`) and
// `ClientMessage` (`Transmitter.ClientMessage`).

import type {
  TransformedWaveletDelta,
  WaveletDelta,
} from "../../model/operation";
import type { HashedVersion } from "../../model/version";
import type { ResponseCode } from "../common";

/**
 * Receiver of messages from a delta channel.
 *
 * A receiver will first receive a connection message followed by zero or
 * more deltas, commits, acks, or nacks, and eventually a termination
 * message. If the channel is reset from either end this sequence may
 * repeat. Every method may throw `ChannelException` if the channel fails.
 */
export interface WaveletDeltaChannelReceiver {
  /**
   * Called upon successful connection (re-)establishment. If the channel
   * connects to the latest wavelet version then `connectVersion` equals
   * `currentVersion`. If the channel reconnects to a prior known version
   * then `currentVersion` may be later than `connectVersion`.
   */
  onConnection(
    connectVersion: HashedVersion,
    currentVersion: HashedVersion,
  ): void;

  /** Called when a delta (not sent on this channel) is received from the server. */
  onDelta(delta: TransformedWaveletDelta): void;

  /**
   * Called when the wave server has committed operations to persistent
   * storage; `version` is the latest committed version.
   */
  onCommit(version: number): void;

  /**
   * Called when the server acknowledges (accepts) operations sent on this
   * connection. `opsApplied` is the number of ops from the delta applied by
   * the server and `version` its wavelet version after the last of them.
   */
  onAck(opsApplied: number, version: HashedVersion): void;

  /**
   * Called when the server rejects a delta sent on this connection.
   * `version` is the server's wavelet version at the time of rejection.
   */
  onNack(
    responseCode: ResponseCode,
    errorString: string | null,
    version: number,
  ): void;
}

/**
 * A message from wave client to wave server, consisting of a delta and a
 * (ignored) requestCommit flag.
 */
export class ClientMessage {
  private readonly delta: WaveletDelta | null;
  private readonly commitRequest: boolean;

  constructor(delta: WaveletDelta | null, commitRequest: boolean) {
    this.delta = delta;
    this.commitRequest = commitRequest;
  }

  /** Null means nothing to send. */
  getDelta(): WaveletDelta | null {
    return this.delta;
  }

  hasCommitRequest(): boolean {
    return this.commitRequest;
  }
}

/** Produces a single client message. Will be called at most once ("one-shot"). */
export interface WaveletDeltaChannelTransmitter {
  /**
   * Called when the channel is ready to transmit. A message with a null
   * delta means nothing to send (for instance, if the operation to send has
   * been undone).
   */
  takeMessage(): ClientMessage;
}

export interface WaveletDeltaChannel {
  /**
   * Resets this delta channel and installs a new receiver (or null). Any
   * messages already received from the server are dropped. The caller must
   * ensure that the next message (if any) received by this channel is a
   * (re)connection message.
   */
  reset(receiver: WaveletDeltaChannelReceiver | null): void;

  /**
   * Signals the intent to send a delta to the server. When the channel is
   * ready to transmit the delta, it invokes the transmitter's `takeMessage`
   * to get the delta to transmit.
   *
   * The transmitter is "one-shot": if its takeMessage is invoked, the
   * transmitter is forgotten. This method must be called (at least) once for
   * every outgoing message. It replaces the transmitter from the last call,
   * if that transmitter has not been invoked in the meanwhile.
   *
   * Clients must only call send once until an ack or nack is received. The
   * channel must be connected.
   */
  send(t: WaveletDeltaChannelTransmitter): void;

  /** Debug string with profile information about the data sent. */
  debugGetProfilingInfo(): string;
}
