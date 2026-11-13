// Scenarios of the conversation model, compared with the records of the
// original Java implementation in `testdata/model_golden.txt` (port of the
// Rust `model_golden_tests/conversation.rs`; `wavelet-authorise` is in the
// wavelet module's tests).
//
// The digest scenarios of the same file need the `digest` module, which is
// not ported; a test below pins down which scenarios are covered where.

import { expect, test } from "vitest";

import type { WaveletId } from "../../id";
import {
  Wave,
  alice,
  bob,
  goldenScenarioNames,
} from "../../wavelet/tests/modelGoldenHarness";
import {
  type Conversation,
  type ConversationThread,
  WaveBasedConversationView,
  type WaveletBasedConversation,
  conversationIdFor,
  tags,
  title,
} from "..";

const ROOT = "conv+root";

/** The generator's wave with its conversation view. */
class ConvWave {
  readonly wave: Wave;
  readonly convs: WaveBasedConversationView;

  constructor() {
    this.wave = new Wave(alice());
    this.convs = new WaveBasedConversationView(this.wave.view, this.wave.ids);
  }

  step(label: string, step: () => void): void {
    this.wave.step(label, step);
  }

  stepUnordered(label: string, step: () => void): void {
    this.wave.stepUnordered(label, step);
  }

  q(answer: string): void {
    this.wave.q(answer);
  }

  root(): WaveletBasedConversation {
    return this.convs.rootMut();
  }

  queryConversation(wavelet: string): void {
    const conversation = this.convs.conversation(Wave.waveletId(wavelet))!;
    this.q(`structure ${dump(conversation, conversation.rootThread)}`);
    this.q(["bfs", ...conversation.blipsBreadthFirst().map((blip) => blip.id)].join(" "));
    this.q(`title ${conversation.title()}`);
  }

  queryBlip(wavelet: string, blipId: string): void {
    const blip = this.convs.conversation(Wave.waveletId(wavelet))!.blipData(blipId)!;
    const contributors = blip.contributors.map((p) => p.address).sort();
    this.q(
      `blip ${blip.id} author ${blip.author} contributors [${contributors.join(", ")}] ` +
        `lmv ${blip.lastModifiedVersion} lmt ${blip.lastModifiedTime}`,
    );
  }
}

function dump(conversation: Conversation, thread: ConversationThread): string {
  const blips = thread.blips.map((blip) => {
    const replies = conversation
      .locateReplyThreads(blip)
      .map((reply) => `${reply.location ?? -1}:${dump(conversation, reply.thread)}`);
    const root = conversation.isRootBlip(blip.id) ? "^" : "";
    return `${blip.id}${root}{${replies.join(",")}}`;
  });
  return `T(${thread.id})[${blips.join(",")}]`;
}

test("conv-basic", () => {
  const w = new ConvWave();
  w.step("create root", () => void w.convs.createRoot());
  w.step("query empty", () => {
    w.q(`id ${w.convs.root()!.id}`);
    w.q(`view ${w.convs.id()}`);
    w.queryConversation(ROOT);
  });
  const id: string[] = new Array<string>(10).fill("");
  const thread: string[] = new Array<string>(10).fill("");
  w.step("append root blip", () => {
    id[1] = w.root().appendBlip("");
  });
  w.step("write text", () =>
    w.root().editBlip(id[1]!, (doc) => doc.insertText(3, "Hello world. More text")),
  );
  w.step("implicit title", () => {
    w.root().updateImplicitTitle();
    w.queryConversation(ROOT);
  });
  w.step("implicit title again", () => w.root().updateImplicitTitle());
  w.step("append second blip", () => {
    id[2] = w.root().appendBlip("");
  });
  w.step("reply thread", () => {
    thread[1] = w.root().addReplyThread(id[1]!);
  });
  w.step("reply blip", () => {
    id[3] = w.root().appendBlip(thread[1]!);
  });
  w.step("inline reply thread", () => {
    thread[2] = w.root().addInlineReplyThread(id[1]!, 8);
  });
  w.step("inline reply blips", () => {
    id[4] = w.root().appendBlip(thread[2]!);
    id[5] = w.root().appendBlip(thread[2]!);
  });
  w.step("second inline reply thread, earlier", () => {
    thread[3] = w.root().addInlineReplyThread(id[1]!, 5);
    id[6] = w.root().appendBlip(thread[3]!);
  });
  w.step("nested reply", () => {
    thread[4] = w.root().addReplyThread(id[3]!);
    id[7] = w.root().appendBlip(thread[4]!);
  });
  w.step("insert blip", () => {
    id[8] = w.root().insertBlip("", id[2]!);
  });
  w.step("query", () => {
    w.queryConversation(ROOT);
    for (const blipId of id.slice(1, 9)) {
      w.queryBlip(ROOT, blipId);
    }
  });
  w.step("explicit title", () => {
    w.root().setExplicitTitle("Explicit");
    w.queryConversation(ROOT);
  });
  w.step("implicit title with explicit present", () => w.root().updateImplicitTitle());
  w.step("participants", () => {
    w.root().addParticipant(bob());
    w.root().addParticipant(bob());
    w.root().removeParticipant(alice());
    w.root().removeParticipant(alice());
  });
  w.step("author is no participant", () =>
    w.root().editBlip(id[2]!, (doc) => doc.insertText(3, "x")),
  );
  w.step("delete first inline blip", () => w.root().deleteBlip(id[4]!));
  w.step("delete last inline blip", () => {
    w.root().deleteBlip(id[5]!);
    w.queryConversation(ROOT);
  });
  w.step("delete nested thread", () => w.root().deleteThread(thread[4]!));
  w.step("delete reply thread", () => w.root().deleteThread(thread[1]!));
  w.step("delete inserted blip", () => w.root().deleteBlip(id[8]!));
  w.step("delete blip with inline reply", () => {
    w.root().deleteBlip(id[1]!);
    w.queryConversation(ROOT);
  });
  w.step("tags", () => {
    const c = w.root();
    c.addTag("one");
    c.addTag("two & <three>");
    c.addTag("four");
    c.removeTag("two & <three>");
    c.removeTag("missing");
    c.editDataDocument("tags", (doc) => tags.insertTag(doc, "zero", 0));
  });
  w.step("delete conversation", () => w.root().delete());
  w.wave.check("conv-basic");
});

