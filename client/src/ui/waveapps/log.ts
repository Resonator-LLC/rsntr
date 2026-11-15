// The messages of an app wavelet (`app+<gadget id>`), which the node's
// apps post with `wave.post`: one document `msg+<id>` per message,
// `<m kind="..." t="<ms>">body</m>`. Read-only here: clients never write it.

import type { WaveletData } from "../../model/data";
import { DocView } from "../../model/doc";
import { WaveletId } from "../../model/id";

export const APP_WAVELET_PREFIX = "app+";
const MESSAGE_PREFIX = "msg+";

export interface AppLogMessage {
  /** The wavelet version that created it; messages are in this order. */
  readonly seq: number;
  readonly author: string;
  readonly kind: string;
  readonly body: string;
  /** Milliseconds since the epoch, as the posting node said. */
  readonly time: number | null;
}

/** The app wavelet of a gadget in a wave of `domain`. */
export function appWaveletId(domain: string, gadgetId: string): WaveletId {
  return WaveletId.of(domain, `${APP_WAVELET_PREFIX}${gadgetId}`);
}

/** The messages of an app wavelet, oldest first. */
export function appLogMessages(data: WaveletData): AppLogMessage[] {
  const messages: AppLogMessage[] = [];
  for (const doc of data.documents()) {
    if (!doc.id.startsWith(MESSAGE_PREFIX)) {
      continue;
    }
    const view = new DocView(doc.content);
    const element = view.firstChildElement(view.root());
    if (element === null || !view.hasTag(element, "m")) {
      continue;
    }
    const kind = view.attribute(element, "kind");
    if (kind === null) {
      continue;
    }
    const body = view
      .children(element)
      .map((child) => view.text(child) ?? "")
      .join("");
    const t = Number(view.attribute(element, "t"));
    messages.push({
      seq: doc.lastModifiedVersion,
      author: doc.author.address,
      kind,
      body,
      time: Number.isFinite(t) && t > 0 ? t : null,
    });
  }
  messages.sort((a, b) => a.seq - b.seq);
  return messages;
}
