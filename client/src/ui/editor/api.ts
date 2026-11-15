// The interface of the blip editor module.
//
// The editor is self-contained: it knows nothing of waves, conversations or
// the network. It renders one blip document into a host element, keeps the
// rendering current as operations are applied to the document (by anybody),
// and, while editable, turns user input into operations on the document.
//
// The embedder (the wave panel) supplies the document through
// `EditorDocument` and gets a `BlipView` back. Read-only display of a blip
// is the same view with `setEditable(false)`.
//
// This file is the contract between the editor and the wave panel. Changes
// must be additive.

import type { LocationRange } from "../../client/locations";
import type { MutableDocument } from "../../model/doc";
import type { BootstrapDocument, DocOp } from "../../model/docop";

export type Unsubscribe = () => void;

/** One operation applied to the document. */
export interface DocumentOpEvent {
  readonly op: DocOp;
  /** Made through this `EditorDocument` (`edit`, `applyOp`), not received. */
  readonly local: boolean;
}

/**
 * The blip document as the editor sees it. For a blip of an open wave:
 * `document` is `wavelet.document(blipId)`, `edit` is `wave.editBlip`,
 * `applyOp` is `wavelet.applyDocOp` and `onOp` relays `blipContentChanged`.
 * For tests: a `StandaloneDocument` (see `fakeDocument.ts`).
 */
export interface EditorDocument {
  /** The document as it is now. */
  document(): BootstrapDocument;
  /**
   * Edits the document. Every call on `doc` is one operation: applied at
   * once, then announced through `onOp` with `local: true` before the call
   * returns. `doc` must not be kept.
   */
  edit<R>(change: (doc: MutableDocument) => R): R;
  /** Applies a ready-made operation (undo, redo); announced like an edit. */
  applyOp(op: DocOp): void;
  /** Every operation applied to the document, after it was applied. */
  onOp(listener: (event: DocumentOpEvent) => void): Unsubscribe;
}

/**
 * Where inline reply threads go. For every `<reply id=…/>` anchor the
 * editor renders a container element (not editable, kept in place across
 * re-renders of the surrounding line) and hands it to the embedder, which
 * mounts the thread in it.
 */
export interface AnchorHost {
  attach(threadId: string, container: HTMLElement): void;
  detach(threadId: string, container: HTMLElement): void;
}

/** A gadget with a wave app, as the editor found it. */
export interface AppGadget {
  /** The gadget id. */
  readonly id: string;
  /** `sha256:<hex>`. */
  readonly app: string;
  /** The bundle's attachment id. */
  readonly bundle: string;
}

/**
 * Draws wave apps: `attach` gets an element of the view, in the document,
 * for the embedder to draw the app in; `detach` when the gadget went away
 * or the view ends.
 */
export interface AppHost {
  attach(gadget: AppGadget, container: HTMLElement): void;
  detach(gadgetId: string, container: HTMLElement): void;
}

/** What is known about an attachment. Sizes in pixels and bytes. */
export interface AttachmentInfo {
  readonly fileName: string;
  readonly mimeType: string;
  readonly size: number;
  readonly image?: { readonly width: number; readonly height: number };
  readonly thumbnail?: { readonly width: number; readonly height: number };
  /**
   * Set if the attachment is another server's and that server cannot be
   * reached now: its domain. Asked for again later.
   */
  readonly unavailableFrom?: string;
}

/** Where `<image attachment=…>` elements get their content from. */
export interface AttachmentSource {
  attachmentUrl(attachmentId: string): string;
  thumbnailUrl(attachmentId: string): string;
  /** Null if unknown (not uploaded yet, or not visible to the user). */
  info(attachmentId: string): Promise<AttachmentInfo | null>;
}

export interface BlipViewOptions {
  /** The element to render into. The editor owns its children. */
  readonly host: HTMLElement;
  readonly doc: EditorDocument;
  readonly anchors?: AnchorHost;
  /** Draws the wave apps of gadgets; without it they show as chips. */
  readonly apps?: AppHost;
  readonly attachments?: AttachmentSource;
  /**
   * The `href` for a `link/wave` annotation value (a wave id or waveref).
   * Default: `#<value>`.
   */
  readonly waveLinkHref?: (value: string) => string;
  /**
   * Sees every keydown in the editable view before the editor does. Return
   * true if the embedder handled it: the editor then ignores the event (the
   * embedder calls `preventDefault` itself if it wants to).
   */
  readonly onKeyDown?: (event: KeyboardEvent) => boolean;
  /** Accessible name of the editable region. */
  readonly ariaLabel?: string;
}

