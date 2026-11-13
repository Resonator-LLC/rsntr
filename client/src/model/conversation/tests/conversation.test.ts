// Tests of the conversation model: port of the Rust `conversation/tests.rs`,
// itself ported from `ConversationTestBase`, `WaveletBasedConversationTest`,
// `ConversationViewTestBase`, `WaveBasedConversationViewTest`,
// `DocumentBasedManifest*Test`, `BlipIteratorsTest`, `TitleHelperTest` and
// `TagsDocumentTest` of Apache Wave. Tests of the events are in
// `events.test.ts`.

import { describe, expect, test } from "vitest";

import {
  DocError,
  DocView,
  type MutableDocument,
  StandaloneDocument,
  XmlFragment,
} from "../../doc";
import { BootstrapDocument } from "../../docop";
import { IdGeneratorImpl, ParticipantId, WaveId, WaveletId } from "../../id";
import { WaveView, WaveletError } from "../../wavelet";
import {
  Conversation,
  type ConversationBlip,
  ConversationError,
  type ConversationThread,
  Manifest,
  WaveBasedConversationView,
  type WaveletBasedConversation,
  blips,
  conversationIdFor,
  makeWaveletConversational,
  manifest,
  readManifest,
  tags,
  title,
  waveletHasConversation,
} from "..";

type View = WaveBasedConversationView<IdGeneratorImpl>;

const alice = (): ParticipantId => ParticipantId.of("alice@example.com");
const bob = (): ParticipantId => ParticipantId.of("bob@example.com");

function emptyView(): View {
  const wave = new WaveView(WaveId.of("example.com", "w+wave"), alice(), { timestamp: 42 });
  return new WaveBasedConversationView(wave, new IdGeneratorImpl("example.com", () => "s"));
}

/** A view with an empty root conversation. */
function rootView(): View {
  const view = emptyView();
  view.createRoot();
  return view;
}

const rootId = (): WaveletId => WaveletId.of("example.com", "conv+root");

const root = (view: View): WaveletBasedConversation => view.rootMut();

const docXml = (view: View, docId: string): string =>
  view.wave.root()!.document(docId).toXmlString();

const manifestXml = (view: View): string => docXml(view, "conversation");

function editManifest(view: View, edit: (doc: MutableDocument) => void): void {
  view.wave.wavelet(rootId())!.editDocument("conversation", edit);
}

const blipIds = (thread: ConversationThread): string[] => thread.blips.map((blip) => blip.id);

const threadIds = (blip: ConversationBlip): string[] => blip.replyThreads.map((t) => t.id);

function located(view: View, blipId: string): [string, number | null][] {
  const conversation = view.root()!;
  return conversation
    .locateReplyThreads(conversation.blip(blipId)!)
    .map((reply) => [reply.thread.id, reply.location]);
}

function thrownBy(f: () => unknown): unknown {
  try {
    f();
  } catch (e) {
    return e;
  }
  throw new Error("expected the call to throw");
}

function conversationError(f: () => unknown): ConversationError {
  const e = thrownBy(f);
  expect(e).toBeInstanceOf(ConversationError);
  return e as ConversationError;
}

const topElement = (doc: MutableDocument) => doc.view().firstChildElement(doc.view().root())!;

describe("conversation view", () => {
  test("empty view has no conversations", () => {
    const view = emptyView();
    expect(view.conversations()).toEqual([]);
    expect(view.root()).toBeNull();
    expect(view.id()).toBe("example.com/w+wave");
  });

  test("created root accessible by get root", () => {
    const view = rootView();
    const conversation = view.root()!;
    expect(conversation.id).toBe("example.com/conv+root");
    expect(conversation.waveletId.equals(rootId())).toBe(true);
    expect(view.conversations().length).toBe(1);
    expect(conversation.participants.map((p) => p.address)).toEqual(["alice@example.com"]);
  });

  test("duplicate create root fails", () => {
    const view = rootView();
    const before = manifestXml(view);
    const e = thrownBy(() => view.createRoot());
    expect(e).toBeInstanceOf(WaveletError);
    expect((e as WaveletError).kind).toBe("duplicateWavelet");
    expect(manifestXml(view)).toBe(before);
  });

  test("create conversations", () => {
    const view = rootView();
    const first = view.createConversation().wavelet.waveletId();
    const second = view.createConversation().wavelet.waveletId();
    expect(first.equals(second)).toBe(false);
    expect(first.id.startsWith("conv+")).toBe(true);
    expect(first.equals(rootId())).toBe(false);
    expect(view.conversations().length).toBe(3);
    expect(view.conversation(first)).not.toBeNull();
    expect(view.conversationById(conversationIdFor(first))!.waveletId.equals(first)).toBe(true);
    expect(conversationError(() => view.conversationById("nonsense")).kind).toBe("invalidArgument");
  });

  test("non-existent conversation is null", () => {
    const view = rootView();
    expect(view.conversation(WaveletId.of("example.com", "conv+missing"))).toBeNull();
    const notConversational = WaveletId.of("example.com", "user+alice@example.com");
    expect(conversationError(() => view.conversation(notConversational)).kind).toBe(
      "invalidArgument",
    );
  });

  test("delete conversation from view", () => {
    const view = rootView();
    const blip = root(view).appendBlip("");
    root(view).delete();
    expect(view.root()).toBeNull();
    expect(view.conversations()).toEqual([]);
    expect(conversationError(() => view.rootMut()).kind).toBe("noManifest");
    expect(manifestXml(view)).toBe("");
    expect(docXml(view, blip)).toBe("");
    // The wavelet remains, and can be made conversational again.
    makeWaveletConversational(view.wave.wavelet(rootId())!);
    expect(view.root()).not.toBeNull();
  });

  test("manifest erasure removes conversation", () => {
    const view = rootView();
    editManifest(view, manifest.deleteManifest);
    expect(view.root()).toBeNull();
  });
});

