// Screen-reader announcements of messages others add to the open wave
// ("Ben replied to you"), batched over a short time so a burst is one
// announcement ("3 new messages from Ben and Anna"). Own messages, from
// this or another device, are never announced. What is announced follows
// the Accessibility setting (data-announce on <html>, see prefs.ts).

import { type Signal, signal } from "@preact/signals";

export type AnnounceMode = "all" | "replies" | "none";

/** A new message: its author, and the author of the message it answers. */
export interface Arrival {
  readonly author: string;
  readonly parentAuthor: string | null;
}

/** The text for these arrivals, or null when there is nothing to say. */
export function announcementText(
  arrivals: readonly Arrival[],
  viewer: string | null,
  mode: AnnounceMode,
  name: (address: string) => string,
): string | null {
  if (mode === "none") {
    return null;
  }
  const kept = arrivals.filter(
    (a) => a.author !== viewer && (mode === "all" || (viewer !== null && a.parentAuthor === viewer)),
  );
  if (kept.length === 0) {
    return null;
  }
  if (kept.length === 1) {
    const { author, parentAuthor } = kept[0]!;
    if (parentAuthor === null) {
      return `${name(author)} wrote a new message`;
    }
    return `${name(author)} replied to ${parentAuthor === viewer ? "you" : name(parentAuthor)}`;
  }
  const names = [...new Set(kept.map((a) => a.author))].map(name);
  const who =
    names.length === 1
      ? names[0]!
      : names.length <= 3
        ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]!}`
        : `${names.slice(0, 2).join(", ")} and ${names.length - 2} others`;
  return `${kept.length} new messages from ${who}`;
}

export interface AnnouncerOptions {
  /** Who wrote the message and what it answers; null when it is gone. */
  readonly describe: (blipId: string) => Arrival | null;
  readonly viewer: string | null;
  readonly name: (address: string) => string;
  readonly mode?: () => AnnounceMode;
  readonly delayMs?: number;
}

/** The mode chosen in Settings; "all" without one. */
export function documentAnnounceMode(): AnnounceMode {
  const value = typeof document === "undefined" ? null : document.documentElement.getAttribute("data-announce");
  return value === "replies" || value === "none" ? value : "all";
}

export class Announcer {
  /** The text of the live region. */
  readonly text: Signal<string> = signal("");
  private pending: string[] = [];
  private timer: ReturnType<typeof setTimeout> | null = null;
  /** Alternates a trailing space, so the same text twice is read twice. */
  private flip = false;

  constructor(private readonly options: AnnouncerOptions) {}

  /** A message someone else added (not yet in the structure). */
  add(blipId: string): void {
    this.pending.push(blipId);
    this.timer ??= setTimeout(() => this.flush(), this.options.delayMs ?? 2000);
  }

  flush(): void {
    this.timer = null;
    const ids = this.pending;
    this.pending = [];
    const arrivals = ids.map(this.options.describe).filter((a): a is Arrival => a !== null);
    const text = announcementText(arrivals, this.options.viewer, (this.options.mode ?? documentAnnounceMode)(), this.options.name);
    if (text !== null) {
      this.flip = !this.flip;
      this.text.value = this.flip ? text : `${text} `;
    }
  }

  dispose(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.pending = [];
  }
}
