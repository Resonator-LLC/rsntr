// Attachments of a wave: new attachment ids, uploads, and what the editor
// needs to show `<image attachment=…>` elements.
//
// Ids follow `AttachmentIdGeneratorImpl` of Apache Wave
// (`org.waveprotocol.wave.media.model`, Apache License 2.0): a unique token
// of the session's id generator. The original's upload popup
// (`AttachmentPopupWidget`) posts the id part alone (`AttachmentId.getId()`)
// and puts the same into the document; so does this, unless the server
// federates: then ids are `<domain>/<token>`, so that other servers know
// which server to fetch the attachment from, and attachment requests name
// the wave (`waveRef`) for the server to fetch it for.

import { IdGeneratorImpl, WaveRef, type WaveId } from "../../model/id";
import type { AttachmentService } from "../../net/http";
import type { AttachmentInfo, AttachmentSource } from "../editor/api";

/** A few random characters of the id alphabet. */
function randomToken(): string {
  const bytes = new Uint8Array(6);
  crypto.getRandomValues(bytes);
  return [...bytes].map((byte) => (byte % 36).toString(36)).join("");
}

/**
 * Generates attachment ids from the session's seed and something random:
 * there is a generator per opened wave, all with the same seed, and the
 * server refuses an id it has seen.
 */
export class AttachmentIdGenerator {
  private readonly generator: IdGeneratorImpl;

  /** `qualified`: ids name the domain (`<domain>/<token>`), for federation. */
  constructor(
    private readonly domain: string,
    seed: string,
    private readonly qualified = false,
  ) {
    const unique = seed + randomToken();
    this.generator = new IdGeneratorImpl(domain, () => unique);
  }

  /** `AttachmentIdGenerator.newAttachmentId().getId()`, or its serialised form. */
  next(): string {
    const token = this.generator.newUniqueToken();
    return this.qualified ? `${this.domain}/${token}` : token;
  }
}

/** The domain an attachment id names; "" if none. */
export function attachmentDomain(attachmentId: string): string {
  const slash = attachmentId.indexOf("/");
  return slash === -1 ? "" : attachmentId.slice(0, slash);
}

/** The file name without a path (`EditToolbar`'s `onDone`). */
export function baseFileName(fullFileName: string): string {
  const slash = fullFileName.lastIndexOf("/");
  if (slash !== -1) {
    return fullFileName.slice(slash + 1);
  }
  const backslash = fullFileName.lastIndexOf("\\");
  return backslash !== -1 ? fullFileName.slice(backslash + 1) : fullFileName;
}

function unavailable(domain: string): AttachmentInfo {
  return { fileName: "", mimeType: "", size: 0, unavailableFrom: domain };
}

/** How many ids go into one `/attachmentsInfo` request. */
const BATCH = 30;

/**
 * The attachments of one wave for the editor: metadata is fetched in
 * batches (all the ids asked for in one tick go into one request) and
 * cached. An attachment the server does not know (yet) is asked for again
 * the next time.
 */
export class WaveAttachments implements AttachmentSource {
  private readonly known = new Map<string, AttachmentInfo>();
  private readonly waiting = new Map<string, ((info: AttachmentInfo | null) => void)[]>();
  private scheduled = false;

  /** With federation: the wave, for the server to fetch other servers' attachments for. */
  private readonly waveRef: string | undefined;

  /**
   * `federation`: the domain of this server if it federates; attachments of
   * other domains are then fetched from their servers.
   */
  constructor(
    private readonly service: AttachmentService,
    private readonly waveId: WaveId,
    private readonly federation: { readonly localDomain: string } | null = null,
  ) {
    this.waveRef = federation !== null ? WaveRef.of(waveId).encodeToUriPathSegment() : undefined;
  }

  attachmentUrl(attachmentId: string): string {
    return this.service.attachmentUrl(attachmentId, this.refFor(attachmentId));
  }

  thumbnailUrl(attachmentId: string): string {
    return this.service.thumbnailUrl(attachmentId, this.refFor(attachmentId));
  }