describe("wavelets and manifests", () => {
  test("new wavelet has no conversation", () => {
    const view = emptyView();
    const wavelet = view.wave.createRoot();
    expect(waveletHasConversation(wavelet.data())).toBe(false);
    expect(Conversation.read(wavelet.data())).toBeNull();
    makeWaveletConversational(wavelet);
    expect(waveletHasConversation(wavelet.data())).toBe(true);
    expect(wavelet.document("conversation").toXmlString()).toBe("<conversation/>");
    // Not twice.
    expect(thrownBy(() => makeWaveletConversational(wavelet))).toBeInstanceOf(DocError);
  });

  test("extra conversation element does not fail", () => {
    const view = rootView();
    const blip = root(view).appendBlip("");
    editManifest(view, (doc) => void doc.createChildElement(doc.view().root(), "conversation"));
    // The first element stays the manifest.
    expect(blipIds(view.root()!.rootThread)).toEqual([blip]);
    const second = root(view).appendBlip("");
    expect(manifestXml(view)).toBe(
      `<conversation><blip id="${blip}"/><blip id="${second}"/></conversation><conversation/>`,
    );
  });

  test("missing anchor attributes means no anchor", () => {
    const view = rootView();
    const blip = root(view).appendBlip("");
    const other = view.createConversation().wavelet.waveletId();
    expect(view.anchorOf(other)).toBeNull();
    // Only one of the two attributes.
    view.wave
      .wavelet(other)!
      .editDocument("conversation", (doc) =>
        doc.setElementAttribute(topElement(doc), "anchorBlip", blip),
      );
    expect(view.anchorOf(other)).toBeNull();
    expect(view.conversation(other)!.anchor).toEqual({ conversationId: null, blipId: blip });
  });

  test("set and clear anchor updates manifest", () => {
    const view = rootView();
    const blip = root(view).appendBlip("");
    const other = view.createConversation().wavelet.waveletId();
    const otherManifest = (): string =>
      view.wave.wavelet(other)!.document("conversation").toXmlString();
    view.setAnchor(other, { waveletId: rootId(), blipId: blip });
    const anchor = view.anchorOf(other)!;
    expect([anchor.waveletId.equals(rootId()), anchor.blipId]).toEqual([true, blip]);
    expect(otherManifest()).toBe(
      `<conversation anchorBlip="${blip}" anchorWavelet="example.com/conv+root"/>`,
    );
    // The anchor survives more blips in the anchor conversation.
    root(view).appendBlip("");
    expect(view.anchorOf(other)!.blipId).toBe(blip);
    // An anchor at a blip that does not exist is no anchor.
    view.setAnchor(other, { waveletId: rootId(), blipId: "b+nope" });
    expect(view.anchorOf(other)).toBeNull();
    view.setAnchor(other, null);
    expect(view.anchorOf(other)).toBeNull();
    expect(otherManifest()).toBe("<conversation/>");
  });

  test("anchor to self fails", () => {
    const view = rootView();
    const blip = root(view).appendBlip("");
    expect(
      conversationError(() => view.setAnchor(rootId(), { waveletId: rootId(), blipId: blip })).kind,
    ).toBe("invalidArgument");
  });

  test("participants are a set", () => {
    const view = rootView();
    const addresses = (): string[] => view.root()!.participants.map((p) => p.address);
    root(view).addParticipant(bob());
    root(view).addParticipant(bob());
    expect(addresses()).toEqual(["alice@example.com", "bob@example.com"]);
    root(view).removeParticipant(bob());
    root(view).removeParticipant(bob());
    expect(addresses()).toEqual(["alice@example.com"]);
    // Create, add, add (no-op), remove, remove (no-op): add alice, manifest,
    // add bob, remove bob.
    expect(view.wave.root()!.ops().length).toBe(4);
  });

  test("blip metadata matches wavelet", () => {
    const view = rootView();
    const blip = root(view).appendBlip("");
    const data = view.root()!.blipData(blip)!;
    expect(data.author.equals(alice())).toBe(true);
    expect(data.contributors.map((p) => p.address)).toEqual(["alice@example.com"]);
    expect(data.lastModifiedTime).toBe(42);
    expect(data.content.toXmlString()).toBe("<body><line/></body>");
    expect(data.id).toBe(blip);
    expect(view.root()!.blipContent(blip)).toBe(data.content);
  });

  test("append blips updates manifest", () => {
    const view = rootView();
    expect(view.root()!.rootThread.blips).toEqual([]);
    expect(view.root()!.rootThread.firstBlip()).toBeNull();
    const b1 = root(view).appendBlip("");
    const b2 = root(view).appendBlip("");
    expect(manifestXml(view)).toBe(
      `<conversation><blip id="${b1}"/><blip id="${b2}"/></conversation>`,
    );
    const conversation = view.root()!;
    expect(blipIds(conversation.rootThread)).toEqual([b1, b2]);
    expect(conversation.isRootBlip(b1)).toBe(true);
    expect(conversation.isRootBlip(b2)).toBe(false);
    expect(conversation.threadOf(b1)!.id).toBe("");
    expect(conversation.blip("foobar")).toBeNull();
  });

  test("insert blip before first and between", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const b0 = root(view).insertBlip("", b1);
    const b2 = root(view).appendBlip("");
    const b15 = root(view).insertBlip("", b2);
    const conversation = view.root()!;
    expect(blipIds(conversation.rootThread)).toEqual([b0, b1, b15, b2]);
    expect(conversation.isRootBlip(b0)).toBe(true);
    // The successor must be in the thread.
    const thread = root(view).addReplyThread(b1);
    expect(conversationError(() => root(view).insertBlip(thread, b2)).kind).toBe("invalidArgument");
    expect(conversationError(() => root(view).insertBlip("", "b+nope")).kind).toBe(
      "invalidArgument",
    );
  });

  test("append replies updates manifest", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(b1);
    // An empty thread is a thread.
    expect(threadIds(view.root()!.blip(b1)!)).toEqual([t1]);
    const r1 = root(view).appendBlip(t1);
    // The thread took the id the next blip then got.
    expect(r1).toBe(t1);
    const r2 = root(view).appendBlip(t1);
    const t2 = root(view).addReplyThread(b1);
    expect(manifestXml(view)).toBe(
      `<conversation><blip id="${b1}"><thread id="${t1}"><blip id="${r1}"/>` +
        `<blip id="${r2}"/></thread><thread id="${t2}"/></blip></conversation>`,
    );
    const conversation = view.root()!;
    expect(blipIds(conversation.thread(t1)!)).toEqual([r1, r2]);
    expect(conversation.parentBlipOf(t1)!.id).toBe(b1);
    expect(conversation.parentBlipOf("")).toBeNull();
    expect(conversation.thread(t1)!.inline).toBe(false);
    expect(conversation.threadOf(r2)!.id).toBe(t1);
    expect(located(view, b1)).toEqual([
      [t1, null],
      [t2, null],
    ]);
  });

  test("append inline reply creates anchor", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    root(view).editBlip(b1, (doc) => doc.insertText(3, "0123456789"));
    const t1 = root(view).addInlineReplyThread(b1, 7);
    expect(docXml(view, b1)).toBe(`<body><line/>0123<reply id="${t1}"/>456789</body>`);
    expect(manifestXml(view)).toBe(
      `<conversation><blip id="${b1}"><thread id="${t1}" inline="true"/></blip></conversation>`,
    );
    expect(view.root()!.thread(t1)!.inline).toBe(true);
    expect(located(view, b1)).toEqual([[t1, 7]]);
    // The location follows the content.
    root(view).editBlip(b1, (doc) => doc.insertText(3, "abc"));
    expect(located(view, b1)).toEqual([[t1, 10]]);
    // An invalid location fails, leaving nothing behind.
    expect(thrownBy(() => root(view).addInlineReplyThread(b1, 1000))).toBeInstanceOf(DocError);
    expect(view.root()!.blip(b1)!.replyThreads.length).toBe(1);
  });

  test("inline reply with multiple anchors uses first", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    root(view).editBlip(b1, (doc) => doc.insertText(3, "0123456789"));
    const t1 = root(view).addInlineReplyThread(b1, 9);
    root(view).editBlip(b1, (doc) => void doc.createElement(5, "reply", [["id", t1]]));
    expect(located(view, b1)).toEqual([[t1, 5]]);
  });

  test("inline reply with deleted anchor has invalid location", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addInlineReplyThread(b1, 3);
    root(view).editBlip(b1, (doc) => doc.deleteNode(doc.view().elementStartingAt(3)!));
    expect(located(view, b1)).toEqual([[t1, null]]);
  });

  test("inline replies in location order", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    root(view).editBlip(b1, (doc) => doc.insertText(3, "0123456789"));
    const late = root(view).addInlineReplyThread(b1, 9);
    root(view).appendBlip(late);
    const none = root(view).addReplyThread(b1);
    root(view).appendBlip(none);
    const early = root(view).addInlineReplyThread(b1, 4);
    // Manifest order is creation order.
    expect(threadIds(view.root()!.blip(b1)!)).toEqual([late, none, early]);
    expect(located(view, b1)).toEqual([
      [early, 4],
      [late, 11],
      [none, null],
    ]);
  });

  test("delete single root thread blip removes it", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    root(view).deleteBlip(b1);
    expect(view.root()!.rootThread.blips).toEqual([]);
    expect(view.root()!.blip(b1)).toBeNull();
    expect(manifestXml(view)).toBe("<conversation/>");
    // The document remains, empty.
    expect(view.wave.root()!.hasDocument(b1)).toBe(true);
    expect(docXml(view, b1)).toBe("");
    // Deleting again: not found.
    expect(conversationError(() => root(view).deleteBlip(b1)).detail).toEqual({
      kind: "noSuchBlip",
      blipId: b1,
    });
    expect(conversationError(() => root(view).editBlip(b1, () => undefined)).kind).toBe(
      "noSuchBlip",
    );
    // Can append after deleting.
    const b2 = root(view).appendBlip("");
    expect(blipIds(view.root()!.rootThread)).toEqual([b2]);
  });

  test("delete blip in thread leaves siblings", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const b2 = root(view).appendBlip("");
    const b3 = root(view).appendBlip("");
    root(view).deleteBlip(b2);
    expect(blipIds(view.root()!.rootThread)).toEqual([b1, b3]);
  });

  test("delete last blip in non-root thread deletes thread", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(b1);
    const r1 = root(view).appendBlip(t1);
    const r2 = root(view).appendBlip(t1);
    root(view).deleteBlip(r1);
    expect(blipIds(view.root()!.thread(t1)!)).toEqual([r2]);
    root(view).deleteBlip(r2);
    expect(view.root()!.thread(t1)).toBeNull();
    expect(manifestXml(view)).toBe(`<conversation><blip id="${b1}"/></conversation>`);
    expect(conversationError(() => root(view).appendBlip(t1)).detail).toEqual({
      kind: "noSuchThread",
      threadId: t1,
    });
    // Can reply again.
    const t2 = root(view).addReplyThread(b1);
    root(view).appendBlip(t2);
    expect(threadIds(view.root()!.blip(b1)!)).toEqual([t2]);
  });

  test("delete blip with replies deletes replies", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const b2 = root(view).appendBlip("");
    root(view).editBlip(b1, (doc) => doc.insertText(3, "0123456789"));
    const t1 = root(view).addInlineReplyThread(b1, 5);
    const r1 = root(view).appendBlip(t1);
    const t2 = root(view).addReplyThread(b1);
    const r2 = root(view).appendBlip(t2);
    const r3 = root(view).appendBlip(t2);
    // A reply to the inline reply.
    const t3 = root(view).addReplyThread(r1);
    const r4 = root(view).appendBlip(t3);
    root(view).deleteBlip(b1);
    expect(manifestXml(view)).toBe(`<conversation><blip id="${b2}"/></conversation>`);
    for (const blip of [b1, r1, r2, r3, r4]) {
      expect(docXml(view, blip), blip).toBe("");
      expect(view.root()!.blip(blip)).toBeNull();
    }
    for (const thread of [t1, t2, t3]) {
      expect(view.root()!.thread(thread)).toBeNull();
    }
    expect(view.root()!.isRootBlip(b2)).toBe(true);
  });

  test("delete inline reply deletes anchor", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addInlineReplyThread(b1, 3);
    const r1 = root(view).appendBlip(t1);
    const t2 = root(view).addInlineReplyThread(b1, 3);
    root(view).appendBlip(t2);
    root(view).deleteBlip(r1);
    expect(docXml(view, b1)).toBe(`<body><line/><reply id="${t2}"/></body>`);
    root(view).deleteThread(t2);
    expect(docXml(view, b1)).toBe("<body><line/></body>");
    expect(manifestXml(view)).toBe(`<conversation><blip id="${b1}"/></conversation>`);
  });

  test("delete root thread removes all blips", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(b1);
    root(view).appendBlip(t1);
    root(view).appendBlip("");
    root(view).deleteThread("");
    expect(manifestXml(view)).toBe("<conversation/>");
    // The root thread remains usable.
    expect(view.root()!.rootThread.blips).toEqual([]);
    root(view).appendBlip("");
  });

  test("delete non-root and empty threads", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(b1);
    const r1 = root(view).appendBlip(t1);
    root(view).appendBlip(t1);
    const empty = root(view).addReplyThread(b1);
    root(view).deleteThread(t1);
    expect(view.root()!.blip(r1)).toBeNull();
    expect(threadIds(view.root()!.blip(b1)!)).toEqual([empty]);
    root(view).deleteThread(empty);
    expect(manifestXml(view)).toBe(`<conversation><blip id="${b1}"/></conversation>`);
    expect(conversationError(() => root(view).deleteThread(empty)).detail).toEqual({
      kind: "noSuchThread",
      threadId: empty,
    });
  });

  test("data documents", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    root(view).editDataDocument("some-doc", (doc) => doc.insertText(0, "x"));
    expect(docXml(view, "some-doc")).toBe("x");
    for (const name of [b1, "b+other", "conversation"]) {
      expect(conversationError(() => root(view).editDataDocument(name, () => undefined)).kind).toBe(
        "invalidArgument",
      );
    }
  });

  test("empty manifest thread not ignored", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(b1);
    const conversation = view.root()!;
    expect(conversation.rootThread.firstBlip()!.replyThreads[0]!.id).toBe(t1);
    expect(conversation.thread(t1)!.blips).toEqual([]);
  });

  test("blip missing from wavelet is skipped", () => {
    const view = rootView();
    // Only in the manifest.
    editManifest(
      view,
      (doc) => void doc.createChildElement(topElement(doc), "blip", [["id", "b+ghost"]]),
    );
    expect(view.root()!.rootThread.firstBlip()).toBeNull();
    expect(view.root()!.blip("b+ghost")).toBeNull();
    const first = root(view).appendBlip("");
    editManifest(
      view,
      (doc) => void doc.createChildElement(topElement(doc), "blip", [["id", "b+ghost2"]]),
    );
    const third = root(view).appendBlip("");
    expect(readManifest(view.wave.root()!.data())!.rootThread.blips.length).toBe(4);
    const conversation = view.root()!;
    expect(blipIds(conversation.rootThread)).toEqual([first, third]);
    // The first blip with a document is the root blip.
    expect(conversation.isRootBlip(first)).toBe(true);
    // Deleting the root thread deletes only the blips with documents.
    root(view).deleteThread("");
    expect(manifestXml(view)).toBe(
      '<conversation><blip id="b+ghost"/><blip id="b+ghost2"/></conversation>',
    );
  });

  test("concurrent deletion of final blips leaves empty thread", () => {
    const view = rootView();
    const first = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(first);
    const b1 = root(view).appendBlip(t1);
    const b2 = root(view).appendBlip(t1);
    root(view).deleteBlip(b1);
    // Someone else removes the last blip's manifest entry.
    const location = view.root()!.blip(b2)!.manifestLocation;
    editManifest(view, (doc) => doc.deleteNode(doc.view().elementStartingAt(location)!));
    const conversation = view.root()!;
    expect(conversation.blip(b2)).toBeNull();
    expect(conversation.thread(t1)!.blips).toEqual([]);
    expect(threadIds(conversation.blip(first)!)).toEqual([t1]);
    root(view).appendBlip("");
    expect(view.root()!.thread(t1)).not.toBeNull();
  });

  test("malformed manifest gives degraded view", () => {
    const view = rootView();
    const real = root(view).appendBlip("");
    editManifest(view, (doc) => {
      // Junk around and inside the manifest, a blip and a thread without
      // id, a blip under a blip, a thread under a thread.
      const xml = XmlFragment.parse(
        '<junk/><blip/><blip id="b+ghost"><thread id="t"><blip id="b+x"/></thread></blip>',
      );
      doc.insertXml(doc.view().size() - 1, xml);
      doc.insertText(1, "text");
      const blip = doc.view().elementWithTagName("blip")!;
      const inner = XmlFragment.parse(
        '<blip id="b+nested"/><thread><blip id="b+y"/></thread>' +
          '<thread id="ok" inline="TRUE"><thread id="deep"/></thread>',
      );
      doc.insertXml(doc.view().contentEnd(blip), inner);
    });
    const conversation = view.root()!;
    expect(blipIds(conversation.rootThread)).toEqual([real]);
    const blip = conversation.blip(real)!;
    // The thread without id is left out; "inline" is parsed leniently.
    expect(threadIds(blip)).toEqual(["ok"]);
    expect(blip.replyThreads[0]!.inline).toBe(true);
    expect(blip.replyThreads[0]!.blips).toEqual([]);
    expect(conversation.thread("deep")).toBeNull();
    expect(conversation.blipsBreadthFirst().map((b) => b.id)).toEqual([real]);
    // Mutations still work on what is there.
    root(view).appendBlip("ok");
    root(view).deleteBlip(real);
    expect(view.root()!.rootThread.blips).toEqual([]);
  });

  test("duplicate manifest entries", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    editManifest(
      view,
      (doc) => void doc.createChildElement(topElement(doc), "blip", [["id", b1]]),
    );
    const conversation = view.root()!;
    expect(blipIds(conversation.rootThread)).toEqual([b1, b1]);
    // The last entry wins.
    expect(conversation.blip(b1)!.manifestLocation).toBe(
      conversation.rootThread.blips[1]!.manifestLocation,
    );
    root(view).deleteBlip(b1);
    // The last entry is gone. The document still exists, though empty, so
    // the other entry still is a blip, as it would be in the original.
    expect(manifestXml(view)).toBe(`<conversation><blip id="${b1}"/></conversation>`);
    expect(blipIds(view.root()!.rootThread)).toEqual([b1]);
    expect(docXml(view, b1)).toBe("");
  });

  test("manifest read", () => {
    const view = rootView();
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addInlineReplyThread(b1, 3);
    const r1 = root(view).appendBlip(t1);
    const read = readManifest(view.wave.root()!.data())!;
    expect(read.anchor).toEqual({ conversationId: null, blipId: null });
    expect(read.rootThread.id).toBeNull();
    expect(read.rootThread.inline).toBe(false);
    const blip = read.rootThread.blips[0]!;
    expect(blip.id).toBe(b1);
    expect(blip.location).toBe(1);
    const thread = blip.threads[0]!;
    expect(thread.id).toBe(t1);
    expect(thread.inline).toBe(true);
    expect(thread.blips[0]!.id).toBe(r1);
    expect(thread.blips[0]!.threads).toEqual([]);
    expect(Manifest.read(new BootstrapDocument())).toBeNull();
  });
});

