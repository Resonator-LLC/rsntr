// An `EditorDocument` on its own, for tests and harnesses.

import { MutableDocument, StandaloneDocument, XmlFragment, type DocumentSink } from "../../model/doc";
import { BootstrapDocument, type DocOp } from "../../model/docop";
import { BLIP_SCHEMA_CONSTRAINTS as BLIP_SCHEMA } from "../../model/schema/conversation";
import type { DocumentOpEvent, EditorDocument, Unsubscribe } from "./api";

export interface FakeDocument extends EditorDocument {
  /** Applies an operation as if received from another participant. */
  applyRemote(op: DocOp): void;
  /** The document as canonical XML. */
  xml(): string;
  /** Every operation applied so far, oldest first. */
  readonly history: readonly DocumentOpEvent[];
}

export const DEFAULT_FAKE_XML = "<body><line/></body>";

/**
 * A document that starts as `initialXml` (default: an empty blip body).
 * Operations are validated against the blip schema, as a wavelet does.
 */
export function createFakeDocument(initialXml: string = DEFAULT_FAKE_XML): FakeDocument {
  const standalone = new StandaloneDocument(new BootstrapDocument());
  if (initialXml !== "") {
    standalone.mutable().insertXml(0, XmlFragment.parse(initialXml));
  }
  const listeners = new Set<(event: DocumentOpEvent) => void>();
  const history: DocumentOpEvent[] = [];
  const announce = (op: DocOp, local: boolean): void => {
    const event = { op, local };
    history.push(event);
    for (const listener of [...listeners]) {
      listener(event);
    }
  };
  const apply = (op: DocOp, local: boolean): void => {
    standalone.document().consume(op, BLIP_SCHEMA);
    announce(op, local);
  };
  const sink: DocumentSink = {
    document: () => standalone.document(),
    consume: (op) => apply(op, true),
  };
  return {
    history,
    document: () => standalone.document(),
    edit: (change) => change(new MutableDocument(sink)),
    applyOp: (op) => apply(op, true),
    applyRemote: (op) => apply(op, false),
    onOp(listener): Unsubscribe {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    xml: () => standalone.document().toXmlString(),
  };
}
