// Port of `org.waveprotocol.wave.concurrencycontrol.client.OT3Test` of
// Apache Wave (Apache License, Version 2.0).
//
// A thorough test of client CC by emitting fake events and checking the
// outputs from CC. The operations used in the tests are secondary, we are
// more focused on the CC producing the right version numbers etc.

import { describe, expect, it } from "vitest";

import { DocOpBuilder } from "../../../model/docop";
import { ParticipantId } from "../../../model/id";
import {
  TransformedWaveletDelta,
  WaveletOperation,
  WaveletOperationContext,
} from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import {
  ChannelException,
  Recoverable,
  isOperationException,
  isTransformException,
} from "../../common";
import { ConcurrencyControl } from "..";
import { ClientMock } from "./clientMock";
import { DeltaTestUtil, TestDoc, genSignature, thrownBy } from "./helpers";
import { ServerConnectionMock } from "./serverConnectionMock";

const DEFAULT_CREATOR = ParticipantId.newUnchecked("test@example.com");
const CLIENT_UTIL = new DeltaTestUtil(DEFAULT_CREATOR);
const EXTRA_UTIL = new DeltaTestUtil("actasme@example.com");
/**
 * Different user id to DEFAULT_CREATOR so that the comparison test in CC for
 * reconnection works.
 */
const SERVER_UTIL = new DeltaTestUtil("someoneelse@example.com");

/**
 * Test config to pretend the editor did something or we got something on the
 * wire from the server.
 */
class TestConfig {
  clientMock!: ClientMock;
  serverConnectionMock = new ServerConnectionMock();

  init(version: number, intialBlipXml: string | null = null): void {
    const signature = genSignature(version);
    this.serverConnectionMock = new ServerConnectionMock();
    const clientCC = new ConcurrencyControl(null, signature);
    this.serverConnectionMock.setListener(clientCC);
    this.clientMock = new ClientMock(
      clientCC,
      intialBlipXml !== null ? TestDoc.parse(intialBlipXml) : null,
      DEFAULT_CREATOR,
      this.serverConnectionMock,
    );

    clientCC.initialise(this.serverConnectionMock, this.clientMock);
    clientCC.onOpen(signature, signature);
  }

  /**
   * Pretend client did some insert. We don't need a version number as it's
   * inferred in client CC. Flushes at the end unless told otherwise.
   */
  clientDoInsert(insertionPoint: number, content: string, flush = true): this {
    this.clientMock.doInsert(insertionPoint, content);
    if (flush) {
      this.clientMock.flush();
    }
    return this;
  }

  /** Pretend client did the given ops (`clientDoOps(WaveletOperation...)`). */
  clientDoGivenOps(...ops: WaveletOperation[]): this {
    for (const op of ops) {
      this.clientMock.addClientOperation(op);
    }
    this.clientMock.flush();
    return this;
  }

  /**
   * Pretend client did some ops.
   *
   * Assumption: no-ops are not merged in MergingSequence. If they were, the
   * tests would break as they count the number of ops sent from the client
   * to the server.
   */
  clientDoOps(numOps: number): this {
    for (let i = 0; i < numOps; i++) {
      this.clientMock.addClientOperation(
        WaveletOperation.noOp(
          new WaveletOperationContext(this.clientMock.getParticipantId(), 0, 1),
        ),
      );
    }
    this.clientMock.flush();
    return this;
  }

  private noOpDocOp(blipId: string): WaveletOperation {
    const context = new WaveletOperationContext(this.clientMock.getParticipantId(), 0, 1);
    return WaveletOperation.blipContent(context, blipId, new DocOpBuilder().build());
  }

  /** Pretend client did some doc ops. */
  clientDoDocOps(...blipIds: string[]): this {
    for (const b of blipIds) {
      this.clientMock.addClientOperation(this.noOpDocOp(b));
    }
    this.clientMock.flush();
    return this;
  }

