// Tests of the conversation events (`ObservableConversation` and the events
// of `WaveBasedConversationView`): every mutation reports the same events
// on the replica that makes it and, through `consumeRemote`, on another.

import { describe, expect, test } from "vitest";

import { WaveletData } from "../../data";
import { XmlFragment } from "../../doc";
import { Attributes, DocOpBuilder, toConciseString } from "../../docop";
import { IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName } from "../../id";
import { WaveletOperation, WaveletOperationContext } from "../../operation";
import { HashedVersionFactory } from "../../version";
import { type OpBasedWavelet, WaveView } from "../../wavelet";
import {
  type ConversationEvents,
  ObservableConversation,
  WaveBasedConversationView,
  type WaveletBasedConversation,
  manifest,
  title,
} from "..";

const alice = (): ParticipantId => ParticipantId.of("alice@example.com");
const bob = (): ParticipantId => ParticipantId.of("bob@example.com");
const WAVE = WaveId.of("example.com", "w+wave");
const ROOT = WaveletId.of("example.com", "conv+root");
/** The content of a new blip. */
const XML = XmlFragment.empty().wrap("line").wrap("body");
const attrs = (record: Record<string, string>): Attributes => Attributes.fromRecord(record);

/** Records every event of a conversation as a compact string. */
function log(conversation: ObservableConversation, out: string[] = []): string[] {
  const where = (local: boolean): string => (local ? "local" : "remote");
  const format: { [K in keyof ConversationEvents]: (e: ConversationEvents[K]) => string } = {
    participantAdded: (e) => `+participant ${e.participant.name()} by ${e.author.name()} ${where(e.local)}`,
    participantRemoved: (e) => `-participant ${e.participant.name()} by ${e.author.name()} ${where(e.local)}`,
    blipAdded: (e) => `+blip ${e.blipId} in [${e.threadId}] at ${e.index} ${where(e.local)}`,
    blipDeleted: (e) => `-blip ${e.blipId} in [${e.threadId}] ${where(e.local)}`,
    threadAdded: (e) =>
      `+thread ${e.threadId} under ${e.parentBlipId}${e.inline ? " inline" : ""} ${where(e.local)}`,
    inlineThreadAdded: (e) =>
      `+anchor ${e.threadId} in ${e.parentBlipId} at ${e.location} ${where(e.local)}`,
    inlineThreadRemoved: (e) => `-anchor ${e.threadId} in ${e.parentBlipId} ${where(e.local)}`,
    threadDeleted: (e) => `-thread ${e.threadId} under ${e.parentBlipId} ${where(e.local)}`,
    blipContentChanged: (e) =>
      `content ${e.blipId} ${toConciseString(e.op)} by ${e.author.name()} ${where(e.local)}`,
    blipContributorAdded: (e) => `+contributor ${e.blipId} ${e.contributor.name()}`,
    blipContributorRemoved: (e) => `-contributor ${e.blipId} ${e.contributor.name()}`,
    blipTimestampChanged: (e) => `time ${e.blipId} ${e.oldTime}->${e.newTime}`,
    titleChanged: (e) => `title ${JSON.stringify(e.oldTitle)}->${JSON.stringify(e.title)}`,
    tagsChanged: (e) => `tags +[${e.added}] -[${e.removed}] =[${e.tags}]`,
    anchorChanged: (e) =>
      `anchor ${e.oldAnchor.conversationId}:${e.oldAnchor.blipId}->` +
      `${e.newAnchor.conversationId}:${e.newAnchor.blipId}`,
    structureChanged: (e) => `structure ${where(e.local)}`,
  };
  for (const type of Object.keys(format) as (keyof ConversationEvents)[]) {
    conversation.events.on(type, (event) =>
      out.push((format[type] as (e: unknown) => string)(event)),
    );
  }
  return out;
}

/** An operation as the server would send it on: with a version increment. */
function fromServer(op: WaveletOperation): WaveletOperation {
  const context = new WaveletOperationContext(op.context.creator, op.context.timestamp, 1);
  return op.cloneOp(context);
}

/**
 * Two replicas of a wave with a root conversation: alice's, which makes the
 * changes, and bob's, which receives them as remote operations.
 */
class Replicas {
  readonly view: WaveBasedConversationView;
  readonly local: ObservableConversation;
  readonly remote: ObservableConversation;
  readonly remoteWavelet: OpBasedWavelet;
  readonly localEvents: string[];
  readonly remoteEvents: string[];
  private time = 1000;

