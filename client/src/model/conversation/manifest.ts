// The conversation manifest: the document that holds the thread structure
// of a conversation (`DocumentBasedManifest`, `DocumentBasedManifestThread`,
// `DocumentBasedManifestBlip`).
//
//   <conversation anchorWavelet="..." anchorBlip="...">   root thread
//     <blip id="b+1">
//       <thread id="b+2" inline="true">                   reply thread
//         <blip id="b+2"/>
//       </thread>
//     </blip>
//   </conversation>
//
// A thread's blips are its `blip` child elements and a blip's reply threads
// its `thread` child elements, in document order; children with other tags
// and text are ignored.
//
// TypeScript port of the Rust crate `resonator-model` (module
// `conversation::manifest`), itself derived from
// `org.waveprotocol.wave.model.conversation` of Apache Wave, licensed under
// the Apache License, Version 2.0.

import { DocError, DocView, type MutableDocument, type NodeId } from "../doc";
import type { BootstrapDocument } from "../docop";

export const MANIFEST_TOP_TAG = "conversation";
export const BLIP_TAG = "blip";
export const THREAD_TAG = "thread";
export const BLIP_ID_ATTR = "id";
export const THREAD_ID_ATTR = "id";
export const INLINE_ATTR = "inline";
export const ANCHOR_WAVELET_ATTR = "anchorWavelet";
export const ANCHOR_BLIP_ATTR = "anchorBlip";

/**
 * Where a conversation is anchored: the serialised wavelet id of another
 * conversation and a blip in it (`AnchorData`). Either may be missing.
 */
export interface AnchorData {
  readonly conversationId: string | null;
  readonly blipId: string | null;
}

/** A thread entry of the manifest. */
export interface ManifestThread {
  /** `null` if the element has no id (as for the root thread). */
  readonly id: string | null;
  readonly inline: boolean;
  readonly blips: readonly ManifestBlip[];
  /** Location of the element in the manifest document it was read from. */
  readonly location: number;
}

/** A blip entry of the manifest. */
export interface ManifestBlip {
  /** `null` if the element has no id. */
  readonly id: string | null;
  readonly threads: readonly ManifestThread[];
  /** Location of the element in the manifest document it was read from. */
  readonly location: number;
}

/** The content of a manifest document at one moment. */
export interface Manifest {
  readonly anchor: AnchorData;
  readonly rootThread: ManifestThread;
}

export function anchorDataEquals(a: AnchorData, b: AnchorData): boolean {
  return a.conversationId === b.conversationId && a.blipId === b.blipId;
}

function readThread(view: DocView, element: NodeId): ManifestThread {
  // Serializer.BOOLEAN: Boolean.valueOf.
  const inline = view.attribute(element, INLINE_ATTR);
  return {
    id: view.attribute(element, THREAD_ID_ATTR),
    inline: inline !== null && inline.toLowerCase() === "true",
    blips: view
      .childElements(element)
      .filter((child) => view.hasTag(child, BLIP_TAG))
      .map((child) => readBlip(view, child)),
    location: view.location(element),
  };
}

function readBlip(view: DocView, element: NodeId): ManifestBlip {
  return {
    id: view.attribute(element, BLIP_ID_ATTR),
    threads: view
      .childElements(element)
      .filter((child) => view.hasTag(child, THREAD_TAG))
      .map((child) => readThread(view, child)),
    location: view.location(element),
  };
}

/**
 * The manifest element: the first `conversation` element of the document
 * (the canonical element of the original's `DocumentBasedSingleton`).
 */
export function manifestElement(view: DocView): NodeId | null {
  return view.elementWithTagName(MANIFEST_TOP_TAG, view.root());
}

/** Reading manifests. */
export const Manifest = {
  /** Reads the manifest of a document, or `null` if it has none. */
  read(doc: BootstrapDocument): Manifest | null {
    return Manifest.fromView(new DocView(doc));
  },

  fromView(view: DocView): Manifest | null {
    const top = manifestElement(view);
    if (top === null) {
      return null;
    }
    return {
      anchor: {
        conversationId: view.attribute(top, ANCHOR_WAVELET_ATTR),
        blipId: view.attribute(top, ANCHOR_BLIP_ATTR),
      },
      rootThread: readThread(view, top),
    };
  },
} as const;

/**
 * Whether the first element of a document is a manifest
 * (`DocumentBasedManifest.documentHasManifest`).
 */
export function documentHasManifest(doc: BootstrapDocument): boolean {
  const view = new DocView(doc);
  const top = view.firstChildElement(view.root());
  return top !== null && view.hasTag(top, MANIFEST_TOP_TAG);
}

/**
 * Creates an empty manifest in an empty document
 * (`DocumentBasedManifest.initialiseDocumentManifest`). Throws
 * {@link DocError} if the document has any content.
 */
export function initialiseDocumentManifest(doc: MutableDocument): void {
  const view = doc.view();
  if (view.firstChild(view.root()) !== null) {
    throw DocError.structure("Document already has top-level node");
  }
  doc.createChildElement(view.root(), MANIFEST_TOP_TAG);
}

/**
 * Deletes the manifest, which must be the first node of the document
 * (`DocumentBasedManifest.delete`).
 */
export function deleteManifest(doc: MutableDocument): void {
  const view = doc.view();
  const first = view.firstChild(view.root());
  if (first === null) {
    throw DocError.structure("Document has no top-level node");
  }
  if (!view.hasTag(first, MANIFEST_TOP_TAG)) {
    throw DocError.structure("First node is not a conversation manifest");
  }
  doc.deleteNode(first);
}
