import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { type Arrival, Announcer, announcementText } from "./announce";

const name = (address: string): string => address.split("@")[0]!.replace(/^./, (c) => c.toUpperCase());
const ME = "me@x";

describe("announcementText", () => {
  it("names a single reply or new message", () => {
    expect(announcementText([{ author: "ben@x", parentAuthor: ME }], ME, "all", name)).toBe("Ben replied to you");
    expect(announcementText([{ author: "ben@x", parentAuthor: "ann@x" }], ME, "all", name)).toBe("Ben replied to Ann");
    expect(announcementText([{ author: "ben@x", parentAuthor: null }], ME, "all", name)).toBe("Ben wrote a new message");
  });

  it("sums up a burst", () => {
    const burst: Arrival[] = [
      { author: "ben@x", parentAuthor: null },
      { author: "ann@x", parentAuthor: null },
      { author: "ben@x", parentAuthor: ME },
    ];
    expect(announcementText(burst, ME, "all", name)).toBe("3 new messages from Ben and Ann");
    const many = ["a", "b", "c", "d"].map((n) => ({ author: `${n}@x`, parentAuthor: null }));
    expect(announcementText(many, ME, "all", name)).toBe("4 new messages from A, B and 2 others");
  });

  it("never announces own messages, and follows the mode", () => {
    expect(announcementText([{ author: ME, parentAuthor: null }], ME, "all", name)).toBeNull();
    const mixed: Arrival[] = [
      { author: "ben@x", parentAuthor: "ann@x" },
      { author: "ann@x", parentAuthor: ME },
    ];
    expect(announcementText(mixed, ME, "replies", name)).toBe("Ann replied to you");
    expect(announcementText(mixed, ME, "none", name)).toBeNull();
  });
});

describe("Announcer", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("batches arrivals for two seconds, and reads the same text twice", () => {
    const authors: Record<string, Arrival> = {
      b1: { author: "ben@x", parentAuthor: ME },
      b2: { author: "ann@x", parentAuthor: null },
    };
    const announcer = new Announcer({ describe: (id) => authors[id] ?? null, viewer: ME, name, mode: () => "all" });
    announcer.add("b1");
    announcer.add("b2");
    announcer.add("gone");
    expect(announcer.text.value).toBe("");
    vi.advanceTimersByTime(2000);
    expect(announcer.text.value).toBe("2 new messages from Ben and Ann");
    announcer.add("b2");
    vi.advanceTimersByTime(2000);
    announcer.add("b2");
    vi.advanceTimersByTime(2000);
    expect(announcer.text.value.trim()).toBe("Ann wrote a new message");
    announcer.dispose();
  });
});
