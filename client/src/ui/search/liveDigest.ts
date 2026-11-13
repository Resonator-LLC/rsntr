// Digests of the waves that are open in this client, made from the live
// wave model, so that title, snippet, counts and participants of such a
// wave follow it at once instead of at the next search.
//
// Stands where `WaveBasedDigest` (and the wave store `SimpleSearch`
// consults) of the Apache Wave web client stand. The digest itself is made
// as the server makes it (`crates/resonator-model/src/digest.rs`, from
// `WaveDigester` and `Snippets`), so that it does not change when the
// server's takes its place again. Apache License 2.0.

import { type ReadonlySignal, signal } from "@preact/signals";

import type { OpenWave, WaveClient } from "../../client";
import type { BlipData, WaveletData } from "../../model/data";
import type { BootstrapDocument } from "../../model/docop";
import { util as idUtil } from "../../model/id";
import type { OpBasedWavelet } from "../../model/wavelet";
import type { Unsubscribe } from "../../util/events";
import type { Digest } from "../../wire/search";

/** `Snippets.DIGEST_SNIPPET_LENGTH`, in UTF-16 code units. */
export const DIGEST_SNIPPET_LENGTH = 140;
/** `WaveDigester.PARTICIPANTS_SNIPPET_LENGTH`: the author and four more. */
export const PARTICIPANTS_SNIPPET_LENGTH = 5;

/** Java's `String.trim`: strips characters up to U+0020 from both ends. */
function javaTrim(s: string): string {
  let start = 0;
  let end = s.length;
  while (start < end && s.charCodeAt(start) <= 0x20) {
    start++;
  }
  while (end > start && s.charCodeAt(end - 1) <= 0x20) {
    end--;
  }
  return s.slice(start, end);
}

/** `substring(0, max)` that does not split a surrogate pair. */
function truncate(s: string, max: number): string {
  if (s.length <= max) {
    return s;
  }
  const last = s.charCodeAt(max - 1);
  return s.slice(0, last >= 0xd800 && last <= 0xdbff ? max - 1 : max);
}

/** `Snippets.collateTextForDocuments` for one document. */
function collateText(doc: BootstrapDocument): string {
  let text = "";
  for (const component of doc.asOperation().components) {
    if (component.type === "characters") {
      text += component.characters;
    } else if (component.type === "elementStart" && component.tag === "line") {
      text += " ";
    }
  }
  return javaTrim(text);
}

/** `Snippets.renderSnippet`. */
export function renderSnippet(wavelet: WaveletData, maxLength: number): string {
  let newest: BlipData | null = null;
  let newestTime = -1;
  for (const document of wavelet.documents()) {
    if (document.lastModifiedTime > newestTime) {
      newestTime = document.lastModifiedTime;
      newest = document;
    }
  }
  if (newest === null) {
    return truncate(javaTrim(wavelet.documents().map((d) => collateText(d.content)).join("")), maxLength);
  }
  let snippet = `${collateText(newest.content)} `;
  for (const component of newest.content.asOperation().components) {
    if (snippet.length >= maxLength) {
      break;
    }
    if (component.type === "elementStart" && component.tag === "blip") {
      const blipId = component.attributes.get("id");
      const document = blipId === undefined || blipId === null ? null : wavelet.document(blipId);
      if (document !== null) {
        snippet += `${collateText(document.content)} `;
      }
    }
  }
  return truncate(snippet, maxLength);
}

/**
 * The digest of an open wave as the server would make it for the viewer
 * now (`WaveDigester.generateDigest`), with the viewer's read state as this
 * client sees it. Null unless the wave is open (or, with `evenIfClosed`, was
 * closed, whose last state is still there) and has a root conversation.
 */
export function digestOfWave(wave: OpenWave, evenIfClosed = false): Digest | null {
  if (wave.state !== "open" && !(evenIfClosed && wave.state === "closed")) {
    return null;
  }
  const root = wave.view.root();
  const conversation = wave.conversation();
  if (root === null || conversation === null) {
    return null;
  }
  const data = root.data();
  const title = javaTrim(conversation.title());
  let snippet = javaTrim(renderSnippet(data, DIGEST_SNIPPET_LENGTH));
  if (title !== "" && snippet.startsWith(title)) {
    // Strip the title from the snippet if the snippet starts with it.
    snippet = snippet.slice(title.length);
  }
  const blips = conversation.blipsBreadthFirst();
  let lastModified = -1;
  for (const blip of blips) {
    const blipData = conversation.blipData(blip.id);
    if (blipData !== null) {
      lastModified = Math.max(lastModified, blipData.lastModifiedTime);
    }
  }
  const waveletId = wave.view.rootId();
  const unreadCount = wave.supplement.unreadCount(
    waveletId,
    blips.map((blip) => blip.id),
  );
  const participants = data.participants
    .slice(0, PARTICIPANTS_SNIPPET_LENGTH)
    .map((participant) => participant.address);
  const [author, ...others] = participants;
  if (author === undefined) {
    return null;
  }
  return {
    title,
    snippet,
    waveId: wave.waveId.serialise(),
    lastModified: BigInt(Math.trunc(lastModified)),
    unreadCount,
    blipCount: blips.length,
    participants: others,
    author,
  };
}

/** Digests that are known better here than by the last search. */
export interface LiveDigests {
  /** Changes whenever a digest may have. */
  readonly version: ReadonlySignal<number>;
  /** The digest to show instead of the server's, if any. */
  get(waveId: string): Digest | undefined;
  /** Looks for waves that were opened or closed since. */
  sync(): void;
  /** A moment in time; see {@link settle}. */
  mark(): number;
  /**
   * A search begun at `mark` has answered: digests kept for waves closed
   * before it are dropped, the server's are as good now.
   */
  settle(mark: number): void;
  dispose(): void;
}