  constructor() {
    const name = WaveletName.of(WAVE, ROOT);
    const empty = (): WaveletData =>
      WaveletData.empty(name, alice(), HashedVersionFactory.createVersionZero(name), 0);
    const there = new WaveView(WAVE, bob());
    this.remoteWavelet = there.addWavelet(empty(), { record: false });
    const here = new WaveView(WAVE, alice(), {
      clock: () => this.time,
      waveletOptions: () => ({
        record: false,
        output: (op) => this.remoteWavelet.consumeRemote(fromServer(op)),
      }),
    });
    here.addWavelet(empty());
    this.view = new WaveBasedConversationView(here, new IdGeneratorImpl("example.com", () => "s"));
    this.local = this.view.observeRoot()!;
    this.remote = new ObservableConversation(this.remoteWavelet);
    this.localEvents = log(this.local);
    this.remoteEvents = log(this.remote);
    here.root()!.addParticipant(alice());
    here.root()!.editDocument("conversation", manifest.initialiseDocumentManifest);
    this.take();
  }

  get c(): WaveletBasedConversation {
    return this.view.rootMut();
  }

  tick(): void {
    this.time += 1000;
  }

  /**
   * The events since the last take, after checking that the remote replica
   * saw the same ones and has the same documents.
   */
  take(): string[] {
    const local = this.localEvents.splice(0);
    const remote = this.remoteEvents.splice(0);
    expect(remote).toEqual(local.map((line) => line.replace(/ local$/, " remote")));
    const here = this.view.wave.root()!.data();
    const there = this.remoteWavelet.data();
    expect(there.documentIds()).toEqual(here.documentIds());
    for (const doc of here.documents()) {
      expect(there.document(doc.id)!.content.toXmlString(), doc.id).toBe(doc.content.toXmlString());
    }
    return local;
  }
}