describe("blip iterators", () => {
  test("blips breadth first", () => {
    const view = rootView();
    const ids = (): string[] => view.root()!.blipsBreadthFirst().map((blip) => blip.id);
    expect(ids()).toEqual([]);
    const b1 = root(view).appendBlip("");
    const t1 = root(view).addReplyThread(b1);
    const b1t1b1 = root(view).appendBlip(t1);
    const t11 = root(view).addReplyThread(b1t1b1);
    const b1t1b1t1b1 = root(view).appendBlip(t11);
    const b1t1b2 = root(view).appendBlip(t1);
    const t2 = root(view).addReplyThread(b1);
    const b1t2b1 = root(view).appendBlip(t2);
    const b2 = root(view).appendBlip("");
    const t3 = root(view).addReplyThread(b2);
    const b2t1b1 = root(view).appendBlip(t3);
    expect(ids()).toEqual([b1, b2, b1t1b1, b1t1b2, b1t2b1, b2t1b1, b1t1b1t1b1]);
    // Depth first: a blip before its replies (not in the Rust port).
    expect(view.root()!.blipsDepthFirst().map((blip) => blip.id)).toEqual([
      b1,
      b1t1b1,
      b1t1b1t1b1,
      b1t1b2,
      b1t2b1,
      b2,
      b2t1b1,
    ]);
  });
});

