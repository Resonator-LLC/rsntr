import { signal } from "@preact/signals";
import { beforeEach, describe, expect, test } from "vitest";

import type { Digest, SearchResponse } from "../../wire/search";
import type { LiveDigests } from "./liveDigest";
import {
  DEFAULT_PAGE_SIZE,
  POLLING_INTERVAL_MS,
  REFRESH_DELAY_MS,
  REFRESH_MAX_DELAY_MS,
  SearchStore,
  type SearchTimers,
  hasMore,
  resultRange,
} from "./searchStore";

class FakeTimers implements SearchTimers {
  time = 0;
  private next = 1;
  private readonly tasks = new Map<number, { at: number; run: () => void }>();

  setTimeout(callback: () => void, delayMs: number): unknown {
    const handle = this.next++;
    this.tasks.set(handle, { at: this.time + delayMs, run: callback });
    return handle;
  }

  clearTimeout(handle: unknown): void {
    this.tasks.delete(handle as number);
  }

  pending(): number {
    return this.tasks.size;
  }

  tick(ms: number): void {
    const end = this.time + ms;
    for (;;) {
      let due: [number, { at: number; run: () => void }] | null = null;
      for (const entry of this.tasks) {
        if (entry[1].at <= end && (due === null || entry[1].at < due[1].at)) {
          due = entry;
        }
      }
      if (due === null) {
        break;
      }
      this.tasks.delete(due[0]);
      this.time = due[1].at;
      due[1].run();
    }
    this.time = end;
  }
}

function digest(n: number, extra: Partial<Digest> = {}): Digest {
  return {
    title: `Wave ${n}`,
    snippet: `snippet ${n}`,
    waveId: `example.com/w+${n}`,
    lastModified: BigInt(1000 * n),
    unreadCount: 0,
    blipCount: 1,
    participants: [],
    author: "alice@example.com",
    ...extra,
  };
}

interface Call {
  query: string;
  index: number;
  numResults: number;
  resolve: (response: SearchResponse) => void;
  reject: (error: unknown) => void;
}

/** A search service whose answers the test gives. */
class FakeSearch {
  calls: Call[] = [];
  /** The waves there are, for `answer`. */
  waves = 0;

  search = (query: string, index: number, numResults: number): Promise<SearchResponse> =>
    new Promise((resolve, reject) => {
      this.calls.push({ query, index, numResults, resolve, reject });
    });

  get last(): Call {
    return this.calls[this.calls.length - 1]!;
  }

  /** Answers the newest call as the server would, from `waves` waves. */
  async answer(call: Call = this.last): Promise<void> {
    const count = Math.min(this.waves, call.numResults);
    call.resolve({
      query: call.query,
      // `SearchServlet`: unknown if as many came back as were asked for.
      totalResults: count >= call.numResults ? -1 : count,
      digests: Array.from({ length: count }, (_, i) => digest(i + 1)),
    });
    await settle();
  }

  async fail(call: Call = this.last): Promise<void> {
    call.reject(new Error("no network"));
    await settle();
  }
}

const settle = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