describe("structure events", () => {
  test("blips and threads appear", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    expect(r.take()).toEqual([`+blip ${b1} in [] at 0 local`, "structure local"]);
    const b2 = r.c.appendBlip("");
    expect(r.take()).toEqual([`+blip ${b2} in [] at 1 local`, "structure local"]);
    const b0 = r.c.insertBlip("", b2);
    expect(r.take()).toEqual([`+blip ${b0} in [] at 1 local`, "structure local"]);
    const t1 = r.c.addReplyThread(b1);
    expect(r.take()).toEqual([`+thread ${t1} under ${b1} local`, "structure local"]);
    const r1 = r.c.appendBlip(t1);
    expect(r.take()).toEqual([`+blip ${r1} in [${t1}] at 0 local`, "structure local"]);
    r.tick();
    r.c.editBlip(b1, (doc) => doc.insertText(3, "0123456789"));
    expect(r.take()).toEqual([
      `content ${b1} __3; ++"0123456789"; __1;  by alice local`,
      `time ${b1} 1000->2000`,
    ]);
    const t2 = r.c.addInlineReplyThread(b1, 7);
    expect(r.take()).toEqual([
      `content ${b1} __7; << reply { id="${t2}" }; >>; __7;  by alice local`,
      `+thread ${t2} under ${b1} inline local`,
      `+anchor ${t2} in ${b1} at 7 local`,
      "structure local",
    ]);
    // What a listener reads during an event is the state after the change.
    const seen: string[] = [];
    r.local.events.on("blipAdded", ({ blipId, threadId }) => {
      const conversation = r.local.read()!;
      seen.push(`${conversation.threadOf(blipId)!.id}=${threadId}`);
      seen.push(conversation.blipContent(blipId)!.toXmlString());
    });
    const r2 = r.c.appendBlip(t2);
    expect(seen).toEqual([`${t2}=${t2}`, "<body><line/></body>"]);
    expect(r.take()).toEqual([`+blip ${r2} in [${t2}] at 0 local`, "structure local"]);
  });

  test("deleting reports replies bottom-up", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    const b2 = r.c.appendBlip("");
    r.c.editBlip(b1, (doc) => doc.insertText(3, "0123456789"));
    const t1 = r.c.addInlineReplyThread(b1, 5);
    const r1 = r.c.appendBlip(t1);
    const t2 = r.c.addReplyThread(b1);
    const r2 = r.c.appendBlip(t2);
    const r3 = r.c.appendBlip(t2);
    const t3 = r.c.addReplyThread(r1);
    const r4 = r.c.appendBlip(t3);
    r.take();
    // Deleting the last blip of a reply thread deletes the thread.
    r.c.deleteBlip(r4);
    expect(r.take()).toEqual([
      `-blip ${r4} in [${t3}] local`,
      "structure local",
      `-thread ${t3} under ${r1} local`,
      "structure local",
    ]);
    const t4 = r.c.addReplyThread(r1);
    const r5 = r.c.appendBlip(t4);
    r.take();
    r.c.deleteBlip(b1);
    const events = r.take().filter((line) => !line.startsWith("structure"));
    expect(events).toEqual([
      // The inline thread with its nested reply.
      `-blip ${r5} in [${t4}] local`,
      `-thread ${t4} under ${r1} local`,
      `-blip ${r1} in [${t1}] local`,
      `-thread ${t1} under ${b1} local`,
      // Its anchor goes after the thread: no anchor event.
      `content ${b1} __5; x< reply { id="${t1}" }; x>; __9;  by alice local`,
      `-blip ${r2} in [${t2}] local`,
      `-blip ${r3} in [${t2}] local`,
      `-thread ${t2} under ${b1} local`,
      `-blip ${b1} in [] local`,
    ]);
    expect(r.local.read()!.rootThread.blips.map((blip) => blip.id)).toEqual([b2]);
    r.c.deleteThread("");
    expect(r.take()).toEqual([`-blip ${b2} in [] local`, "structure local"]);
  });

  test("one remote operation deleting a subtree reports it bottom-up", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    const t1 = r.c.addReplyThread(b1);
    const r1 = r.c.appendBlip(t1);
    const r2 = r.c.appendBlip(t1);
    const t2 = r.c.addReplyThread(r1);
    const r3 = r.c.appendBlip(t2);
    const t3 = r.c.addReplyThread(b1);
    const b2 = r.c.appendBlip("");
    r.take();
    // Another client removes the manifest entry of b1 with all below it.
    const location = r.local.read()!.blip(b1)!.manifestLocation;
    r.view.wave
      .root()!
      .editDocument("conversation", (doc) =>
        doc.deleteNode(doc.view().elementStartingAt(location)!),
      );
    expect(r.take()).toEqual([
      `-blip ${r3} in [${t2}] local`,
      `-thread ${t2} under ${r1} local`,
      `-blip ${r1} in [${t1}] local`,
      `-blip ${r2} in [${t1}] local`,
      `-thread ${t1} under ${b1} local`,
      `-thread ${t3} under ${b1} local`,
      `-blip ${b1} in [] local`,
      "structure local",
    ]);
    expect(r.remote.read()!.rootThread.blips.map((blip) => blip.id)).toEqual([b2]);
  });

  test("one operation adding a subtree reports it top-down", () => {
    const r = new Replicas();
    const wavelet = r.view.wave.root()!;
    // The documents first, as blips only count once they exist.
    for (const id of ["b+1", "b+2", "b+3"]) {
      wavelet.editDocument(id, (doc) => void doc.appendXml(XML));
    }
    wavelet.editDocument("b+1", (doc) => void doc.createElement(2, "reply", [["id", "t+2"]]));
    expect(r.take()).toEqual([]);
    wavelet.applyDocOp(
      "conversation",
      new DocOpBuilder()
        .retain(1)
        .elementStart("blip", attrs({ id: "b+1" }))
        .elementStart("thread", attrs({ id: "t+1" }))
        .elementStart("blip", attrs({ id: "b+2" }))
        .elementEnd()
        .elementEnd()
        .elementStart("thread", attrs({ id: "t+2", inline: "true" }))
        .elementEnd()
        .elementEnd()
        .elementStart("blip", attrs({ id: "b+3" }))
        .elementEnd()
        .retain(1)
        .build(),
    );
    expect(r.take()).toEqual([
      "+blip b+1 in [] at 0 local",
      "+thread t+1 under b+1 local",
      "+blip b+2 in [t+1] at 0 local",
      "+thread t+2 under b+1 inline local",
      "+anchor t+2 in b+1 at 2 local",
      "+blip b+3 in [] at 1 local",
      "structure local",
    ]);
  });

  test("a manifest entry becomes a blip when its document appears", () => {
    const r = new Replicas();
    const wavelet = r.view.wave.root()!;
    wavelet.editDocument(
      "conversation",
      (doc) => void doc.createElement(1, "blip", [["id", "b+late"]]),
    );
    expect(r.take()).toEqual([]);
    expect(r.local.read()!.blip("b+late")).toBeNull();
    wavelet.editDocument("b+late", (doc) => void doc.appendXml(XML));
    // No content or metadata events for the operation that made it a blip.
    expect(r.take()).toEqual(["+blip b+late in [] at 0 local", "structure local"]);
    wavelet.editDocument("b+late", (doc) => doc.insertText(3, "x"));
    expect(r.take()).toEqual(['content b+late __3; ++"x"; __1;  by alice local']);
  });

  test("anchors appearing and disappearing later", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    const t1 = r.c.addReplyThread(b1);
    r.take();
    r.c.editBlip(b1, (doc) => void doc.createElement(3, "reply", [["id", t1]]));
    expect(r.take()).toEqual([
      `content ${b1} __3; << reply { id="${t1}" }; >>; __1;  by alice local`,
      `+anchor ${t1} in ${b1} at 3 local`,
    ]);
    // A second anchor of the same thread changes nothing; nor does text.
    r.c.editBlip(b1, (doc) => void doc.createElement(3, "reply", [["id", t1]]));
    r.c.editBlip(b1, (doc) => doc.insertText(3, "abc"));
    expect(r.take().filter((line) => !line.startsWith("content"))).toEqual([]);
    r.c.editBlip(b1, (doc) => doc.deleteNode(doc.view().elementStartingAt(6)!));
    expect(r.take().filter((line) => !line.startsWith("content"))).toEqual([]);
    r.c.editBlip(b1, (doc) => doc.deleteNode(doc.view().elementStartingAt(6)!));
    expect(r.take().filter((line) => !line.startsWith("content"))).toEqual([
      `-anchor ${t1} in ${b1} local`,
    ]);
    // An anchor of an unknown thread is not reported.
    r.c.editBlip(b1, (doc) => void doc.createElement(3, "reply", [["id", "t+unknown"]]));
    expect(r.take().filter((line) => !line.startsWith("content"))).toEqual([]);
  });

  test("reordering is a structure change without other events", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    const b2 = r.c.appendBlip("");
    r.take();
    r.view.wave.root()!.applyDocOp(
      "conversation",
      new DocOpBuilder()
        .retain(1)
        .deleteElementStart("blip", attrs({ id: b1 }))
        .deleteElementEnd()
        .retain(2)
        .elementStart("blip", attrs({ id: b1 }))
        .elementEnd()
        .retain(1)
        .build(),
    );
    // The root blip changed, and with it the title could have.
    expect(r.take()).toEqual(["structure local"]);
    expect(r.remote.read()!.rootThread.blips.map((blip) => blip.id)).toEqual([b2, b1]);
  });
});