describe("title helper", () => {
  const TITLE_KEY = title.TITLE_KEY;

  function doc(xml: string): StandaloneDocument {
    const d = new StandaloneDocument();
    d.mutable().insertXml(0, XmlFragment.parse(xml));
    return d;
  }

  const titleOf = (d: StandaloneDocument): string => title.extractTitle(d.document());
  const hasExplicit = (d: StandaloneDocument): boolean => title.hasExplicitTitle(d.document());
  const firstTitleChange = (d: StandaloneDocument): number | null =>
    d.document().firstAnnotationChange(0, new DocView(d.document()).size(), TITLE_KEY, null);

  test("empty title when no annotation", () => {
    const d = doc("<body><line/>Some text<line/>Some more text</body>");
    expect(titleOf(d)).toBe("");
    expect(hasExplicit(d)).toBe(false);
    expect(titleOf(doc("Just text"))).toBe("");
  });

  test("explicit value overrides", () => {
    const d = doc("<body><line/>Some text<line/>Some more text</body>");
    d.mutable().setAnnotation(2, 7, TITLE_KEY, "Blah");
    expect(titleOf(d)).toBe("Blah");
    expect(hasExplicit(d)).toBe(true);
  });

  test("empty value uses encompassed text", () => {
    const d = doc('<body><line id="1"/>Some text<line/>Some more text</body>');
    const startText = 3;
    for (let i = 0; i <= startText; i++) {
      d.mutable().resetAnnotation(i, startText + 7, TITLE_KEY, "");
      expect(titleOf(d)).toBe("Some te");
    }
    d.mutable().resetAnnotation(startText + 1, startText + 7, TITLE_KEY, "");
    expect(titleOf(d)).toBe("ome te");
    d.mutable().resetAnnotation(startText + 1, startText + 13, TITLE_KEY, "");
    expect(titleOf(d)).toBe("ome textSo");
    expect(hasExplicit(d)).toBe(false);
  });

  test("setting title changes expliciticity", () => {
    const d = doc('<body><line id="1"/>Some text<line/>Some mor</body>');
    const startText = 3;
    title.setExplicitTitle(d.mutable(), "Blah");
    expect(titleOf(d)).toBe("Blah");
    expect(hasExplicit(d)).toBe(true);
    title.setImplicitTitle(d.mutable(), startText + 1, startText + 7);
    expect(titleOf(d)).toBe("ome te");
    expect(hasExplicit(d)).toBe(false);
    title.setExplicitTitle(d.mutable(), "Blah2");
    expect(titleOf(d)).toBe("Blah2");
    expect(hasExplicit(d)).toBe(true);
    expect(thrownBy(() => title.setImplicitTitle(d.mutable(), 4, 4))).toBeInstanceOf(DocError);
    expect(
      thrownBy(() => title.setExplicitTitle(new StandaloneDocument().mutable(), "x")),
    ).toBeInstanceOf(DocError);
  });

  test("finding implicit title doesn't override explicit", () => {
    const d = doc("<body><line/>Some text<line/>Some more text</body>");
    title.setExplicitTitle(d.mutable(), "Blah");
    const ops = d.ops().length;
    title.maybeFindAndSetImplicitTitle(d.mutable());
    expect(d.ops().length).toBe(ops);
    expect(titleOf(d)).toBe("Blah");
    expect(hasExplicit(d)).toBe(true);
  });

  function implicitTitle(xml: string): [string, StandaloneDocument] {
    const d = doc(xml);
    title.maybeFindAndSetImplicitTitle(d.mutable());
    return [titleOf(d), d];
  }

  test("finding implicit title obeys sentence and first line boundaries", () => {
    const [first, d0] = implicitTitle(
      '<body><line id="1"/>Some text<line/>Some more. text?</body>',
    );
    expect(first).toBe("Some text");
    expect(hasExplicit(d0)).toBe(false);
    expect(firstTitleChange(d0)).toBe(1);
    const cases: [string, string][] = [
      ["Some text.", "Some text."],
      ["Some text!", "Some text!"],
      ["Some text!!", "Some text!!"],
      ["Some text!?.!", "Some text!?.!"],
      ["The number 5.5 is good", "The number 5.5 is good"],
      ["Some text.", "Some text. and more"],
      ["Some text!?.!", "Some text!?.! and more!"],
      ["Some text!?.!", "Some t<x/>ext!?.! and more!"],
      ["Some text!?.!", "Some text<x/>!?.! and more!"],
      ["Some text!?.!", "Some text!?.! a<x/>nd more!"],
      ["Some text!?.!", "Some te<x>xt!?.! a</x>nd more!"],
      ["Some 5.5 text!?.!", "Some 5.5 text!?.! and more!"],
    ];
    for (const [expected, firstLine] of cases) {
      const [found, d] = implicitTitle(`<body><line/>${firstLine}<line/>Some more text</body>`);
      expect(found, firstLine).toBe(expected);
      expect(firstTitleChange(d)).toBe(1);
      expect(hasExplicit(d)).toBe(false);
    }
    // An empty first line: the title is empty but marked.
    const [empty, d] = implicitTitle(
      "<body><line/><line/>Some text.and more<line/>Some more text</body>",
    );
    expect(empty).toBe("");
    expect(firstTitleChange(d)).toBe(1);
    expect(d.document().firstAnnotationChange(1, 100, TITLE_KEY, "")).toBe(3);
  });

  test("find implicit title", () => {
    const find = (xml: string) => title.findImplicitTitle(new DocView(doc(xml).document()));
    expect(find("<body><line/>Some text.<line/>Some more text</body>")).toEqual([1, 13]);
    expect(find("<body></body>")).toBeNull();
    expect(title.findImplicitTitle(new DocView(new BootstrapDocument()))).toBeNull();
    // Without a second line or sentence end: up to the last item.
    expect(find("<body><line/>Some</body>")).toEqual([1, 7]);
    const [start, end] = find(
      "<body><line/>a!xa!!!!!!!!!!!!!!!!!!!!!!!!!!!!xxxxxxxxxxxxxxxxxxxxxxx<line/>foo</body>",
    )!;
    expect(end).toBeGreaterThan(start);
  });

  test("finding implicit title clears from invalid documents", () => {
    const [found, d] = implicitTitle("<body><line/>Some text.<line/>Some more text</body>");
    expect(found).toBe("Some text.");
    const m = d.mutable();
    m.emptyElement(m.view().root());
    m.appendXml(XmlFragment.text("Blah. Blah").wrap("x"));
    title.maybeFindAndSetImplicitTitle(m);
    expect(titleOf(d)).toBe("");
    expect(hasExplicit(d)).toBe(false);
    expect(firstTitleChange(d)).toBeNull();
  });

  test("empty document with title", () => {
    const d = new BootstrapDocument();
    d.consume(title.emptyDocumentWithTitle().asOp());
    expect(d.toXmlString()).toBe('<body><?a "conv/title"=""?><line/></body><?a "conv/title"?>');
    const plain = new StandaloneDocument();
    plain.mutable().appendXml(blips.initialContent());
    // The same elements, annotations aside.
    expect(XmlFragment.fromInitialization(d.asOperation()).equals(blips.initialContent())).toBe(
      true,
    );
    expect(plain.document().toXmlString()).toBe("<body><line/></body>");
  });

  test("conversation title", () => {
    const view = rootView();
    expect(view.root()!.title()).toBe("");
    expect(conversationError(() => root(view).setExplicitTitle("x")).kind).toBe("noSuchBlip");
    const b1 = root(view).appendBlip("");
    root(view).editBlip(b1, (doc) => doc.insertText(3, "First line. Rest"));
    expect(view.root()!.title()).toBe("");
    root(view).updateImplicitTitle();
    expect(view.root()!.title()).toBe("First line.");
    root(view).setExplicitTitle("Named");
    expect(view.root()!.title()).toBe("Named");
  });
});

