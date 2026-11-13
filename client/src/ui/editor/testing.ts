// Helpers shared by the editor's tests.

import { EditorCore } from "./core";
import { type FakeDocument, createFakeDocument } from "./fakeDocument";

export interface CoreFixture {
  readonly doc: FakeDocument;
  readonly core: EditorCore;
  /** The body's content as XML, without the `<body>` wrapper. */
  body(): string;
  /** Puts the caret at a location, or selects a range. */
  select(anchor: number, focus?: number): void;
  time: number;
}

/** `content` is the inside of the body, e.g. `<line/>abc`. */
export function coreFixture(content = "<line/>"): CoreFixture {
  const doc = createFakeDocument(`<body>${content}</body>`);
  const fixture = {
    doc,
    time: 0,
    core: undefined as unknown as EditorCore,
    body: () => doc.xml().replace(/^<body>/, "").replace(/<\/body>$/, ""),
    select: (anchor: number, focus: number = anchor) => fixture.core.setSelection({ anchor, focus }),
  };
  fixture.core = new EditorCore(doc, { now: () => fixture.time });
  return fixture;
}

/** A seeded random number generator (mulberry32). */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
