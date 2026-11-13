// Port of `org.waveprotocol.wave.concurrencycontrol.client.DeltaPairTest` of
// Apache Wave (Apache License, Version 2.0): we transform a delta pair
// correctly.

import { describe, expect, it } from "vitest";

import { DocOpBuilder } from "../../../model/docop";
import type { WaveletOperation } from "../../../model/operation";
import { HashedVersion } from "../../../model/version";
import { DeltaPair } from "../../common";
import { DeltaTestUtil } from "./helpers";

const CLIENT_UTIL = new DeltaTestUtil("client@example.com");
const SERVER_UTIL = new DeltaTestUtil("server@example.com");

function checkInsert(
  operation: WaveletOperation | undefined,
  location: number,
  content: string,
  remaining: number,
): void {
  const contentOp = operation?.contentOp ?? null;
  if (contentOp === null) {
    throw new Error("Did not get an insertion operation.");
  }
  const builder = new DocOpBuilder().retain(location).characters(content);
  if (remaining > 0) {
    builder.retain(remaining);
  }
  expect(builder.build().equals(contentOp)).toBe(true);
}

function checkVersionUpdate(
  operation: WaveletOperation | undefined,
  versionIncrement: number,
  distinctVersion: HashedVersion | null,
): void {
  if (operation === undefined) {
    throw new Error("missing operation");
  }
  expect(operation.isVersionUpdate()).toBe(true);
  expect(operation.context.versionIncrement).toBe(versionIncrement);
  const hashedVersion = operation.context.hashedVersion;
  if (distinctVersion === null) {
    expect(hashedVersion).toBeNull();
  } else {
    expect(hashedVersion !== null && hashedVersion.equals(distinctVersion)).toBe(true);
  }
}

describe("DeltaPairTest", () => {
  /** Test multiple server and client ops. */
  it("testMultipleClientServerOps", () => {
    // Client insert ".A.B"
    const client = [CLIENT_UTIL.insert(1, "A", 1, null), CLIENT_UTIL.insert(3, "B", 0, null)];

    // Server insert ".2.1"
    const server = [SERVER_UTIL.insert(2, "1", 0, null), SERVER_UTIL.insert(1, "2", 2, null)];

    const pair = new DeltaPair(client, server).transform();

    // Expect the transformation of the inserts are correct. If client and
    // server have the same insert point, client op is transformed to the left
    // of the server op.
    // Expect client inserts ".A..B."
    expect(pair.client.length).toBe(2);
    checkInsert(pair.client[0], 1, "A", 3);
    checkInsert(pair.client[1], 4, "B", 1);

    // Expect server inserts "..2..1"
    expect(pair.server.length).toBe(2);
    checkInsert(pair.server[0], 4, "1", 0);
    checkInsert(pair.server[1], 2, "2", 3);
  });

  /** Simple test for deltas that have the same operations and the same author. */
  it("testIsSame", () => {
    // Client insert ".A.B"
    const client = [CLIENT_UTIL.insert(1, "A", 1, null), CLIENT_UTIL.insert(3, "B", 0, null)];

    // Server insert ".A.B"
    const resultingVersion = HashedVersion.of(1, new Uint8Array([1, 2, 3, 4]));
    // Use CLIENT_UTIL to get the same author info.
    const server = [
      CLIENT_UTIL.insert(1, "A", 1, null),
      CLIENT_UTIL.insert(3, "B", 0, resultingVersion),
    ];

    // Deltas with same ops are the same, other info should be ignored.
    expect(DeltaPair.areSame(client, server)).toBe(true);

    // Transforming the ops should result in only version update server ops.
    const pair = new DeltaPair(client, server).transform();
    expect(pair.client.length).toBe(0);
    expect(pair.server.length).toBe(2);
    checkVersionUpdate(pair.server[0], 1, null);
    checkVersionUpdate(pair.server[1], 1, resultingVersion);
  });
});
