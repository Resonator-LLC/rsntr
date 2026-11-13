// Federation in the wave panel: attachment ids and requests, remote
// participants, the probe of the add-participant dialog.

import { describe, expect, test } from "vitest";

import { WaveId } from "../../../model/id";
import type { AttachmentService } from "../../../net/http";
import type { AttachmentsResponse } from "../../../wire";
import { AttachmentIdGenerator, WaveAttachments, attachmentDomain } from "../attachments";
import { describeProbe, probeStateName, remoteDomain, shortDomain } from "../chrome";

/** An attachment service that knows `known` and answers `status` with 503. */
function service(known: string[]): AttachmentService & { asked: [string[], string | undefined][] } {
  const asked: [string[], string | undefined][] = [];
  return {
    asked,
    attachmentUrl: (id, waveRef) => `/attachment/${id}${waveRef !== undefined ? `?waveRef=${waveRef}` : ""}`,
    thumbnailUrl: (id, waveRef) => `/thumbnail/${id}${waveRef !== undefined ? `?waveRef=${waveRef}` : ""}`,
    getAttachmentsInfo: async (ids, waveRef): Promise<AttachmentsResponse> => {
      asked.push([[...ids], waveRef]);
      return {
        attachment: ids
          .filter((id) => known.includes(id))
          .map((id) => ({
            attachmentId: id,
            waveRef: "",
            fileName: `${id}.png`,
            mimeType: "image/png",
            size: 3n,
            creator: "bob@b.example",
            downloadUrl: "",
            thumbnailUrl: "",
          })),
      } as unknown as AttachmentsResponse;
    },
    status: async () => 503,
    upload: async () => {},
  };
}

describe("attachments with federation", () => {
  test("ids name the domain only with federation", () => {
    expect(new AttachmentIdGenerator("a.example", "seed").next()).not.toContain("/");
    const id = new AttachmentIdGenerator("a.example", "seed", true).next();
    expect(attachmentDomain(id)).toBe("a.example");
    expect(attachmentDomain("plain")).toBe("");
  });

  test("other servers' attachments name the wave; an unreachable one is unavailable", async () => {
    const backend = service(["b.example/known"]);
    const wave = WaveId.of("a.example", "w+abc");
    const attachments = new WaveAttachments(backend, wave, { localDomain: "a.example" });
    expect(attachments.attachmentUrl("b.example/known")).toBe("/attachment/b.example/known?waveRef=a.example/w+abc");
    // Its own domain's too: it may be on another device of the person.
    expect(attachments.attachmentUrl("a.example/mine")).toBe("/attachment/a.example/mine?waveRef=a.example/w+abc");
    const [known, missing] = await Promise.all([
      attachments.info("b.example/known"),
      attachments.info("b.example/offline"),
    ]);
    expect(known?.fileName).toBe("b.example/known.png");
    expect(missing?.unavailableFrom).toBe("b.example");
    expect(backend.asked[0]![1]).toBe("a.example/w+abc");
    // Unavailable is not remembered: asked again next time.
    await attachments.info("b.example/offline");
    expect(backend.asked.length).toBe(2);
  });

  test("generators with one seed make different ids", () => {
    const one = new AttachmentIdGenerator("a.example", "seed").next();
    const other = new AttachmentIdGenerator("a.example", "seed").next();
    expect(one).not.toBe(other);
    expect(one.startsWith("seed")).toBe(true);
  });

  test("an id without a domain may be on another server", async () => {
    const backend = service([]);
    const local = new WaveAttachments(backend, WaveId.of("a.example", "w+abc"), { localDomain: "a.example" });
    // It may be here or on a participant's server: the server finds out.
    expect(local.attachmentUrl("plain")).toBe("/attachment/plain?waveRef=a.example/w+abc");
    const remote = new WaveAttachments(backend, WaveId.of("b.example", "w+abc"), { localDomain: "a.example" });
    expect(remote.attachmentUrl("plain")).toBe("/attachment/plain?waveRef=b.example/w+abc");
    expect((await remote.info("plain"))?.unavailableFrom).toBe("b.example");
  });

  test("without federation nothing changes", async () => {
    const backend = service([]);
    const attachments = new WaveAttachments(backend, WaveId.of("a.example", "w+abc"));
    expect(attachments.attachmentUrl("b.example/x")).toBe("/attachment/b.example/x");
    expect(await attachments.info("b.example/x")).toBe(null);
    expect(backend.asked[0]![1]).toBe(undefined);
  });
});

describe("remote participants", () => {
  test("the domain of other servers' participants is badged", () => {
    expect(remoteDomain("a.example", "bob@b.example")).toBe("b.example");
    expect(remoteDomain("a.example", "alice@a.example")).toBe(null);
    expect(remoteDomain(null, "bob@b.example")).toBe(null);
    expect(shortDomain("b.example")).toBe("b");
    expect(shortDomain("yryyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy")).toBe("yryyyy…");
  });

  test("probe results are described", () => {
    const result = (reachable: boolean, allowed: boolean, exists: boolean | null) => ({
      reachable,
      allowed,
      exists,
      message: null,
    });
    expect(probeStateName("checking")).toBe("checking");
    expect(probeStateName(result(true, true, true))).toBe("reachable");
    expect(probeStateName(result(true, true, false))).toBe("unknown-address");
    expect(probeStateName(result(false, true, null))).toBe("unreachable");
    expect(probeStateName(result(false, false, null))).toBe("not-allowed");
    expect(describeProbe(result(false, false, null), "b.example")).toContain("does not federate with b.example");
    expect(describeProbe(result(false, true, null), "b.example")).toContain("unreachable");
  });
});