  /** The `waveRef` to send for an attachment of another server. */
  private refFor(attachmentId: string): string | undefined {
    return this.isRemote(attachmentId) ? this.waveRef : undefined;
  }

  /**
   * Whether the attachment may be elsewhere, so that requests for it must
   * name the wave for the server to fetch it: with federation, always. An
   * id may name another server, name none (made by a client that did not
   * know its server federates), or name this domain and still be on
   * another device of the same person (device mode).
   */
  private isRemote(_attachmentId: string): boolean {
    return this.federation !== null;
  }

  /** Who to name when the attachment cannot be had. */
  private originDomain(attachmentId: string): string {
    const domain = attachmentDomain(attachmentId);
    if (domain !== "" && domain !== this.federation?.localDomain) {
      return domain;
    }
    if (domain === "" && this.waveId.domain !== this.federation?.localDomain) {
      return this.waveId.domain;
    }
    return domain === "" ? "another server" : "your other device";
  }

  info(attachmentId: string): Promise<AttachmentInfo | null> {
    const known = this.known.get(attachmentId);
    if (known !== undefined) {
      return Promise.resolve(known);
    }
    return new Promise((resolve) => {
      const waiters = this.waiting.get(attachmentId);
      if (waiters !== undefined) {
        waiters.push(resolve);
        return;
      }
      this.waiting.set(attachmentId, [resolve]);
      if (!this.scheduled) {
        this.scheduled = true;
        queueMicrotask(() => {
          this.scheduled = false;
          void this.fetchWaiting();
        });
      }
    });
  }

  private async fetchWaiting(): Promise<void> {
    const batch = [...this.waiting.entries()];
    this.waiting.clear();
    for (let i = 0; i < batch.length; i += BATCH) {
      const part = batch.slice(i, i + BATCH);
      const found = new Map<string, AttachmentInfo>();
      try {
        const ids = part.map(([id]) => id);
        const response = await this.service.getAttachmentsInfo(
          ids,
          ids.some((id) => this.isRemote(id)) ? this.waveRef : undefined,
        );
        for (const metadata of response.attachment) {
          const info: AttachmentInfo = {
            fileName: metadata.fileName,
            mimeType: metadata.mimeType,
            size: Number(metadata.size),
            ...(metadata.imageMetadata !== undefined
              ? { image: { width: metadata.imageMetadata.width, height: metadata.imageMetadata.height } }
              : {}),
            ...(metadata.thumbnailMetadata !== undefined
              ? {
                  thumbnail: {
                    width: metadata.thumbnailMetadata.width,
                    height: metadata.thumbnailMetadata.height,
                  },
                }
              : {}),
          };
          found.set(metadata.attachmentId, info);
          // The server may answer with the serialised id (`domain/id`).
          const slash = metadata.attachmentId.lastIndexOf("/");
          if (slash !== -1) {
            found.set(metadata.attachmentId.slice(slash + 1), info);
          }
        }
      } catch {
        // Unknown for now; asked again the next time.
      }
      // Another server's attachment not had: is its server unreachable?
      for (const [id] of part) {
        if (!found.has(id) && this.isRemote(id)) {
          try {
            if ((await this.service.status(id, this.waveRef)) === 503) {
              found.set(id, unavailable(this.originDomain(id)));
            }
          } catch {
            // Unknown for now.
          }
        }
      }
      for (const [id, waiters] of part) {
        const info = found.get(id) ?? null;
        if (info !== null && info.unavailableFrom === undefined) {
          this.known.set(id, info);
        }
        for (const resolve of waiters) {
          resolve(info);
        }
      }
    }
  }

  /**
   * Uploads a file as a new attachment of the wave. Rejects with the
   * error of the request if the server refuses.
   */
  async upload(attachmentId: string, file: Blob, fileName: string): Promise<void> {
    await this.service.upload(
      attachmentId,
      WaveRef.of(this.waveId).encodeToUriPathSegment(),
      file,
      baseFileName(fileName),
    );
  }
}
