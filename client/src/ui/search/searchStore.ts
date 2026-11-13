// The logic of the search panel: the query, how many results are asked
// for, the repeated search, and the result merged with what is known
// better locally.
//
// Follows `SearchPresenter` and `SimpleSearch` of the Apache Wave web
// client (Apache License 2.0): the default query `in:inbox`, pages of 20
// ("show more" asks for 20 more from the start), a search every 15 seconds.
// Beyond the original, a search is also made when the connection comes
// back, when the page becomes visible again and when the shell says
// something changed; none is made while the page is hidden.

import { type ReadonlySignal, type Signal, batch, computed, signal } from "@preact/signals";

import type { Digest, SearchResponse } from "../../wire/search";
import { type LiveDigests, NO_LIVE_DIGESTS } from "./liveDigest";

/** `SearchPresenter.POLLING_INTERVAL_MS`. */
export const POLLING_INTERVAL_MS = 15_000;
/** `SearchPresenter.DEFAULT_SEARCH`. */
export const DEFAULT_SEARCH = "in:inbox";
/** `SearchPresenter.DEFAULT_PAGE_SIZE`. */
export const DEFAULT_PAGE_SIZE = 20;
/** `Search.UNKNOWN_SIZE`. */
export const UNKNOWN_SIZE = -1;
/**
 * How soon a search follows a hint that something changed: the delay of
 * the original's search after "New Wave", "to give enough time for the wave
 * state to propagate to the server".
 */
export const REFRESH_DELAY_MS = 500;
/** Hints that keep coming do not put the search off for longer than this. */
export const REFRESH_MAX_DELAY_MS = 3000;

/** `SearchPresenterMessages`. */
export const SEARCHING_TEXT = "Searching...";

export interface SearchTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

const DEFAULT_TIMERS: SearchTimers = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export interface SearchStoreOptions {
  /** `SearchService.search`. */
  search: (query: string, index: number, numResults: number) => Promise<SearchResponse>;
  live?: LiveDigests;
  timers?: SearchTimers;
  clock?: () => number;
  initialQuery?: string;
}

/** What the last search returned. */
export interface SearchResult {
  readonly query: string;
  readonly digests: readonly Digest[];
  /** The number of waves matching, or {@link UNKNOWN_SIZE}. */
  readonly total: number;
  /** How many results were asked for. */
  readonly asked: number;
}

/**
 * `searching`: a query the user entered has no result yet (the first
 * search, or a new query). `ready`: `result` is that of the current query.
 */
export type SearchStatus = "searching" | "ready";

export class SearchStore {
  /** The query being shown. */
  readonly query: Signal<string>;
  readonly status: Signal<SearchStatus> = signal<SearchStatus>("searching");
  /** The last result; kept while a new query is searched and after a failure. */
  readonly result: Signal<SearchResult | null> = signal<SearchResult | null>(null);
  /** The last search failed. */
  readonly failed: Signal<boolean> = signal(false);
  /** More results were asked for and have not arrived. */
  readonly loadingMore: Signal<boolean> = signal(false);
  /** The digests to show: the result, with live digests in place. */
  readonly digests: ReadonlySignal<readonly Digest[]>;

  private readonly search: SearchStoreOptions["search"];
  private readonly live: LiveDigests;
  private readonly timers: SearchTimers;
  private readonly clock: () => number;
  /** Number of results to query for (`querySize`). */
  private querySize = DEFAULT_PAGE_SIZE;
  /** Identifies the newest request: older answers are dropped. */
  private sequence = 0;
  private inFlight = 0;
  private pollTimer: unknown = null;
  private refreshTimer: unknown = null;
  private refreshDeadline: number | null = null;
  private running = false;
  private visible = true;
  /** A search was due while the page was hidden. */
  private missed = false;

  constructor(options: SearchStoreOptions) {
    this.search = options.search;
    this.live = options.live ?? NO_LIVE_DIGESTS;
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.clock = options.clock ?? Date.now;
    this.query = signal(options.initialQuery ?? DEFAULT_SEARCH);
    this.digests = computed(() => {
      // Subscribes to the live digests.
      void this.live.version.value;
      const result = this.result.value;
      if (result === null) {
        return [];
      }
      return result.digests.map((digest) => this.live.get(digest.waveId) ?? digest);
    });
  }

  /** How many results are asked for now. */
  get pageSize(): number {
    return this.querySize;
  }

  // ---- life cycle ----

  /** Searches now and then every {@link POLLING_INTERVAL_MS}. */
  start(): void {
    if (this.running) {
      return;
    }
    this.running = true;
    this.doSearch();
  }

  stop(): void {
    this.running = false;
    this.sequence++;
    this.inFlight = 0;
    this.cancelPoll();
    this.cancelRefresh();
  }