describe("participants, metadata, title, tags, anchor", () => {
  test("participants and contributors", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    r.take();
    r.c.addParticipant(bob());
    r.c.addParticipant(bob());
    expect(r.take()).toEqual(["+participant bob by alice local"]);
    // Bob edits alice's blip.
    r.tick();
    const wavelet = r.view.wave.root()!;
    wavelet.setAuthor(bob());
    r.c.editBlip(b1, (doc) => doc.insertText(3, "hi"));
    expect(r.take()).toEqual([
      `content ${b1} __3; ++"hi"; __1;  by bob local`,
      `+contributor ${b1} bob`,
      `time ${b1} 1000->2000`,
    ]);
    r.c.removeParticipant(alice());
    expect(r.take()).toEqual(["-participant alice by bob local"]);
  });

  test("title", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    const b2 = r.c.appendBlip("");
    r.c.editBlip(b1, (doc) => doc.insertText(3, "First line. Rest"));
    r.c.editBlip(b2, (doc) => doc.insertText(3, "Second."));
    r.c.editBlip(b2, (doc) => title.maybeFindAndSetImplicitTitle(doc));
    expect(r.take().filter((line) => line.startsWith("title"))).toEqual([]);
    const titles = (): string[] => r.take().filter((line) => !line.startsWith("content"));
    r.c.updateImplicitTitle();
    expect(titles()).toEqual(['title ""->"First line."']);
    expect([r.local.title(), r.remote.title()]).toEqual(["First line.", "First line."]);
    r.c.updateImplicitTitle();
    expect(titles()).toEqual([]);
    // Editing inside the implicit title changes it.
    r.c.editBlip(b1, (doc) => doc.insertText(8, "!"));
    expect(titles()).toEqual(['title "First line."->"First! line."']);
    r.c.setExplicitTitle("Named");
    expect(titles()).toEqual(['title "First! line."->"Named"']);
    // The title is the root blip's: deleting it gives the next blip's.
    r.c.deleteBlip(b1);
    expect(titles()).toEqual([`-blip ${b1} in [] local`, 'title "Named"->"Second."', "structure local"]);
    r.c.deleteBlip(b2);
    expect(titles()).toEqual([`-blip ${b2} in [] local`, 'title "Second."->""', "structure local"]);
  });

  test("tags", () => {
    const r = new Replicas();
    r.c.addTag("one");
    expect(r.take()).toEqual(["tags +[one] -[] =[one]"]);
    r.c.addTag("two");
    r.c.addTag("one");
    expect(r.take()).toEqual(["tags +[two] -[] =[one,two]", "tags +[one] -[] =[one,two,one]"]);
    expect(r.c.removeTag("one")).toBe(true);
    expect(r.take()).toEqual(["tags +[] -[one] =[two,one]"]);
    expect(r.c.removeTag("missing")).toBe(false);
    expect(r.take()).toEqual([]);
    expect([...r.remote.tags()]).toEqual(["two", "one"]);
  });

  test("anchor", () => {
    const r = new Replicas();
    r.c.setAnchor({ conversationId: "example.com/conv+other", blipId: "b+1" });
    expect(r.take()).toEqual(["anchor null:null->example.com/conv+other:b+1"]);
    r.c.setAnchor({ conversationId: "example.com/conv+other", blipId: "b+1" });
    expect(r.take()).toEqual([]);
    r.c.setAnchor(null);
    expect(r.take()).toEqual(["anchor example.com/conv+other:b+1->null:null"]);
  });

  test("deleting the conversation", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    r.take();
    expect(r.local.hasConversation()).toBe(true);
    r.c.delete();
    expect(r.take()).toEqual([`-blip ${b1} in [] local`, "structure local", "structure local"]);
    expect([r.local.hasConversation(), r.remote.hasConversation()]).toEqual([false, false]);
    expect(r.local.read()).toBeNull();
  });
});

