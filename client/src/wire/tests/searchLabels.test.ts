import { describe, expect, it } from "vitest";

import { Digest } from "../search";

const base = {
  title: "t",
  snippet: "s",
  waveId: "example.com/w+a",
  lastModified: 5n,
  unreadCount: 0,
  blipCount: 1,
  participants: [],
  author: "a@example.com",
};

describe("digest labels (field 9, not in the original)", () => {
  it("are written only when there are some", () => {
    const json = (d: Digest): unknown => JSON.parse(Digest.encode(d));
    expect(json(base)).not.toHaveProperty("9");
    expect(json({ ...base, labels: [] })).not.toHaveProperty("9");
    expect(json({ ...base, labels: [100, 7] })).toHaveProperty("9", [100, 7]);
  });

  it("are read when present and unset otherwise", () => {
    const back = (d: Digest): Digest => Digest.decode(JSON.parse(Digest.encode(d)));
    expect(back({ ...base, labels: [100] }).labels).toEqual([100]);
    expect(back(base).labels).toBeUndefined();
  });
});