  /** The page was hidden or shown: nothing is searched while it is hidden. */
  setVisible(visible: boolean): void {
    if (this.visible === visible) {
      return;
    }
    this.visible = visible;
    if (visible && this.running && this.missed) {
      this.doSearch();
    }
  }

  // ---- gestures ----

  /** `onQueryEntered`: a new query, from its first page. */
  setQuery(query: string): void {
    batch(() => {
      this.query.value = query;
      this.querySize = DEFAULT_PAGE_SIZE;
      this.status.value = "searching";
      this.loadingMore.value = false;
    });
    this.doSearch();
  }

  /** `onShowMoreClicked`. */
  showMore(): void {
    this.querySize += DEFAULT_PAGE_SIZE;
    this.loadingMore.value = true;
    this.doSearch();
  }

  /** Searches again now (the retry button, the connection is back). */
  refresh(): void {
    if (this.running) {
      this.doSearch();
    }
  }

  /**
   * Searches again soon: something may have changed. Calls in quick
   * succession make one search.
   */
  refreshSoon(): void {
    if (!this.running) {
      return;
    }
    const now = this.clock();
    if (this.refreshDeadline === null) {
      this.refreshDeadline = now + REFRESH_MAX_DELAY_MS;
    }
    if (this.refreshTimer !== null) {
      this.timers.clearTimeout(this.refreshTimer);
    }
    const delay = Math.max(0, Math.min(REFRESH_DELAY_MS, this.refreshDeadline - now));
    this.refreshTimer = this.timers.setTimeout(() => {
      this.refreshTimer = null;
      this.refreshDeadline = null;
      this.doSearch();
    }, delay);
  }

  // ---- searching ----

  private cancelPoll(): void {
    if (this.pollTimer !== null) {
      this.timers.clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  private cancelRefresh(): void {
    if (this.refreshTimer !== null) {
      this.timers.clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
    this.refreshDeadline = null;
  }

  private schedulePoll(): void {
    this.cancelPoll();
    if (!this.running) {
      return;
    }
    this.pollTimer = this.timers.setTimeout(() => {
      this.pollTimer = null;
      this.poll();
    }, POLLING_INTERVAL_MS);
  }

  /** The repeated search: skipped while one is under way. */
  private poll(): void {
    if (this.inFlight > 0) {
      this.schedulePoll();
    } else {
      this.doSearch();
    }
  }

  /** `doSearch`: the current query from the start, `querySize` results. */
  private doSearch(): void {
    this.cancelRefresh();
    this.schedulePoll();
    if (!this.visible) {
      this.missed = true;
      return;
    }
    this.missed = false;
    const sequence = ++this.sequence;
    const query = this.query.value;
    const asked = this.querySize;
    this.live.sync();
    const mark = this.live.mark();
    this.inFlight++;
    this.search(query, 0, asked).then(
      (response) => {
        if (sequence !== this.sequence) {
          return;
        }
        this.inFlight = 0;
        this.live.sync();
        this.live.settle(mark);
        batch(() => {
          this.result.value = {
            query,
            digests: response.digests,
            total: response.totalResults,
            asked,
          };
          this.status.value = "ready";
          this.failed.value = false;
          this.loadingMore.value = false;
        });
      },
      () => {
        if (sequence !== this.sequence) {
          return;
        }
        this.inFlight = 0;
        batch(() => {
          this.failed.value = true;
          this.loadingMore.value = false;
          const result = this.result.value;
          if (result !== null && result.query !== query) {
            // The results of another query are not this query's.
            this.result.value = null;
          }
          if (result !== null && result.query === query) {
            // What is shown is still the last answer for this query.
            this.status.value = "ready";
            this.querySize = Math.max(DEFAULT_PAGE_SIZE, result.asked);
          }
        });
      },
    );
  }
}

/** `renderShowMore`: there may be more results than were asked for. */
export function hasMore(result: SearchResult | null): boolean {
  return result !== null && (result.total === UNKNOWN_SIZE || result.asked < result.total);
}

/**
 * The line about the results (`renderTitle`: "in:inbox (0-20 of unknown)"),
 * counting from one and up to what is shown: "1-20 of unknown", "1-7 of 7".
 */
export function resultRange(result: SearchResult | null, status: SearchStatus): string {
  if (status === "searching" || result === null) {
    return SEARCHING_TEXT;
  }
  const shown = result.digests.length;
  if (shown === 0) {
    return "No waves";
  }
  // `SearchPresenterMessages.of` / `ofUnknown`.
  const total = result.total === UNKNOWN_SIZE ? "of unknown" : `of ${Math.max(result.total, shown)}`;
  return `1-${shown} ${total}`;
}