interface Tracked {
  readonly wave: OpenWave;
  readonly off: Unsubscribe[];
  wavelets: Map<OpBasedWavelet, Unsubscribe>;
  digest: Digest | null | undefined;
}

export interface OpenWaveDigestsOptions {
  /** Called (later, once) after something changed. Default: a 30 ms timer. */
  schedule?: (run: () => void) => void;
}

/**
 * The {@link LiveDigests} of the waves open in a `WaveClient`
 * (`client.openWaves()`). The client does not announce the opening and
 * closing of waves: call {@link sync} when that may have happened.
 *
 * When a wave is closed its last digest is kept until a search begun after
 * that has answered, so that what the user just did in the wave (reading
 * it, say) does not seem undone by an older search result.
 */
export class OpenWaveDigests implements LiveDigests {
  private readonly versionSignal = signal(0);
  readonly version: ReadonlySignal<number> = this.versionSignal;

  private readonly tracked = new Map<string, Tracked>();
  /** Last digests of closed waves, with the mark at which they closed. */
  private readonly kept = new Map<string, { digest: Digest; closedAt: number }>();
  private clock = 0;
  private scheduled = false;
  private disposed = false;
  private readonly schedule: (run: () => void) => void;

  constructor(
    private readonly client: Pick<WaveClient, "openWaves">,
    options: OpenWaveDigestsOptions = {},
  ) {
    this.schedule = options.schedule ?? ((run) => void setTimeout(run, 30));
  }

  dispose(): void {
    this.disposed = true;
    for (const tracked of this.tracked.values()) {
      this.untrack(tracked);
    }
    this.tracked.clear();
    this.kept.clear();
  }

  get(waveId: string): Digest | undefined {
    const tracked = this.tracked.get(waveId);
    if (tracked !== undefined) {
      if (tracked.digest === undefined) {
        try {
          tracked.digest = digestOfWave(tracked.wave);
        } catch {
          // A wave in a state the digest does not understand: the server's
          // digest will do.
          tracked.digest = null;
        }
      }
      if (tracked.digest !== null) {
        return tracked.digest;
      }
    }
    return this.kept.get(waveId)?.digest;
  }

  mark(): number {
    return ++this.clock;
  }

  settle(mark: number): void {
    let changed = false;
    for (const [waveId, kept] of this.kept) {
      if (kept.closedAt < mark) {
        this.kept.delete(waveId);
        changed = true;
      }
    }
    if (changed) {
      this.versionSignal.value++;
    }
  }

  sync(): void {
    if (this.disposed) {
      return;
    }
    const open = new Map<string, OpenWave>();
    for (const wave of this.client.openWaves()) {
      open.set(wave.waveId.serialise(), wave);
    }
    let changed = false;
    for (const [waveId, tracked] of this.tracked) {
      if (open.get(waveId) !== tracked.wave) {
        let last = tracked.digest;
        if (last === undefined) {
          // Changed after it was last shown: its state at closing.
          try {
            last = digestOfWave(tracked.wave, true);
          } catch {
            last = null;
          }
        }
        if (last !== undefined && last !== null) {
          this.kept.set(waveId, { digest: last, closedAt: ++this.clock });
        }
        this.untrack(tracked);
        this.tracked.delete(waveId);
        changed = true;
      }
    }
    for (const [waveId, wave] of open) {
      if (!this.tracked.has(waveId)) {
        this.tracked.set(waveId, this.track(wave));
        this.kept.delete(waveId);
        changed = true;
      }
    }
    if (changed) {
      this.versionSignal.value++;
    }
  }

  private track(wave: OpenWave): Tracked {
    const tracked: Tracked = { wave, off: [], wavelets: new Map(), digest: undefined };
    const invalidate = (): void => this.invalidate(tracked);
    const watch = (wavelet: OpBasedWavelet): void => {
      let conversational = false;
      try {
        conversational = idUtil.isConversationalId(wavelet.waveletId());
      } catch {
        // Not a kind of id the digest cares about.
      }
      if (conversational && !tracked.wavelets.has(wavelet)) {
        tracked.wavelets.set(wavelet, wavelet.events.on("operation", invalidate));
      }
    };
    wave.view.wavelets().forEach(watch);
    tracked.off.push(
      wave.view.events.on("waveletAdded", ({ wavelet }) => {
        watch(wavelet);
        invalidate();
      }),
      wave.view.events.on("waveletRemoved", ({ wavelet }) => {
        tracked.wavelets.get(wavelet)?.();
        tracked.wavelets.delete(wavelet);
        invalidate();
      }),
      wave.events.on("opened", invalidate),
      wave.supplement.events.on("maybeBlipReadChanged", invalidate),
      wave.supplement.events.on("maybeWaveletReadChanged", invalidate),
    );
    return tracked;
  }

  private untrack(tracked: Tracked): void {
    tracked.off.forEach((off) => off());
    tracked.wavelets.forEach((off) => off());
    tracked.wavelets.clear();
  }

  private invalidate(tracked: Tracked): void {
    tracked.digest = undefined;
    if (this.scheduled || this.disposed) {
      return;
    }
    this.scheduled = true;
    this.schedule(() => {
      this.scheduled = false;
      if (!this.disposed) {
        this.versionSignal.value++;
      }
    });
  }
}

/** Used by tests and by a panel without live digests. */
export const NO_LIVE_DIGESTS: LiveDigests = {
  version: signal(0),
  get: () => undefined,
  sync: () => undefined,
  mark: () => 0,
  settle: () => undefined,
  dispose: () => undefined,
};
