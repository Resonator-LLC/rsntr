// Port of `org.waveprotocol.wave.concurrencycontrol.client.ClientAndServerTest`
// of Apache Wave (Apache License, Version 2.0).
//
// This tests the CC works with a client and server component. The actual
// operations used in this test are secondary.

import { describe, expect, it } from "vitest";

import { ParticipantId } from "../../../model/id";
import { WaveletDelta } from "../../../model/operation";
import { ChannelException } from "../../common";
import { ConcurrencyControlCore, SimpleDeltaHistory } from "../../server";
import { ConcurrencyControl } from "..";
import { ClientMock } from "./clientMock";
import { DeltaTestUtil, TestDoc, genSignature } from "./helpers";
import { ServerConnectionMock } from "./serverConnectionMock";
import { ServerMock } from "./serverMock";

const NOBODY_UTIL = new DeltaTestUtil("nobody@example.com");

/**
 * Test config to pretend the editor did something or we got something on the
 * wire from the server.
 */
class TestConfig {
  /** Always start at version 0. */
  private readonly history = new SimpleDeltaHistory(genSignature(0));
  private readonly serverMock = new ServerMock(
    new ConcurrencyControlCore(this.history),
    this.history,
  );
  private readonly clientMocks: ClientMock[] = [];

  /**
   * @param injectV0Delta by default we inject a NoOp at V0 due to a security
   *        constraint on OT, where you can't transform against V0. By
   *        injecting an op at v0, all clients can happily submit deltas
   *        concurrently.
   */
  constructor(intialBlipXml: string, numClients: number, injectV0Delta: boolean) {
    for (let i = 0; i < numClients; i++) {
      const serverConnectionMock = new ServerConnectionMock();
      serverConnectionMock.setServerMock(this.serverMock);
      this.serverMock.addClientConnection(serverConnectionMock);

      const clientCC = new ConcurrencyControl(null, genSignature(0));
      serverConnectionMock.setListener(clientCC);
      const clientMock = new ClientMock(
        clientCC,
        TestDoc.parse(intialBlipXml),
        ParticipantId.newUnchecked(`${i}@example.com`),
        serverConnectionMock,
      );

      clientCC.initialise(serverConnectionMock, clientMock);
      this.clientMocks.push(clientMock);

      // Always start at version 0.
      const signature = genSignature(0);
      clientCC.onOpen(signature, signature);
    }

    if (injectV0Delta) {
      // Inject a single NoOp from a null connection, this ensures that all
      // clients submit AFTER version 0.
      const initialDelta = new WaveletDelta(NOBODY_UTIL.getAuthor(), genSignature(0), [
        NOBODY_UTIL.noOp(),
      ]);
      this.serverMock.receive(null, initialDelta);
      this.serverProcessDeltas();
    }
  }

  private client(clientNumber: number): ClientMock {
    const client = this.clientMocks[clientNumber];
    if (client === undefined) {
      throw new Error(`no client ${clientNumber}`);
    }
    return client;
  }

  /**
   * Pretend client did some insert. We don't need a version number as it's
   * inferred in client CC.
   */
  clientDoInsert(clientNumber: number, insertionPoint: number, content: string): this {
    const clientMock = this.client(clientNumber);
    clientMock.doInsert(insertionPoint, content);
    clientMock.flush();
    return this;
  }

  /** Process deltas from the clients on the server. */
  serverProcessDeltas(): void {
    this.serverMock.start();
  }

  /** Make all the clients read operations from CC. */
  private clientsReceiveServerOperations(): void {
    for (const c of this.clientMocks) {
      c.receiveServerOperations();
    }
  }

  /** Check client has a document that looks like the following. */
  checkClientDoc(clientNumber: number, xml: string): this {
    this.clientsReceiveServerOperations();
    const expected = TestDoc.parse(xml).asOperation();
    const actual = this.client(clientNumber).requireDoc().asOperation();
    expect(expected.equals(actual), `[Expected: ${expected}] [Actual: ${actual}]`).toBe(true);
    return this;
  }

  /**
   * Prevents sending the delta to the server. But the connection still seems
   * connected.
   */
  preventSending(clientNumber: number): void {
    const connection = this.client(clientNumber).getConnection();
    this.serverMock.removeClientConnection(connection);
    connection.setServerMock(null);
  }

  /** Disconnect a single client from server. */
  killClient(clientNumber: number): void {
    const connection = this.client(clientNumber).getConnection();
    this.serverMock.removeClientConnection(connection);
    connection.setOpen(false);
    connection.getReceivedDeltas().length = 0;
    connection.getSentDeltas().length = 0;
    connection.setServerMock(null);
  }

  /** Reconnect a single client to the server. Throws `ChannelException`. */
  reconnectClient(clientNumber: number): void {
    const client = this.client(clientNumber);
    const connection = client.getConnection();
    this.serverMock.addClientConnection(connection);
    connection.setServerMock(this.serverMock);
    connection.setOpen(true);
    connection.reconnect(client.getReconnectionVersions());
  }