describe("SearchStore", () => {
  let timers: FakeTimers;
  let service: FakeSearch;
  let store: SearchStore;

  beforeEach(() => {
    timers = new FakeTimers();
    service = new FakeSearch();
    store = new SearchStore({ search: service.search, timers, clock: () => timers.time });
  });

  test("starts with the inbox, the first twenty", async () => {
    expect(store.query.value).toBe("in:inbox");
    expect(store.status.value).toBe("searching");
    expect(resultRange(store.result.value, store.status.value)).toBe("Searching...");
    store.start();
    expect(service.calls).toHaveLength(1);
    expect(service.last).toMatchObject({ query: "in:inbox", index: 0, numResults: 20 });
    service.waves = 3;
    await service.answer();
    expect(store.status.value).toBe("ready");
    expect(store.digests.value.map((d) => d.waveId)).toEqual([
      "example.com/w+1",
      "example.com/w+2",
      "example.com/w+3",
    ]);
    expect(resultRange(store.result.value, store.status.value)).toBe("1-3 of 3");
    expect(hasMore(store.result.value)).toBe(false);
  });

  test("searches again every fifteen seconds", async () => {
    store.start();
    await service.answer();
    timers.tick(POLLING_INTERVAL_MS - 1);
    expect(service.calls).toHaveLength(1);
    timers.tick(1);
    expect(service.calls).toHaveLength(2);
    // The status does not go back to "searching" for a repeated search.
    expect(store.status.value).toBe("ready");
    service.waves = 1;
    await service.answer();
    expect(store.digests.value).toHaveLength(1);
    timers.tick(POLLING_INTERVAL_MS);
    expect(service.calls).toHaveLength(3);
  });

  test("a repetition is skipped while a search is under way", () => {
    store.start();
    timers.tick(POLLING_INTERVAL_MS * 3);
    expect(service.calls).toHaveLength(1);
  });

  test("paging: show more asks for twenty more from the start", async () => {
    service.waves = 45;
    store.start();
    await service.answer();
    expect(store.digests.value).toHaveLength(20);
    expect(resultRange(store.result.value, store.status.value)).toBe("1-20 of unknown");
    expect(hasMore(store.result.value)).toBe(true);

    store.showMore();
    expect(store.loadingMore.value).toBe(true);
    expect(service.last).toMatchObject({ index: 0, numResults: 40 });
    // The list stays while more is loaded.
    expect(store.digests.value).toHaveLength(20);
    await service.answer();
    expect(store.loadingMore.value).toBe(false);
    expect(store.digests.value).toHaveLength(40);
    expect(hasMore(store.result.value)).toBe(true);

    store.showMore();
    await service.answer();
    expect(store.digests.value).toHaveLength(45);
    expect(resultRange(store.result.value, store.status.value)).toBe("1-45 of 45");
    expect(hasMore(store.result.value)).toBe(false);

    // The repeated search keeps the size.
    timers.tick(POLLING_INTERVAL_MS);
    expect(service.last).toMatchObject({ index: 0, numResults: 60 });
  });

  test("a new query starts from the first page and shows that it searches", async () => {
    service.waves = 45;
    store.start();
    await service.answer();
    store.showMore();
    await service.answer();

    store.setQuery("with:bob");
    expect(store.status.value).toBe("searching");
    expect(store.pageSize).toBe(DEFAULT_PAGE_SIZE);
    expect(service.last).toMatchObject({ query: "with:bob", index: 0, numResults: 20 });
    service.waves = 0;
    await service.answer();
    expect(store.status.value).toBe("ready");
    expect(store.digests.value).toEqual([]);
    expect(resultRange(store.result.value, store.status.value)).toBe("No waves");
  });

  test("an answer that was overtaken is dropped", async () => {
    store.start();
    const first = service.last;
    store.setQuery("with:bob");
    const second = service.last;
    service.waves = 2;
    await service.answer(second);
    service.waves = 5;
    await service.answer(first);
    expect(store.result.value?.query).toBe("with:bob");
    expect(store.digests.value).toHaveLength(2);
  });

  test("a failure keeps the last results and can be retried", async () => {
    service.waves = 2;
    store.start();
    await service.answer();
    timers.tick(POLLING_INTERVAL_MS);
    await service.fail();
    expect(store.failed.value).toBe(true);
    expect(store.status.value).toBe("ready");
    expect(store.digests.value).toHaveLength(2);

    store.refresh();
    await service.answer();
    expect(store.failed.value).toBe(false);
    // And the repetition goes on.
    const calls = service.calls.length;
    timers.tick(POLLING_INTERVAL_MS);
    expect(service.calls).toHaveLength(calls + 1);
  });

  test("the first search failing leaves no results; a failed new query drops the old ones", async () => {
    store.start();
    await service.fail();
    expect(store.failed.value).toBe(true);
    expect(store.result.value).toBeNull();

    service.waves = 2;
    store.refresh();
    await service.answer();
    store.setQuery("with:bob");
    await service.fail();
    expect(store.failed.value).toBe(true);
    expect(store.result.value).toBeNull();
  });

  test("a failed show more can be asked again", async () => {
    service.waves = 45;
    store.start();
    await service.answer();
    store.showMore();
    await service.fail();
    expect(store.loadingMore.value).toBe(false);
    expect(store.pageSize).toBe(20);
    expect(store.digests.value).toHaveLength(20);
    store.showMore();
    expect(service.last.numResults).toBe(40);
  });

  test("hints of a change make one search soon", async () => {
    store.start();
    await service.answer();
    store.refreshSoon();
    timers.tick(REFRESH_DELAY_MS - 100);
    store.refreshSoon();
    timers.tick(REFRESH_DELAY_MS - 1);
    expect(service.calls).toHaveLength(1);
    timers.tick(1);
    expect(service.calls).toHaveLength(2);
    await service.answer();
    // The repetition counts from that search.
    timers.tick(POLLING_INTERVAL_MS - 1);
    expect(service.calls).toHaveLength(2);
    timers.tick(1);
    expect(service.calls).toHaveLength(3);
  });

  test("hints that keep coming do not put the search off for ever", async () => {
    store.start();
    await service.answer();
    for (let elapsed = 0; elapsed < REFRESH_MAX_DELAY_MS; elapsed += 100) {
      store.refreshSoon();
      timers.tick(100);
    }
    expect(service.calls).toHaveLength(2);
  });

  test("nothing is searched while the page is hidden; one search when it shows again", async () => {
    store.start();
    await service.answer();
    store.setVisible(false);
    timers.tick(POLLING_INTERVAL_MS * 4);
    store.refreshSoon();
    timers.tick(REFRESH_DELAY_MS);
    expect(service.calls).toHaveLength(1);
    store.setVisible(true);
    expect(service.calls).toHaveLength(2);
    // Shown again without anything missed: no search.
    await service.answer();
    store.setVisible(false);
    store.setVisible(true);
    expect(service.calls).toHaveLength(2);
  });

  test("stop ends the repetition and drops the answer under way", async () => {
    store.start();
    const call = service.last;
    store.stop();
    service.waves = 3;
    await service.answer(call);
    expect(store.result.value).toBeNull();
    timers.tick(POLLING_INTERVAL_MS * 2);
    expect(service.calls).toHaveLength(1);
    expect(timers.pending()).toBe(0);
    store.refresh();
    store.refreshSoon();
    expect(timers.pending()).toBe(0);
  });

  test("live digests take the place of the server's, and follow their changes", async () => {
    const version = signal(0);
    const liveDigests = new Map<string, Digest>();
    const log: string[] = [];
    let clock = 0;
    const live: LiveDigests = {
      version,
      get: (waveId) => liveDigests.get(waveId),
      sync: () => log.push("sync"),
      mark: () => {
        log.push("mark");
        return ++clock;
      },
      settle: (mark) => log.push(`settle ${mark}`),
      dispose: () => undefined,
    };
    store = new SearchStore({ search: service.search, timers, clock: () => timers.time, live });
    service.waves = 3;
    store.start();
    expect(log).toEqual(["sync", "mark"]);
    await service.answer();
    expect(log).toEqual(["sync", "mark", "sync", "settle 1"]);
    expect(store.digests.value[1]!.title).toBe("Wave 2");

    liveDigests.set("example.com/w+2", digest(2, { title: "Edited here", unreadCount: 4 }));
    version.value++;
    expect(store.digests.value.map((d) => d.title)).toEqual(["Wave 1", "Edited here", "Wave 3"]);
    expect(store.digests.value[1]!.unreadCount).toBe(4);
    // A wave that is open but not in the result is not added.
    liveDigests.set("example.com/w+99", digest(99));
    version.value++;
    expect(store.digests.value).toHaveLength(3);
    // The order is the server's.
    liveDigests.delete("example.com/w+2");
    version.value++;
    expect(store.digests.value[1]!.title).toBe("Wave 2");
  });
});