export type TextStyle =
  | "bold"
  | "italic"
  | "underline"
  | "strikethrough"
  | "superscript"
  | "subscript";

export type ListKind = "none" | "bullet" | "numbered";
export type Alignment = "left" | "center" | "right" | "justify";

/** What a toolbar can ask of the editor. Applies to the selection. */
export type EditorCommand =
  | { readonly type: "toggleStyle"; readonly style: TextStyle }
  /** CSS values as the original stores them, e.g. "13pt"; null removes. */
  | { readonly type: "setFontSize"; readonly value: string | null }
  | { readonly type: "setFontFamily"; readonly value: string | null }
  | { readonly type: "setColor"; readonly value: string | null }
  | { readonly type: "setBackgroundColor"; readonly value: string | null }
  /** 0 is a normal paragraph. */
  | { readonly type: "setHeading"; readonly level: 0 | 1 | 2 | 3 | 4 }
  /** Turns the lines into list items of the kind, or back if they all are. */
  | { readonly type: "toggleList"; readonly list: "bullet" | "numbered" }
  | { readonly type: "indent" }
  | { readonly type: "outdent" }
  | { readonly type: "setAlignment"; readonly value: Alignment }
  /** `link/manual` over the selection. */
  | { readonly type: "setLink"; readonly href: string }
  /** Removes manual, auto and wave links touching the selection. */
  | { readonly type: "removeLink" }
  | { readonly type: "clearFormatting" }
  | { readonly type: "insertText"; readonly text: string }
  /** Inserts `<image attachment=id><caption>…</caption></image>` at the caret. */
  | { readonly type: "insertAttachment"; readonly attachmentId: string; readonly caption: string }
  /** A gadget with a wave app (an atom of size 2). */
  | { readonly type: "insertApp"; readonly gadgetId: string; readonly app: string; readonly bundle: string }
  /**
   * Replaces `start..end` (the typed `@query`, see `EditorState.mention`)
   * with `text` annotated `mention/user` = `address`, followed by a space.
   */
  | {
      readonly type: "insertMention";
      readonly start: number;
      readonly end: number;
      readonly text: string;
      readonly address: string;
    }
  | { readonly type: "undo" }
  | { readonly type: "redo" };

/**
 * An `@mention` being typed: the caret is collapsed right after `@query`
 * on one line, and the `@` starts a word.
 */
export interface MentionQuery {
  /** Location of the `@`. */
  readonly start: number;
  /** Location of the caret, the end of the query. */
  readonly end: number;
  /** The text typed after the `@` (may contain spaces). */
  readonly query: string;
}

/** The state a toolbar shows: the styles at the caret or over the selection. */
export interface EditorState {
  readonly editable: boolean;
  readonly focused: boolean;
  /** There is a selection in the view and it is not collapsed. */
  readonly hasRange: boolean;
  readonly styles: Readonly<Record<TextStyle, boolean>>;
  readonly fontSize: string | null;
  readonly fontFamily: string | null;
  readonly color: string | null;
  readonly backgroundColor: string | null;
  readonly heading: 0 | 1 | 2 | 3 | 4;
  readonly list: ListKind;
  readonly alignment: Alignment;
  /** The link under the caret (any kind), or null. */
  readonly link: string | null;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  /** An `@mention` being typed at the caret; null or absent if none. */
  readonly mention?: MentionQuery | null;
}

/** A live rendering of one blip document, optionally editable. */
export interface BlipView {
  /** The root element of the rendering (a child of the host, or the host). */
  readonly element: HTMLElement;
  readonly editable: boolean;
  /**
   * Switches editing on or off. Switching off first finishes whatever is
   * pending (an IME composition) so that the document has all the input.
   */
  setEditable(editable: boolean): void;
  /** Focuses the view and puts the caret at a location (default: the end). */
  focus(at?: "start" | "end" | number): void;
  /** The selection as document locations; null if it is not in this view. */
  selection(): LocationRange | null;
  setSelection(range: LocationRange): void;
  state(): EditorState;
  onStateChange(listener: (state: EditorState) => void): Unsubscribe;
  /**
   * Called after the user changed the document through this view (not for
   * remote operations), at most once per input event.
   */
  onLocalEdit(listener: () => void): Unsubscribe;
  exec(command: EditorCommand): void;
  /** Stops rendering and listening; leaves the host empty. */
  destroy(): void;
}
