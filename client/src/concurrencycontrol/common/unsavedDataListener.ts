// Listener for information about whether data has been acknowledged and
// committed by servers.
//
// A typical connection state (D = client delta):
//
//  acknowledged
//    | |   |
//    v v   v
// ---D-D---D---D------------D-D---D--D----------------> Time
//   ^          ^            ^
//   |          |            |
// commited   in flight    queue start
//
// Derived from `org.waveprotocol.wave.concurrencycontrol.common.UnsavedDataListener`
// of Apache Wave, licensed under the Apache License, Version 2.0.

/** Information about acknowledged and committed data. */
export interface UnsavedDataInfo {
  /** Size of data sent but not yet acknowledged. */
  inFlightSize(): number;

  /**
   * An estimate of the size of the total unacknowledged data: in flight data
   * plus data accumulating waiting to be sent.
   */
  estimateUnacknowledgedSize(): number;

  /**
   * An estimate of the size of the total uncommitted data: unacknowledged
   * data plus acknowledged but not committed data.
   */
  estimateUncommittedSize(): number;

  /** Latest acknowledgement version (`laskAckVersion` in the original). */
  lastAckVersion(): number;

  /** Latest commit version. */
  lastCommitVersion(): number;

  /**
   * A debug string about the unsaved data. Could be expensive due to
   * stringification of data; refrain from calling this constantly.
   */
  getInfo(): string;
}

export interface UnsavedDataListener {
  /**
   * Called by the source of updates. Will not be called after onClose().
   * `unsavedDataInfo` is only valid during the call.
   */
  onUpdate(unsavedDataInfo: UnsavedDataInfo): void;

  /**
   * Called when the resource has been closed. `everythingCommitted` is true
   * if all data on the server has been committed.
   */
  onClose(everythingCommitted: boolean): void;
}