  /** Pretend the server did some insert at the given version. */
  serverDoInsert(
    startVersion: number,
    insertionPoint: number,
    content: string,
    remaining: number,
  ): this {
    const d = TransformedWaveletDelta.cloneOperations(
      SERVER_UTIL.getAuthor(),
      genSignature(startVersion + 1),
      0,
      [SERVER_UTIL.insert(insertionPoint, content, remaining, null)],
    );
    this.serverConnectionMock.triggerServerDeltas([d]);
    return this;
  }

  /**
   * Pretend server did some ops at the given op version using different
   * deltas.
   *
   * We generate a predictable signature here for testing later. Each
   * signature is the version on the server after the ops.
   */
  serverDoOps(startVersion: number, numOps = 1): this {
    const deltas: TransformedWaveletDelta[] = [];
    for (let i = 0; i < numOps; i++) {
      deltas.push(
        TransformedWaveletDelta.cloneOperations(
          SERVER_UTIL.getAuthor(),
          genSignature(startVersion + i + 1),
          0,
          [SERVER_UTIL.noOp()],
        ),
      );
    }
    this.serverConnectionMock.triggerServerDeltas(deltas);
    return this;
  }

  /** Pretend the server echos back the client's doc operation. */
  serverDoEchoBackDocOp(startVersion: number, blipId: string): this {
    const d = TransformedWaveletDelta.cloneOperations(
      this.clientMock.getParticipantId(),
      genSignature(startVersion + 1),
      0,
      [this.noOpDocOp(blipId)],
    );
    this.serverConnectionMock.triggerServerDeltas([d]);
    return this;
  }

  /** Pretend the server echos back the client's operation. */
  serverDoEchoBack(startVersion: number, timestamp = 0): this {
    const d = TransformedWaveletDelta.cloneOperations(
      this.clientMock.getParticipantId(),
      genSignature(startVersion + 1),
      timestamp,
      [SERVER_UTIL.noOp()],
    );
    this.serverConnectionMock.triggerServerDeltas([d]);
    return this;
  }

  /**
   * Pretend the server acked the given version.
   *
   * We generate a predictable signature here for testing later, which is the
   * version on the server after the op.
   *
   * @param version the new version after the operations from the client are
   *        applied.
   * @param numApplied number of operations applied on the server.
   */
  serverAck(version: number, numApplied = 1): this {
    this.serverConnectionMock.triggerServerSuccess(numApplied, genSignature(version));
    return this;
  }

  /** Pretend the server sent a commit notification for the given version. */
  serverCommit(version: number): this {
    this.serverConnectionMock.triggerServerCommit(version);
    return this;
  }

  /** Pretend we are reconnecting to the server. */
  reconnectToServer(): this {
    this.serverConnectionMock.setOpen(true);
    this.serverConnectionMock.reconnect(this.clientMock.getReconnectionVersions());
    return this;
  }

  /** Pretend we are disconnected from the server. */
  disconnectFromServer(): this {
    this.serverConnectionMock.setOpen(false);
    return this;
  }

  /** Server opens the connection at the given version and tells of the last version. */
  serverDoOpen(startVersion: number, endVersion: number): this {
    this.serverConnectionMock.triggerOnOpen(genSignature(startVersion), genSignature(endVersion));
    return this;
  }

  private clientReceiveOpFromCC(expectOps: boolean): void {
    if (expectOps) {
      expect(this.clientMock.getNumOpsReceived()).toBeGreaterThan(0);
    }
    this.clientMock.clearEvents();
    this.clientMock.receiveServerOperations();
  }

  /**
   * Without arguments: check client got nothing from the server. Otherwise:
   * check client got the given number of operations from the server (the
   * start version, the version before the first op, is not checked, as in
   * the original).
   */
  checkClientGotOps(startVersion?: number, numOps = 1): this {
    if (startVersion === undefined) {
      this.clientReceiveOpFromCC(false);
      expect(this.clientMock.getServerOperations().length).toBe(0);
      return this;
    }
    this.clientReceiveOpFromCC(numOps !== 0);
    const clientGot = this.clientMock.getServerOperations();
    expect(clientGot.length).toBe(numOps);
    for (const op of clientGot) {
      expect(op.context.versionIncrement).toBe(1);
    }
    // Clear the client received ops in clientMock.
    clientGot.length = 0;
    return this;
  }

