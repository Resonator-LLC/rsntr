// The order in which the focus frame moves over the blips of a
// conversation.
//
// A port of `ViewTraverser` of the Apache Wave web client
// (`org.waveprotocol.wave.client.wavepanel.impl.focus`, Apache License 2.0)
// onto ids: the original walks the view tree (blip, its inline anchors in
// document order, its default anchors in manifest order, the next blip of
// the thread, then up); here the reply threads of a blip are given in that
// display order already, so the walk is a pre-order of the blips.
// Collapsed threads can be skipped, as the focus frame of the original
// does (`FocusFrameBuilder`: `traverser.skipCollapsedContent()`).

/** The structure of a conversation as it is displayed. */
export interface BlipTree {
  /** The blips of a thread in order; the root thread is `""`. Empty if unknown. */
  threadBlips(threadId: string): readonly string[];
  /** The thread a blip is in, or null if there is no such blip. */
  threadOf(blipId: string): string | null;
  /**
   * The reply threads of a blip in display order: those anchored in its
   * text by location, then the others in manifest order.
   */
  replyThreads(blipId: string): readonly string[];
  /** The blip a thread hangs off; null for the root thread (and unknown ones). */
  parentBlipOf(threadId: string): string | null;
  isCollapsed(threadId: string): boolean;
}

export const ROOT_THREAD = "";

export class Traverser {
  constructor(
    private readonly tree: BlipTree,
    private readonly skipCollapsed: boolean,
  ) {}

  private blipsOf(threadId: string): readonly string[] {
    return this.skipCollapsed && this.tree.isCollapsed(threadId) ? [] : this.tree.threadBlips(threadId);
  }

  /** `getFirst`: the first blip of the conversation. */
  first(): string | null {
    return this.tree.threadBlips(ROOT_THREAD)[0] ?? null;
  }

  /** `getLast`: the last blip in the order. */
  last(): string | null {
    const blips = this.tree.threadBlips(ROOT_THREAD);
    const last = blips[blips.length - 1];
    return last === undefined ? null : this.lastWithin(last);
  }

  /** The first blip inside the reply threads of a blip from index `from` on. */
  private firstInReplies(blipId: string, from: number): string | null {
    const threads = this.tree.replyThreads(blipId);
    for (let i = from; i < threads.length; i++) {
      const first = this.blipsOf(threads[i]!)[0];
      if (first !== undefined) {
        return first;
      }
    }
    return null;
  }

  /** The last blip in the subtree of a blip (the blip itself if it has no replies shown). */
  private lastWithin(blipId: string, before?: number): string {
    const threads = this.tree.replyThreads(blipId);
    for (let i = (before ?? threads.length) - 1; i >= 0; i--) {
      const blips = this.blipsOf(threads[i]!);
      const last = blips[blips.length - 1];
      if (last !== undefined) {
        return this.lastWithin(last);
      }
    }
    return blipId;
  }

  /** `getNext`: the blip after `blipId`, or null at the end. */
  next(blipId: string): string | null {
    const inside = this.firstInReplies(blipId, 0);
    if (inside !== null) {
      return inside;
    }
    let current = blipId;
    for (;;) {
      const threadId = this.tree.threadOf(current);
      if (threadId === null) {
        return null;
      }
      const blips = this.tree.threadBlips(threadId);
      const next = blips[blips.indexOf(current) + 1];
      if (next !== undefined) {
        return next;
      }
      // After the last blip of a thread: the parent blip's later threads,
      // then whatever follows the parent blip.
      const parent = this.tree.parentBlipOf(threadId);
      if (parent === null) {
        return null;
      }
      const later = this.firstInReplies(parent, this.tree.replyThreads(parent).indexOf(threadId) + 1);
      if (later !== null) {
        return later;
      }
      current = parent;
    }
  }

  /** `getPrevious`: the blip before `blipId`, or null at the start. */
  previous(blipId: string): string | null {
    const threadId = this.tree.threadOf(blipId);
    if (threadId === null) {
      return null;
    }
    const blips = this.tree.threadBlips(threadId);
    const index = blips.indexOf(blipId);
    if (index > 0) {
      return this.lastWithin(blips[index - 1]!);
    }
    const parent = this.tree.parentBlipOf(threadId);
    if (parent === null) {
      return null;
    }
    // The first blip of a reply thread: the end of the parent's earlier
    // threads, or the parent itself.
    return this.lastWithin(parent, this.tree.replyThreads(parent).indexOf(threadId));
  }
}