test("conv-delete-many", () => {
  const w = new ConvWave();
  const id: string[] = new Array<string>(10).fill("");
  const thread: string[] = new Array<string>(10).fill("");
  w.step("build", () => {
    const c = w.convs.createRoot();
    id[1] = c.appendBlip("");
    c.editBlip(id[1], (doc) => doc.insertText(3, "Some text here"));
    thread[1] = c.addInlineReplyThread(id[1], 5);
    id[2] = c.appendBlip(thread[1]);
    id[3] = c.appendBlip(thread[1]);
    thread[2] = c.addReplyThread(id[1]);
    id[4] = c.appendBlip(thread[2]);
    thread[3] = c.addInlineReplyThread(id[1], 9);
    id[5] = c.appendBlip(thread[3]);
    thread[4] = c.addInlineReplyThread(id[3], 4);
    id[6] = c.appendBlip(thread[4]);
    id[7] = c.appendBlip("");
    w.queryConversation(ROOT);
  });
  w.stepUnordered("delete blip with many replies", () => {
    w.root().deleteBlip(id[1]!);
    w.queryConversation(ROOT);
  });
  w.stepUnordered("delete root thread", () => {
    w.root().deleteThread("");
    w.queryConversation(ROOT);
  });
  w.wave.check("conv-delete-many");
});

test("conv-anchor", () => {
  const w = new ConvWave();
  const id: string[] = new Array<string>(10).fill("");
  const rootId = Wave.waveletId(ROOT);
  let other!: WaveletId;
  w.step("create root", () => {
    id[1] = w.convs.createRoot().appendBlip("");
  });
  const hasAnchor = (): void => w.q(`has anchor ${w.convs.anchorOf(other) !== null}`);
  w.step("create conversation", () => {
    const c = w.convs.createConversation();
    id[2] = c.appendBlip("");
    other = c.wavelet.waveletId();
    w.q(`id ${c.read().id}`);
    hasAnchor();
  });
  w.step("set anchor", () => {
    w.convs.setAnchor(other, { waveletId: rootId, blipId: id[1]! });
    hasAnchor();
    const anchor = w.convs.anchorOf(other)!;
    w.q(`anchor ${conversationIdFor(anchor.waveletId)} ${anchor.blipId}`);
  });
  w.step("set same anchor", () => w.convs.setAnchor(other, { waveletId: rootId, blipId: id[1]! }));
  w.step("delete anchor blip", () => {
    w.root().deleteBlip(id[1]!);
    hasAnchor();
  });
  w.step("clear anchor", () => {
    w.convs.setAnchor(other, null);
    hasAnchor();
  });
  w.step("append with content", () => {
    w.convs.conversationMut(other).appendBlipWithContent("", title.emptyDocumentWithTitle());
  });
  w.wave.check("conv-anchor");
});

test("the scenarios of the golden file are all accounted for", () => {
  const here = ["conv-basic", "conv-delete-many", "conv-anchor"];
  const wavelet = ["wavelet-authorise"];
  const names = goldenScenarioNames();
  const rest = names.filter((name) => !here.includes(name) && !wavelet.includes(name));
  for (const name of [...here, ...wavelet]) {
    expect(names, name).toContain(name);
  }
  // Document scenarios are tested in `model/doc`; digest scenarios need the
  // unported `digest` module.
  for (const name of rest) {
    expect(name.startsWith("doc-") || name.startsWith("digest"), name).toBe(true);
  }
});