describe("tags document", () => {
  test("tags document", () => {
    const d = new StandaloneDocument();
    tags.addTag(d.mutable(), "tag1");
    expect(d.document().toXmlString()).toBe("<tag>tag1</tag>");
    tags.addTag(d.mutable(), "tag2");
    tags.insertTag(d.mutable(), "tag0", 0);
    tags.insertTag(d.mutable(), "tag15", 2);
    tags.insertTag(d.mutable(), "tag3", 10);
    expect(tags.tags(d.document())).toEqual(["tag0", "tag1", "tag15", "tag2", "tag3"]);
    expect(tags.deleteTag(d.mutable(), "tag15")).toBe(true);
    expect(tags.deleteTag(d.mutable(), "tag15")).toBe(false);
    expect(tags.tags(d.document())).toEqual(["tag0", "tag1", "tag2", "tag3"]);
    // Empty tag elements are not tags, and can be cleaned up.
    const m = d.mutable();
    m.createElement(0, "tag");
    m.createElement(m.size(), "tag");
    tags.deleteEmptyTags(m);
    expect(d.document().toXmlString()).toBe(
      "<tag>tag0</tag><tag>tag1</tag><tag>tag2</tag><tag>tag3</tag>",
    );
  });

  test("conversation tags", () => {
    const view = rootView();
    expect(view.root()!.tags()).toEqual([]);
    root(view).addTag("a");
    root(view).addTag("b");
    expect(root(view).removeTag("a")).toBe(true);
    expect(root(view).removeTag("a")).toBe(false);
    expect(view.root()!.tags()).toEqual(["b"]);
    expect(docXml(view, "tags")).toBe("<tag>b</tag>");
  });
});
