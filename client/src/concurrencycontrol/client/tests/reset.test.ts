// Not in the original: a rewind of the server's history (a change of host
// epoch) taken back by the client's concurrency control, which keeps the
// operations it has not had acknowledged.

import { describe, expect, it } from "vitest";

import { DocOpBuilder } from "../../../model/docop";
import { ParticipantId } from "../../../model/id";
import {
  TransformedWaveletDelta,
  type WaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
  invertDeltas,
} from "../../../model/operation";
import type { HashedVersion } from "../../../model/version";
import { ConcurrencyControl } from "..";
import type { ServerConnection } from "../serverConnection";
import { ClientMock } from "./clientMock";
import { DeltaTestUtil, TestDoc, genSignature } from "./helpers";

const ALICE = new DeltaTestUtil("alice@example.com");

/** Alice inserts `text` at `pos` of a document of `pos + remaining` items. */
function insert(
  pos: number,
  text: string,
  remaining: number,
  version: HashedVersion | null,
): WaveletOperation {
  const builder = new DocOpBuilder();
  if (pos > 0) {
    builder.retain(pos);
  }
  builder.characters(text);
  if (remaining > 0) {
    builder.retain(remaining);
  }
  return WaveletOperation.blipContent(
    new WaveletOperationContext(ALICE.getAuthor(), 0, 1, version),
    "blip id",
    builder.build(),
  );
}

class Connection implements ServerConnection {
  readonly sent: WaveletDelta[] = [];
  isOpen(): boolean {
    return true;
  }
  send(delta: WaveletDelta): void {
    this.sent.push(delta);
  }
  debugGetProfilingInfo(): string {
    return "";
  }
}

function setup(xml: string): {
  cc: ConcurrencyControl;
  client: ClientMock;
  connection: Connection;
} {
  const cc = new ConcurrencyControl(null, genSignature(0));
  const connection = new Connection();
  const client = new ClientMock(
    cc,
    TestDoc.parse(xml),
    ParticipantId.newUnchecked("bob@example.com"),
    null,
  );
  cc.initialise(connection, client);
  cc.onOpen(genSignature(0), genSignature(0));
  return { cc, client, connection };
}

function serverDelta(
  at: number,
  op: WaveletOperation,
): TransformedWaveletDelta {
  return TransformedWaveletDelta.cloneOperations(
    ALICE.getAuthor(),
    genSignature(at + 1),
    0,
    [op],
  );
}

function text(client: ClientMock): string {
  client.receiveServerOperations();
  return client.requireDoc().asOperation().toString();
}

function expectText(client: ClientMock, xml: string): void {
  expect(text(client)).toBe(TestDoc.parse(xml).asOperation().toString());
}

describe("ConcurrencyControl.onReset", () => {
  it("takes back rewound deltas, its own included, and keeps pending ops", () => {
    const { cc, client, connection } = setup("ab");
    // Alice's delta 0 -> 1, then bob's acknowledged delta 1 -> 2.
    cc.onServerDelta(serverDelta(0, insert(0, "X", 2, genSignature(1))));
    expectText(client, "Xab");
    client.doInsert(3, "Y");
    client.flush();
    expect(connection.sent.length).toBe(1);
    const own = connection.sent[0]!;
    cc.onSuccess(1, genSignature(2));
    expectText(client, "XabY");
    // Bob types on: one delta in flight, more queued.
    client.doInsert(0, "1");
    client.flush();
    client.doInsert(1, "2");
    client.flush();
    expect(connection.sent.length).toBe(2);
    expectText(client, "12XabY");

    // The history goes back to version 0; both deltas are gone.
    const rewound = [
      serverDelta(0, insert(0, "X", 2, genSignature(1))),
      TransformedWaveletDelta.cloneOperations(
        own.author,
        genSignature(2),
        0,
        own.ops,
      ),
    ];
    cc.onReset(genSignature(0), rewound);
    expectText(client, "12ab");
    expect(cc.getReconnectionVersions()).toEqual([genSignature(0)]);
    expect(cc.getUnacknowledgedDelta()).toBeNull();

    // Reconnected at version 0: the pending ops go again, from version 0.
    cc.onOpen(genSignature(0), genSignature(0));
    expect(connection.sent.length).toBe(3);
    const resent = connection.sent[2]!;
    expect(resent.targetVersion).toEqual(genSignature(0));
    cc.onSuccess(resent.ops.length, genSignature(1));
    expect(connection.sent.length).toBe(4);
    expect(connection.sent[3]!.targetVersion).toEqual(genSignature(1));
    const doc = TestDoc.parse("ab");
    for (const op of [...resent.ops, ...connection.sent[3]!.ops]) {
      doc.consume(op.contentOp!);
    }
    expect(doc.asOperation().toString()).toBe(
      TestDoc.parse("12ab").asOperation().toString(),
    );
  });

  it("applies the replayed history on top", () => {
    const { cc, client } = setup("ab");
    cc.onServerDelta(serverDelta(0, insert(2, "Z", 0, genSignature(1))));
    expectText(client, "abZ");
    cc.onReset(genSignature(0), [
      serverDelta(0, insert(2, "Z", 0, genSignature(1))),
    ]);
    expectText(client, "ab");
    cc.onOpen(genSignature(0), genSignature(0));
    cc.onServerDelta(serverDelta(0, insert(0, "W", 2, genSignature(1))));
    cc.onServerDelta(serverDelta(1, insert(3, "Z", 0, genSignature(2))));
    expectText(client, "WabZ");
  });

  it("inverts deltas last first, ending at the fork", () => {
    const ops = [
      insert(0, "X", 2, null),
      ALICE.addParticipant(ParticipantId.newUnchecked("c@example.com")),
    ];
    const inverse = invertDeltas([{ ops }], genSignature(4));
    expect(inverse.map((op) => op.kind.type)).toEqual([
      "removeParticipant",
      "blip",
    ]);
    expect(inverse.every((op) => op.context.versionIncrement === -1)).toBe(
      true,
    );
    expect(inverse[1]!.context.hashedVersion).toEqual(genSignature(4));
    const doc = TestDoc.parse("Xab");
    doc.consume(inverse[1]!.contentOp!);
    expect(doc.asOperation().toString()).toBe(
      TestDoc.parse("ab").asOperation().toString(),
    );
  });
});
