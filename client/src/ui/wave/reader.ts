// Reading by moving the focus frame.
//
// A port of `Reader` of the Apache Wave web client
// (`org.waveprotocol.wave.client.wavepanel.impl.reader`, Apache License
// 2.0): the blip the focus frame arrives on starts being read, the one it
// leaves stops being read, and "next/previous unread" walk the full blip
// order (collapsed threads included) to the nearest unread blip.
//
// What "being read" means is the supplement's business
// (`LocalSupplementedWave`: the blip is marked read and stays read while
// changes to it arrive). `SimpleReadingSupplement` is a small stand-in for
// an open wave that does not offer `startReading`/`stopReading`.

import type { Traverser } from "./traversal";

/** The part of `LocalSupplementedWave` the reader needs. */
export interface ReadingSupplement {
  startReading(blipId: string): void;
  stopReading(blipId: string): void;
  isUnread(blipId: string): boolean;
}

export class Reader {
  constructor(
    private readonly supplement: ReadingSupplement,
    /** The order to look for unread blips in: collapsed content included. */
    private readonly traverser: Traverser,
    /** Whether a blip (still) exists; a deleted blip is not stopped. */
    private readonly exists: (blipId: string) => boolean = () => true,
  ) {}

  /** `onFocusMoved`. */
  onFocusMoved(oldBlipId: string | null, newBlipId: string | null): void {
    if (oldBlipId !== null && this.exists(oldBlipId)) {
      this.supplement.stopReading(oldBlipId);
    }
    if (newBlipId !== null && this.exists(newBlipId)) {
      this.supplement.startReading(newBlipId);
    }
  }

  isRead(blipId: string): boolean {
    return !this.supplement.isUnread(blipId);
  }

  /** `getNext`: the next unread blip after `start`. */
  next(start: string): string | null {
    let blip = this.traverser.next(start);
    while (blip !== null && this.isRead(blip)) {
      blip = this.traverser.next(blip);
    }
    return blip;
  }

  /** `getPrevious`: the nearest unread blip before `start`. */
  previous(start: string): string | null {
    let blip = this.traverser.previous(start);
    while (blip !== null && this.isRead(blip)) {
      blip = this.traverser.previous(blip);
    }
    return blip;
  }
}

/** What an open wave offers for read state without local read tracking. */
export interface PlainSupplement {
  isUnread(blipId: string): boolean;
  markBlipRead(blipId: string): void;
}

/**
 * `startReading`/`stopReading` over a plain supplement: the blip is marked
 * read when reading starts and again when it stops, and counts as read in
 * between whatever arrives (a reduced `LocalSupplementedWaveImpl`: no
 * auto-read period after reading stopped).
 */
export class SimpleReadingSupplement implements ReadingSupplement {
  private reading: string | null = null;

  constructor(private readonly delegate: PlainSupplement) {}

  startReading(blipId: string): void {
    this.reading = blipId;
    this.markRead(blipId);
  }

  stopReading(blipId: string): void {
    if (this.reading === blipId) {
      this.reading = null;
    }
    this.markRead(blipId);
  }

  isUnread(blipId: string): boolean {
    return this.reading !== blipId && this.delegate.isUnread(blipId);
  }

  private markRead(blipId: string): void {
    if (this.delegate.isUnread(blipId)) {
      this.delegate.markBlipRead(blipId);
    }
  }
}