  /**
   * Without arguments: check client didn't send anything. Otherwise: check
   * the server got one delta from the client with the given target version
   * and the given number of ops.
   */
  checkClientSentOps(startVersion?: number, numOps = 1): this {
    const serverGot = this.serverConnectionMock.getSentDeltas();
    if (startVersion === undefined) {
      expect(serverGot.length).toBe(0);
      return this;
    }
    // Should have only sent 1 delta.
    expect(serverGot.length).toBe(1);
    const delta = serverGot[0] as (typeof serverGot)[number];
    // Delta has the version number.
    expect(delta.targetVersion.version).toBe(startVersion);
    expect(delta.length).toBe(numOps);
    // Clear the sent ops inside serverConnectionMock.
    serverGot.length = 0;
    return this;
  }

  /**
   * Check client sent signatures for the given versions. Each version number
   * is after the operation as applied.
   */
  checkClientSentOpen(...versions: number[]): this {
    const signatures = this.serverConnectionMock.getReconnectSignatures();
    expect(signatures.map(String)).toEqual(versions.map((v) => String(genSignature(v))));
    signatures.length = 0;
    return this;
  }

  /** Check client has a document that looks like the following. */
  checkClientDoc(xml: string): this {
    const expected = TestDoc.parse(xml).asOperation();
    const actual = this.clientMock.requireDoc().asOperation();
    expect(expected.equals(actual), `[Expected: ${expected}] [Actual: ${actual}]`).toBe(true);
    return this;
  }
}

