import { describe, expect, it } from "vitest";

import { matchesMention, mentionCandidates, mentionLabel } from "../MentionPopup";

describe("mention suggestions", () => {
  const participants = [
    { address: "me@example.com", name: "Me Myself" },
    { address: "ann@example.com", name: "Ann Lee" },
    { address: "bob@example.com", name: "Bob Stone" },
  ];

  it("match names by any word and addresses by their start", () => {
    expect(matchesMention("", "Ann Lee", "ann@example.com")).toBe(true);
    expect(matchesMention("le", "Ann Lee", "ann@example.com")).toBe(true);
    expect(matchesMention("Ann L", "Ann Lee", "ann@example.com")).toBe(true);
    expect(matchesMention("ann@", "Ann Lee", "ann@example.com")).toBe(true);
    expect(matchesMention("nn", "Ann Lee", "ann@example.com")).toBe(false);
  });

  it("list participants first, then the directory, never the viewer twice or at all", () => {
    const found = [
      { address: "bob@example.com", name: "Bob Stone", kind: "human" as const },
      { address: "helper@example.com", name: "Helper", kind: "agent" as const },
      { address: "me@example.com", name: "Me Myself", kind: "human" as const },
    ];
    const list = mentionCandidates("", participants, found, "me@example.com", null);
    expect(list.map((c) => [c.address, c.participant])).toEqual([
      ["ann@example.com", true],
      ["bob@example.com", true],
      ["helper@example.com", false],
    ]);
    expect(mentionCandidates("st", participants, [], "me@example.com", null).map((c) => c.address)).toEqual([
      "bob@example.com",
    ]);
  });

  it("offer a full address of another server last", () => {
    const list = mentionCandidates("zed@other.org", participants, [], "me@example.com", "zed@other.org");
    expect(list).toEqual([
      { address: "zed@other.org", name: "zed@other.org", kind: "human", participant: false, remote: true },
    ]);
  });

  it("label a mention with the name, else the local part", () => {
    expect(mentionLabel("Ann Lee", "ann@example.com")).toBe("Ann Lee");
    expect(mentionLabel("ann@example.com", "ann@example.com")).toBe("ann");
    expect(mentionLabel("  ", "zed@other.org")).toBe("zed");
  });
});