describe("lifecycle", () => {
  test("the initial state is not reported; dispose stops the events", () => {
    const r = new Replicas();
    const b1 = r.c.appendBlip("");
    const t1 = r.c.addInlineReplyThread(b1, 3);
    r.c.addTag("x");
    r.c.updateImplicitTitle();
    r.take();
    const late = new ObservableConversation(r.remoteWavelet);
    const events = log(late);
    expect([late.title(), [...late.tags()], late.conversationId]).toEqual([
      "",
      ["x"],
      "example.com/conv+root",
    ]);
    // It knows the anchor that is there.
    r.c.editBlip(b1, (doc) => doc.deleteNode(doc.view().elementStartingAt(3)!));
    expect(events.splice(0).filter((line) => !line.startsWith("content"))).toEqual([
      `-anchor ${t1} in ${b1} remote`,
    ]);
    late.dispose();
    expect(late.isDisposed()).toBe(true);
    r.c.appendBlip("");
    expect(events).toEqual([]);
    expect(r.take().slice(-2)).toEqual(["+blip b+sB in [] at 1 local", "structure local"]);
  });

  test("a failing listener stops nothing", () => {
    const errors: unknown[] = [];
    const wave = new WaveView(WAVE, alice());
    const view = new WaveBasedConversationView(
      wave,
      new IdGeneratorImpl("example.com", () => "s"),
      { onListenerError: (error) => errors.push(error) },
    );
    const c = view.createRoot();
    const observable = view.observeRoot()!;
    observable.events.on("blipAdded", () => {
      throw new Error("boom");
    });
    const events = log(observable);
    const b1 = c.appendBlip("");
    expect(events).toEqual([`+blip ${b1} in [] at 0 local`, "structure local"]);
    expect(errors.length).toBe(1);
    expect(view.root()!.blip(b1)).not.toBeNull();
  });
});