  /** Turn all the deltas sent by a client into ghosts, or stop doing so. */
  ghostClientDeltas(clientNumber: number, ghostSend: boolean): void {
    this.client(clientNumber).getConnection().setGhostSend(ghostSend);
  }

  /** Send all the ghosted deltas from a client. */
  sendGhostDeltas(clientNumber: number): void {
    this.client(clientNumber).getConnection().sendGhosts();
  }

  /** Reboots the server, which wakes up at the given version. */
  rebootServer(version: number): void {
    this.serverMock.reboot(version);
  }
}

describe("ClientAndServerTest", () => {
  /**
   * 2 Clients, one do some insert, followed by another client doing some stuff. No transformation
   * on server
   */
  it("testSimple2Client", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 2, true);
    t.clientDoInsert(0, 2, "X").serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>Xabc</p></blip>");

    t.clientDoInsert(1, 3, "Y").serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>XYabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>XYabc</p></blip>");

    t.clientDoInsert(0, 4, "Z").serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>XYZabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>XYZabc</p></blip>");
  });

  /**
   * 3 Clients, concurrently editing.
   *
   */
  it("testConcurrent3Client", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 3, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(1, 2, "1X").clientDoInsert(2, 2, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2X1X0Xabc</p></blip>");

    // lots of concurrent editing
    t.clientDoInsert(0, 5, "A").clientDoInsert(0, 6, "B");
    t.clientDoInsert(1, 3, "E").clientDoInsert(1, 4, "F");
    t.clientDoInsert(2, 6, "G").clientDoInsert(2, 7, "H");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
  });

  /**
   * 1 Client dies and tries to recover.
   *
   * The disconnected client did not send any delta before disconnection.
   *
   */
  it("testRecoveryNoUnacknowledged", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 3, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(1, 2, "1X").clientDoInsert(2, 2, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2X1X0Xabc</p></blip>");

    t.killClient(0);
    t.clientDoInsert(0, 5, "A").clientDoInsert(0, 6, "B");

    // lots of concurrent editing without client 0 connected
    t.clientDoInsert(1, 3, "E").clientDoInsert(1, 4, "F");
    t.clientDoInsert(2, 6, "G").clientDoInsert(2, 7, "H");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1ABX0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1XGH0Xabc</p></blip>");

    // Reconnect client 1
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
  });

  /**
   * 1 Client dies and tries to recover.
   *
   * The disconnected client did set a delta before disconnection.
   * The server got the delta.
   *
   */
  it("testRecoveryUnacknowledgedRecieved", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 3, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(1, 2, "1X").clientDoInsert(2, 2, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2X1X0Xabc</p></blip>");

    t.clientDoInsert(0, 5, "A");
    t.killClient(0);
    t.clientDoInsert(0, 6, "B");

    // lots of concurrent editing without client 0 connected
    t.clientDoInsert(1, 3, "E").clientDoInsert(1, 4, "F");
    t.clientDoInsert(2, 6, "G").clientDoInsert(2, 7, "H");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1ABX0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1AXGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1AXGH0Xabc</p></blip>");

    // Reconnect client 1
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
  });

  /**
   * 1 Client dies and tries to recover.
   *
   * The disconnected client did set a delta before disconnection.
   * The delta was not received by the server.
   *
   */
  it("testRecoveryUnacknowledgedMissing", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 3, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(1, 2, "1X").clientDoInsert(2, 2, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2X1X0Xabc</p></blip>");

    t.preventSending(0);
    t.clientDoInsert(0, 5, "A");
    t.clientDoInsert(0, 6, "B");
    t.killClient(0);

    // lots of concurrent editing without client 0 connected
    t.clientDoInsert(1, 3, "E").clientDoInsert(1, 4, "F");
    t.clientDoInsert(2, 6, "G").clientDoInsert(2, 7, "H");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1ABX0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1XGH0Xabc</p></blip>");

    // Reconnect client 1
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1ABXGH0Xabc</p></blip>");
  });

  /**
   * 1 Client dies and tries to recover several time.
   *
   */
  it("testFlakyClient", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 3, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(1, 2, "1X").clientDoInsert(2, 2, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2X1X0Xabc</p></blip>");

    t.killClient(0);

    // lots of concurrent editing without client 0 connected
    t.clientDoInsert(1, 3, "E").clientDoInsert(1, 4, "F");
    t.clientDoInsert(2, 6, "G").clientDoInsert(2, 7, "H");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2X1X0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1XGH0Xabc</p></blip>");

    // Reconnect client 1
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2EFX1XGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2EFX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2EFX1XGH0Xabc</p></blip>");

    // more edits
    t.clientDoInsert(1, 3, "+");
    t.clientDoInsert(2, 4, "-");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>2+E-FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2+E-FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2+E-FX1XGH0Xabc</p></blip>");

    t.killClient(0);
    t.clientDoInsert(0, 2, "?");

    // more edits
    t.clientDoInsert(1, 4, "+");
    t.clientDoInsert(2, 6, "-");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>?2+E-FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2++E--FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2++E--FX1XGH0Xabc</p></blip>");

    // Reconnect client 1
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>?2++E--FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>?2++E--FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>?2++E--FX1XGH0Xabc</p></blip>");

    t.killClient(0);
    t.clientDoInsert(0, 3, "?");
    t.checkClientDoc(0, "<blip><p>??2++E--FX1XGH0Xabc</p></blip>");

    // Reconnect client 1
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>??2++E--FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>??2++E--FX1XGH0Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>??2++E--FX1XGH0Xabc</p></blip>");
  });

  /**
   * Server crashes with 1 client and reset to version 0
   *
   * The client connects back to an older version
   *
   */
  it("testRecoveryServerCrash1ClientReset0", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 1, false);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(0, 4, "1X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");

    t.killClient(0);
    t.rebootServer(0);
    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");
  });

  /**
   * Server crashes with 1 client without reset to version 0
   *
   * The client connects back to an older version
   *
   */
  it("testRecoveryServerCrash1Client", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 1, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(0, 4, "1X").clientDoInsert(0, 6, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1X2Xabc</p></blip>");

    t.killClient(0);
    t.rebootServer(1);

    t.clientDoInsert(0, 8, "0Y");
    t.checkClientDoc(0, "<blip><p>0X1X2X0Yabc</p></blip>");

    t.reconnectClient(0);
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1X2X0Yabc</p></blip>");
  });


  /**
   * Server crashes with 2 client
   *
   * The client connects back to an older version
   *
   */
  it("testRecoveryServerCrash2Clients", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 2, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(0, 4, "1X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>0X1Xabc</p></blip>");

    t.killClient(0);
    t.killClient(1);
    // Server wakes up at version 1
    t.rebootServer(1);

    // Clients do more stuff whilst off line
    t.clientDoInsert(0, 2, "0Y").clientDoInsert(1, 2, "1Y");

    t.reconnectClient(0);
    expect(() => t.reconnectClient(1)).toThrow(ChannelException);
    // Client 1 dead, since for v1 release we don't support recovery from
    // server crash where there are multiple client that are concurrently editing
    t.killClient(1);

    t.serverProcessDeltas();

    t.checkClientDoc(0, "<blip><p>0Y0X1Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>1Y0X1Xabc</p></blip>");
  });

  /**
   * Test a ghost submit that ended up on the server from a previous client session before
   * the client's resubmit.
   */
  it("testRecoveryGhostBeforeResubmit", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 1, false);
    // mimic delta not getting to server
    t.ghostClientDeltas(0, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(0, 4, "1X");
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");
    t.killClient(0);
    t.ghostClientDeltas(0, false);

    // server now gets client 0's delta from a previous session before client's resend
    t.sendGhostDeltas(0);
    t.reconnectClient(0);

    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");
  });

  /**
   * Test a ghost submit that ended up on the server from a previous client session after
   * a client's resubmit.
   */
  it("testRecoveryGhostAfterResubmit", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 1, false);
    // mimic delta not getting to server
    t.ghostClientDeltas(0, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(0, 4, "1X");
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");
    t.killClient(0);
    t.ghostClientDeltas(0, false);

    t.reconnectClient(0);
    // server now gets client 0's delta from a previous session after client's resend
    t.sendGhostDeltas(0);

    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X1Xabc</p></blip>");
  });


  /**
   * 1 Client dies and tries to recover, but there is a ghost delta from a previous submit.
   *
   * The disconnected client send a delta before disconnecting.
   *
   */
  it("testRecoveryWithGhost", () => {
    const t = new TestConfig("<blip><p>abc</p></blip>", 3, true);
    // mimic delta not getting to server for client 0
    t.ghostClientDeltas(0, true);
    t.clientDoInsert(0, 2, "0X").clientDoInsert(1, 2, "1X").clientDoInsert(2, 2, "2X");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X2X1Xabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>2X1Xabc</p></blip>");
    t.checkClientDoc(2, "<blip><p>2X1Xabc</p></blip>");

    t.killClient(0);
    t.ghostClientDeltas(0, false);

    t.clientDoInsert(0, 7, "A").clientDoInsert(0, 8, "B");

    // lots of concurrent editing without client 0 connected
    t.clientDoInsert(1, 2, "E").clientDoInsert(1, 3, "F");
    t.clientDoInsert(2, 7, "G").clientDoInsert(2, 8, "H");
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0X2X1ABXabc</p></blip>");
    t.checkClientDoc(1, "<blip><p>EF2X1XaGHbc</p></blip>");
    t.checkClientDoc(2, "<blip><p>EF2X1XaGHbc</p></blip>");

    // Reconnect client 0
    t.reconnectClient(0);
    // server now gets client 0's delta from a previous session
    t.sendGhostDeltas(0);
    t.serverProcessDeltas();
    t.serverProcessDeltas();
    t.checkClientDoc(0, "<blip><p>0XEF2X1ABXaGHbc</p></blip>");
    t.checkClientDoc(1, "<blip><p>0XEF2X1ABXaGHbc</p></blip>");
    t.checkClientDoc(2, "<blip><p>0XEF2X1ABXaGHbc</p></blip>");
  });
});