describe("OT3Test", () => {
  /**
   * Test the various divergence to check we transform the operations correctly. It's not
   * an exhaustive test of transformation, but to test CC call transformation properly.
   *
   */
  it("testTransformOperations", () => {
    const t = new TestConfig();

    // Empty Case
    t.init(0);
    t.checkClientGotOps().checkClientSentOps();

    // Simple insert from client
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X");
    t.checkClientGotOps().checkClientSentOps(0).checkClientDoc("<blip><p>Xabc</p></blip>");

    // Simple insert from server
    t.init(0, "<blip><p>abc</p></blip>");
    t.serverDoInsert(0, 2, "X", 5);
    t.checkClientGotOps(0).checkClientSentOps().checkClientDoc("<blip><p>Xabc</p></blip>");

    // Simple conflict
    //     c1 /\ s1
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X").serverDoInsert(0, 3, "Y", 4);
    t.checkClientGotOps(0).checkClientSentOps(0).checkClientDoc("<blip><p>XaYbc</p></blip>");

    // Conflict
    //     c1 /\ s1
    //    c2 /
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X").clientDoInsert(3, "Y");
    t.serverDoInsert(0, 3, "A", 4);
    t.checkClientGotOps(0).checkClientSentOps(0).checkClientDoc("<blip><p>XYaAbc</p></blip>");

    // Conflict
    // c1, c2 /\ s1
    //    c3 /
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X", false).clientDoInsert(4, "Y").clientDoInsert(6, "Z");
    t.serverDoInsert(0, 3, "A", 5);
    t.checkClientGotOps(0).checkClientSentOps(0, 1).checkClientDoc("<blip><p>XaYAbZc</p></blip>");

    // Conflict
    //     c1 /\ s1
    //          \ s2
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X").serverDoInsert(0, 3, "Y", 4).serverDoInsert(1, 4, "Z", 4);
    t.checkClientGotOps(0, 2).checkClientSentOps(0).checkClientDoc("<blip><p>XaYZbc</p></blip>");

    // Conflict
    //    c1 /\ s1
    //   c2 /  \ s2
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X").clientDoInsert(3, "Y");
    t.serverDoInsert(0, 3, "Z", 4).serverDoInsert(1, 4, "A", 4);
    t.checkClientGotOps(0, 2).checkClientSentOps(0).checkClientDoc("<blip><p>XYaZAbc</p></blip>");


    // Empty Server Delta
    t.init(0);
    // The original passes a null author.
    t.serverConnectionMock.triggerServerDeltas([
      new TransformedWaveletDelta(SERVER_UTIL.getAuthor(), HashedVersion.unsigned(0), 0, []),
    ]);
    t.checkClientGotOps().checkClientSentOps();
    t.clientDoOps(1);
    t.checkClientGotOps().checkClientSentOps(0);
  });

  /**
   * Test the CC caches the operations from the server correct when the client is not
   * ready receive them yet.
   *
   */
  it("testServerOperationCache", () => {
    const t = new TestConfig();

    //                     c1 /\ s1
    //  c2 (not given to cc) /
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X").clientDoInsert(3, "Y", false).checkClientSentOps(0);
    t.checkClientDoc("<blip><p>XYabc</p></blip>");
    t.serverDoInsert(0, 3, "A", 4).serverDoInsert(1, 4, "B", 4);
    t.clientMock.flush();
    t.checkClientGotOps(0, 2).checkClientSentOps().checkClientDoc("<blip><p>XYaABbc</p></blip>");


    // Check that if we don't flush the operations cached in the client, bad things happen
    //                     c1 /\ s1
    //  c2 (not given to cc) /
    t.init(0, "<blip><p>abc</p></blip>");
    t.clientDoInsert(2, "X").clientDoInsert(3, "Y", false).checkClientSentOps(0);
    t.checkClientDoc("<blip><p>XYabc</p></blip>");
    t.serverDoInsert(0, 3, "A", 4);
    // Expect an exception because the client ops didn't get transformed
    // so the expected length of the document is incorrect when applying
    // server ops.
    const expected = thrownBy(() =>
      t.checkClientGotOps(0, 2).checkClientSentOps().checkClientDoc("<blip><p>XYABabc</p></blip>"),
    );
    expect(expected).toBeInstanceOf(Error);
    expect((expected as Error).message).toBe("OperationException");
    expect(isOperationException((expected as Error).cause)).toBe(true);
  });

  /**
   * Tests bundling of operations into deltas and queuing of operations while
   * others are in flight.
   */
  it("testClientOperationQueuing", () => {
    const t = new TestConfig();

    // Tests queuing of a delta until one which is in flight has been acked, at
    // which point the waiting delta should be sent.
    t.init(1);
    t.clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
    t.serverAck(2).checkClientSentOps(2).checkClientGotOps(1);
    t.serverAck(3).checkClientSentOps().checkClientGotOps(2);

    // Tests bundling of operations by the one author into a single delta.
    t.init(1);
    t.clientDoGivenOps(CLIENT_UTIL.noOp(), CLIENT_UTIL.noOp());
    t.checkClientSentOps(1, 2).checkClientGotOps();
    t.serverAck(3, 2).checkClientSentOps().checkClientGotOps(1, 2);

    // Tests that two operations from differing creators are sent as separate
    // deltas. In the process, checks again that only one delta is in flight at
    // a time and that the acking of the first causes the second to be sent.
    t.init(1);
    t.clientDoGivenOps(CLIENT_UTIL.noOp(), EXTRA_UTIL.noOp());
    t.checkClientSentOps(1, 1).checkClientGotOps();
    t.serverAck(2, 1).checkClientSentOps(2, 1).checkClientGotOps(1, 1);
    t.serverAck(3, 1).checkClientSentOps().checkClientGotOps(2, 1);
  });

  /**
   * Test various ways the operations are interleaved. We don't test they are transformed
   * as they are tested elsewhere.
   */
  it("testConcurrencySimulation", () => {
    const t = new TestConfig();

    // Simple insert from client
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0);
    t.serverAck(1).checkClientSentOps();

    // Simple insert from server
    t.init(0);
    t.serverDoOps(0).checkClientGotOps(0).checkClientSentOps();

    // No Conflict. Left is client action. Right is server action.
    //             c1 / ack c1
    //            c2 /\ s1
    //  c3 (cached) / / ack c2, causes c3' to be sent
    //          s1' \/ ack c3
    //               \ s2
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0); // Expect a version update op here.
    t.serverDoOps(1).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).clientDoOps(1).checkClientSentOps(2).checkClientGotOps();
    t.serverAck(3).checkClientSentOps(3).checkClientGotOps(2);
    t.serverAck(4).checkClientSentOps().checkClientGotOps(3);
    t.serverDoOps(4).checkClientSentOps().checkClientGotOps(4);

    // Conflict. Left is client action. Right is server action.
    //             c1 / ack c1
    //                \ s1
    //             c2 /\ s2
    //   c3 (cached) / / ack c2, causes c3', c4' to be sent
    //  c4 (cached) /
    //        s2''' \
    //           c5 /
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.serverDoOps(1).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).clientDoOps(1).clientDoOps(1).serverDoOps(2)
        .checkClientSentOps(2).checkClientGotOps(2);
    t.serverAck(4).checkClientSentOps(4, 2).checkClientGotOps(3);
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
  });

  /**
   * Test errors in the protocol.
   */
  it("testErrorConditions", () => {
    const t = new TestConfig();

    // Missing ack
    //             c1 /\ s1
    //            s1' \
    //                 \ s2
    t.init(0);
    t.clientDoOps(1).serverDoOps(0).checkClientSentOps(0).checkClientGotOps(0);
    expect(isTransformException(thrownBy(() => t.serverDoOps(2)))).toBe(true);

    // Wrong ack
    //             c1 /
    //
    //                | ack c2
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    expect(isTransformException(thrownBy(() => t.serverAck(2)))).toBe(true);
  });

  /**
   * test reconnecting to the server.
   */
  it("testRecovery", () => {
    const t = new TestConfig();
    // Simple case, but also test breaking connection.
    //              c1 / ack c1 <-- recover from here
    //         c2, c3 / ack c2, c3
    // c4 (in flight)/ <-- connection broken
    //  c5 (cached) /
    // c6 (cached) /
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(2).checkClientSentOps(1, 2).checkClientGotOps();
    t.serverAck(3, 2).checkClientSentOps().checkClientGotOps(1, 2);
    t.clientDoOps(1).checkClientSentOps(3).checkClientGotOps();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
    // Break connection
    t.disconnectFromServer();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1, 3);
    // Resend delta containing c2, c3
    t.serverDoOpen(1, 3).checkClientSentOps(1, 2);


    // Simple case
    //               c1 / ack c1
    //              c2 / ack c2 <-- recover from here
    // c4 (in flight) / <-- connection broken
    //  c5 (cached)  /
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    t.serverAck(2).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).checkClientSentOps(2).checkClientGotOps();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1, 2);
    // Resend delta containing c4
    t.serverDoOpen(2, 2).checkClientSentOps(2, 1);


    // Simple case
    //              c1 / ack c1 <-- recover from here
    //             c2 / ack c2
    // c3 (ack lost) / ack c3 <-- connection broken
    //  c4 (cached) /
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    t.serverAck(2).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).checkClientSentOps(2).checkClientGotOps();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1, 2);
    t.serverDoOpen(2, 3).checkClientSentOps();
    // Resend delta containing c2
    t.serverDoEchoBack(2).checkClientSentOps(3);

    // Server ops, no transformation needed at recovery
    //              c1 / ack c1
    // U               \ s1
    //              c2 / ack c2 <-- recover from here (test we chop the inferred server path)
    // c3 (in flight) / <-- connection broken
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.serverDoOps(1).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).checkClientSentOps(2).checkClientGotOps();
    t.serverAck(3).checkClientSentOps().checkClientGotOps(2);
    t.clientDoOps(1).checkClientSentOps(3).checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(2, 3);
    // Resend delta containing c3
    t.serverDoOpen(2, 2).checkClientSentOps(2, 1);

    // Server ops, transformation needed at recovery and no operation comparison need.
    //              c1 / ack c1 <-- recover from here  <-- point of resend
    //    c2 (cached) /\ s2 <-- connection broken
    //                  \ s3
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    // Break connection
    t.disconnectFromServer();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1);
    t.serverDoOpen(1, 4).checkClientSentOps(1);
    // Now send back ops on server, need to test comparison here.
    t.serverDoOps(1).checkClientSentOps().checkClientGotOps(1);
    t.serverDoOps(2).checkClientSentOps().checkClientGotOps(2);


    // Server ops, transformation needed at recovery and operation comparison need.
    //              c1 / ack c1 <-- recover from here
    //  c2 (ack lost) /\ s2 <-- connection broken
    //  c3 (cached)  / / ack c2 <-- point of resend
    //                 \ s3
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(1).clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1);
    t.serverDoOpen(1, 4).checkClientSentOps();
    // Now send back ops on server, need to test comparison here.
    t.serverDoOps(1).checkClientSentOps().checkClientGotOps(1);
    t.serverDoEchoBack(2).checkClientSentOps(3).checkClientGotOps(2);
    t.serverDoOps(3).checkClientSentOps().checkClientGotOps(3);


    // Server ops, transformation needed at recovery and operation comparison need.
    //              c1 / ack c1 <-- recover from here
    // c2 (in flight) /\ s2 <-- connection broken
    //  c3 (cached)  /  \ s3 <-- point of resend
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(1).clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1);
    t.serverDoOpen(1, 3).checkClientSentOps();
    // Now send back ops on server, need to test comparison here.
    t.serverDoOps(1).checkClientSentOps().checkClientGotOps(1);
    t.serverDoOps(2).checkClientSentOps(3, 1).checkClientGotOps(2);

    // Not recoverable, no signatures recognised.
    //                   <-- server restarted before inferred server path
    //              c1 / ack c1
    //  c2 (ack lost) / <-- connection broken
    t.init(1);  // start at version 1
    t.clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    t.serverAck(2).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).checkClientSentOps(2).checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(1, 2);
    // Server doesn't recognize any signature, so it sends the latest signature it knows
    const failure = thrownBy(() => t.serverDoOpen(0, 0));
    expect(failure).toBeInstanceOf(ChannelException);
    expect((failure as ChannelException).recoverable).toBe(Recoverable.NOT_RECOVERABLE);
  });

  /**
   * Mainly to test that we are doing comparison for doc op.
   */
  it("testEchoBackDocumentEquality", () => {
    const t = new TestConfig();
    // Simple case
    //              c1 / ack c1
    //             c2 / ack c2
    // c3 (ack lost) / ack c3 <-- connection broken, <-- recover from here
    //  c4 (cached) /
    t.init(0);
    // Use different blip ids so that we don't merge ops
    t.clientDoDocOps("blip1").checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoDocOps("blip2").checkClientSentOps(1).checkClientGotOps();
    t.serverAck(2).checkClientSentOps().checkClientGotOps(1);
    t.clientDoDocOps("blip3").checkClientSentOps(2).checkClientGotOps();
    t.clientDoDocOps("blip4").checkClientSentOps().checkClientGotOps();
    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1, 2);
    t.serverDoOpen(2, 3).checkClientSentOps();
    // Client interprets the echo-back as an ack, which we test by
    // seeing that it goes on to send c4
    t.serverDoEchoBackDocOp(2, "blip3").checkClientSentOps(3);
  });

  /**
   * Test being disconnected several times.
   */
  it("testRecoveryMultipleTimes", () => {
      const t = new TestConfig();
      // Simple case, but also test breaking connection.
      //              c1 / ack c1 <-- (2) recover from here, (4) recover from here again
      //         c2, c3 / ack c2, c3
      // c4 (in flight)/ <-- (1) connection broken, (3) rebroken again
      //  c5 (cached) /
      // c6 (cached) /
      t.init(0);
      t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
      t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
      t.clientDoOps(2).checkClientSentOps(1, 2).checkClientGotOps();
      t.serverAck(3, 2).checkClientSentOps().checkClientGotOps(1, 2);
      t.clientDoOps(1).checkClientSentOps(3).checkClientGotOps();
      t.clientDoOps(1).checkClientSentOps().checkClientGotOps();

      // Break connection
      t.disconnectFromServer();
      t.clientDoOps(1).checkClientSentOps().checkClientGotOps();

      // Reconnect
      t.reconnectToServer().checkClientSentOpen(0, 1, 3);
      // Resend delta containing c1
      t.serverDoOpen(1, 1).checkClientSentOps(1, 2);

      // Break connection again
      t.disconnectFromServer();

      // Reconnect again
      t.reconnectToServer().checkClientSentOpen(0, 1);
      // Resend delta containing c1
      t.serverDoOpen(1, 1).checkClientSentOps(1, 2);
  });

  it("testRecoveryWithCommit", () => {
    const t = new TestConfig();

    // Reconnect after server commit
    //              c1 / ack c1 <-- server commit, reconnect here.
    //             c2 / ack c2

    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    t.serverAck(2).checkClientSentOps().checkClientGotOps(1);
    t.serverCommit(1);

    // Reconnect
    t.reconnectToServer().checkClientSentOpen(1, 2);
  });

  /**
   * Test that the client ignores the timestamp in the echo back op from the server.
   */
  it("testEchobackWithDifferentTimeStamp", () => {
    const t = new TestConfig();

    // Simple case
    //              c1 / ack c1 <-- recover from here
    //             c2 / ack c2
    // c3 (ack lost) / ack c3 <-- connection broken
    //  c4 (cached) /
    t.init(0);
    t.clientDoOps(1).checkClientSentOps(0).checkClientGotOps();
    t.serverAck(1).checkClientSentOps().checkClientGotOps(0);
    t.clientDoOps(1).checkClientSentOps(1).checkClientGotOps();
    t.serverAck(2).checkClientSentOps().checkClientGotOps(1);
    t.clientDoOps(1).checkClientSentOps(2).checkClientGotOps();
    t.clientDoOps(1).checkClientSentOps().checkClientGotOps();

    // Reconnect
    t.reconnectToServer().checkClientSentOpen(0, 1, 2);
    t.serverDoOpen(2, 3).checkClientSentOps();
    // Using a different timestamp. The client should not care about the timestamp.
    t.serverDoEchoBack(2, 12345).checkClientSentOps(3);
  });

  /**
   * Test that a double submit from the same user is nullified.
   */
  it("testDoubleSubmit", () => {
    const t = new TestConfig();
    // Simple double submit. After recovery, server sends client's own op back then ack
    //              c1 /\ c1 <-- connection broken, <-- recover before here <--- got c1 then ack c1
    //    c2 (cached) /
    t.init(0);
    t.clientDoDocOps("blip1").checkClientSentOps(0).checkClientGotOps();
    t.clientDoDocOps("blip2").checkClientSentOps().checkClientGotOps();
    // Reconnect before c1
    t.reconnectToServer().checkClientSentOpen(0);
    // Client resending c1
    t.serverDoOpen(0, 0).checkClientSentOps(0);

    // Should not send c2 on echo back, just nullify c1
    t.serverDoEchoBackDocOp(0, "blip1").checkClientSentOps().checkClientGotOps(0);
    // Send c2, once we get an ack on c1
    t.serverAck(1, 0).checkClientSentOps(1).checkClientGotOps();


    // Slightly more complicated double submit.
    //            c1 / ack <-- (2) recover from here
    //           c2 /\ c2 <-- (1) connection broken <--- (3) got c2 then ack c2 <-- (4) broken again
    // c3 (cached) /
    t.init(0);
    t.clientDoDocOps("blip1").checkClientSentOps(0).checkClientGotOps();
    t.clientDoDocOps("blip2").checkClientSentOps().checkClientGotOps();
    t.serverAck(1).checkClientSentOps(1).checkClientGotOps(0);
    t.clientDoDocOps("blip3").checkClientSentOps().checkClientGotOps();
    // Reconnect after c1
    t.reconnectToServer().checkClientSentOpen(0, 1);
    // Client resending c2
    t.serverDoOpen(1, 1).checkClientSentOps(1);

    // Should not send c2 on echo back, just nullify c2
    t.serverDoEchoBackDocOp(1, "blip2").checkClientSentOps().checkClientGotOps(1);
    // Send c3, once we get an ack on c2
    t.serverAck(2, 0).checkClientSentOps(2).checkClientGotOps();

    // Connection broken again at c2. There should be just one signature sent out in open.
    t.reconnectToServer().checkClientSentOpen(2);
  });
});