describe("conversation view events", () => {
  function setup(): { wave: WaveView; view: WaveBasedConversationView; events: string[] } {
    const wave = new WaveView(WAVE, alice(), { timestamp: 1 });
    const view = new WaveBasedConversationView(wave, new IdGeneratorImpl("example.com", () => "s"));
    const events: string[] = [];
    view.events.on("conversationAdded", (e) => events.push(`+${e.conversationId}`));
    view.events.on("conversationRemoved", (e) => events.push(`-${e.conversationId}`));
    return { wave, view, events };
  }

  test("conversations come and go", () => {
    const { wave, view, events } = setup();
    // A conversational wavelet without manifest is no conversation yet.
    const root = wave.createRoot();
    wave.createUserData();
    expect(events).toEqual([]);
    const observable = view.observeRoot()!;
    expect(observable.hasConversation()).toBe(false);
    const observed = log(observable);
    root.editDocument("conversation", manifest.initialiseDocumentManifest);
    expect(events.splice(0)).toEqual(["+example.com/conv+root"]);
    expect(observed.splice(0)).toEqual(["structure local"]);
    const other = view.createConversation();
    const otherId = other.read().id;
    expect(events.splice(0)).toEqual([`+${otherId}`]);
    other.delete();
    expect(events.splice(0)).toEqual([`-${otherId}`]);
    // The root is reset from a snapshot: the conversation is replaced.
    const b1 = view.rootMut().appendBlip("");
    expect(observed.splice(0).length).toBe(2);
    const replacement = wave.replaceWavelet(root.data().clone());
    expect(events.splice(0)).toEqual(["-example.com/conv+root", "+example.com/conv+root"]);
    expect(observable.isDisposed()).toBe(true);
    const again = view.observeRoot()!;
    expect(again).not.toBe(observable);
    expect(again).toBe(view.observe(ROOT));
    expect(again.wavelet).toBe(replacement);
    expect(again.read()!.blip(b1)).not.toBeNull();
    wave.removeWavelet(ROOT);
    expect(events.splice(0)).toEqual(["-example.com/conv+root"]);
    expect(again.isDisposed()).toBe(true);
    expect(view.observeRoot()).toBeNull();
  });

  test("a remote manifest makes a conversation", () => {
    const { wave, view, events } = setup();
    const name = WaveletName.of(WAVE, ROOT);
    const wavelet = wave.addWavelet(
      WaveletData.empty(name, bob(), HashedVersionFactory.createVersionZero(name), 0),
    );
    expect(events).toEqual([]);
    const context = (): WaveletOperationContext => new WaveletOperationContext(bob(), 5, 1);
    wavelet.consumeRemote(WaveletOperation.addParticipant(context(), bob()));
    wavelet.consumeRemote(
      WaveletOperation.blipContent(
        context(),
        "conversation",
        new DocOpBuilder().elementStart("conversation", attrs({})).elementEnd().build(),
      ),
    );
    expect(events.splice(0)).toEqual(["+example.com/conv+root"]);
    expect(view.root()!.participants.map((p) => p.address)).toEqual(["bob@example.com"]);
    wavelet.consumeRemote(
      WaveletOperation.blipContent(
        context(),
        "conversation",
        new DocOpBuilder()
          .deleteElementStart("conversation", attrs({}))
          .deleteElementEnd()
          .build(),
      ),
    );
    expect(events.splice(0)).toEqual(["-example.com/conv+root"]);
  });

  test("existing conversations are known but not announced; dispose", () => {
    const wave = new WaveView(WAVE, alice(), { timestamp: 1 });
    const ids = new IdGeneratorImpl("example.com", () => "s");
    new WaveBasedConversationView(wave, ids).createRoot();
    const view = new WaveBasedConversationView(wave, ids);
    const events: string[] = [];
    view.events.on("conversationAdded", (e) => events.push(`+${e.conversationId}`));
    view.events.on("conversationRemoved", (e) => events.push(`-${e.conversationId}`));
    const observable = view.observeRoot()!;
    view.rootMut().delete();
    expect(events.splice(0)).toEqual(["-example.com/conv+root"]);
    view.dispose();
    expect(observable.isDisposed()).toBe(true);
    wave.removeWavelet(ROOT);
    expect(events).toEqual([]);
    expect(() => view.observe(WaveletId.of("example.com", "user+alice@example.com"))).toThrow();
  });
});
