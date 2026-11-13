// The editor without a DOM: the document, the selection as document
// locations, the caret's pending styles, undo, and every edit the user can
// make, each turned into operations on the document. The view feeds it
// input events and renders what comes out; tests drive it directly.
//
// Behaviour mirrored from the original editor (Apache Wave, paths below
// `org/waveprotocol/wave/client/editor/`):
//
// * Enter (`content/paragraph/LocalParagraphEventHandler.handleEnter`): the
//   new line keeps `i`, `a`, `d`, `listyle`, and `t` only if it is `li`
//   (`ParagraphEventHandler.attributeKeptOnNewline`); Enter on an empty list
//   item acts as Backspace at its start.
// * Backspace at a line start (`handleBackspaceAtBeginning`): outdent if
//   indented; else a list item becomes a paragraph; else the line is joined
//   to the previous one, which takes this line's attributes if it was empty
//   (`maybeRemove`). Delete at a line end joins the next line the same way.
// * Tab / Shift-Tab (`EditorImpl.handleTab`): indent / outdent every line of
//   the selection; no tab character is ever inserted.
// * Headings and lists share `t` (`Paragraph.RegularStyler`, `ListStyler`).
// * Typed text inherits annotations from its left, from its right at the
//   start of a line, and never extends a link at either end
//   (`util/AnnotationBehaviourLogic`, `doodad/link/LinkAnnotationHandler`).
//   A style toggled with a collapsed selection is kept for the next typed
//   text (`content/misc/CaretAnnotations`).
// * Backspace or Delete next to an inline reply anchor does nothing; a
//   ranged deletion removes the anchors in it (the thread then loses its
//   place in the text but is not deleted) (`InlineAnchorLiveRenderer`,
//   `EditorImpl.deleteRange`).
// * Undo (`EditorUndoManagerImpl`): own operations are undoable, everything
//   else is not; the selection of the moment a step began is restored.

import { type LocationRange, transformLocation } from "../../client/locations";
import { type MutableDocument, Nindo, XmlFragment } from "../../model/doc";
import { Attributes, type DocOp } from "../../model/docop";
import type {
  Alignment,
  EditorCommand,
  EditorDocument,
  EditorState,
  ListKind,
  MentionQuery,
  TextStyle,
  Unsubscribe,
} from "./api";
import {
  BOUNDED_KEYS,
  type DocModel,
  LINK_AUTO,
  LINK_KEYS,
  LINK_MANUAL,
  type Line,
  MENTION_USER,
  RENDER_KEYS,
  STYLE_BG_COLOR,
  STYLE_COLOR,
  STYLE_FONT_FAMILY,
  STYLE_FONT_SIZE,
  STYLE_FONT_STYLE,
  STYLE_FONT_WEIGHT,
  STYLE_KEYS,
  STYLE_TEXT_DECORATION,
  STYLE_VERTICAL_ALIGN,
  clampCaret,
  firstCaret,
  lastCaret,
  lineIndexAt,
  parseDocument,
} from "./model";
import { decorations, isBold, isItalic, linkOf } from "./styles";
import {
  findUrls,
  nextGrapheme,
  nextWord,
  normalizeLink,
  prevGrapheme,
  prevWord,
  sanitizeText,
  splitLines,
} from "./text";
import { UndoManager } from "./undo";

/** A selection with direction; `anchor === focus` is a caret. */
export interface Sel {
  readonly anchor: number;
  readonly focus: number;
}

export type DeleteUnit = "char" | "word" | "line";

export interface PasteRun {
  readonly text: string;
  /** Rendering annotations (style/*, link/manual) of the run. */
  readonly annotations: Readonly<Record<string, string>>;
}

/** One line of pasted content. */
export interface PasteBlock {
  readonly type?: "" | "h1" | "h2" | "h3" | "h4" | "li";
  readonly decimal?: boolean;
  readonly indent?: number;
  readonly runs: readonly PasteRun[];
}

export interface CoreChange {
  readonly op: DocOp;
  /** Applied through the document's `edit`/`applyOp` (by anybody local). */
  readonly local: boolean;
  /** Made by this editor. */
  readonly own: boolean;
}

export const MAX_INDENT = 50; // Paragraph.MAX_INDENT
const TYPING_PAUSE_MS = 1500;

const STYLE_VALUES: Readonly<Record<TextStyle, readonly [key: string, value: string]>> = {
  bold: [STYLE_FONT_WEIGHT, "bold"],
  italic: [STYLE_FONT_STYLE, "italic"],
  underline: [STYLE_TEXT_DECORATION, "underline"],
  strikethrough: [STYLE_TEXT_DECORATION, "line-through"],
  superscript: [STYLE_VERTICAL_ALIGN, "super"],
  subscript: [STYLE_VERTICAL_ALIGN, "sub"],
};

const ALIGN_CODES: Readonly<Record<Alignment, string | null>> = { left: null, center: "c", right: "r", justify: "j" };
const ALIGN_NAMES: Readonly<Record<string, Alignment>> = { l: "left", c: "center", r: "right", j: "justify" };

type ActionKind = "type" | "delete" | "other";

export class EditorCore {
  private readonly doc: EditorDocument;
  private readonly now: () => number;
  private readonly unsubscribe: Unsubscribe;
  private cached: DocModel | null = null;
  private sel: Sel | null = null;
  /** Styles for the next typed text; null means "explicitly none". */
  private caretStyles = new Map<string, string | null>();
  private readonly listeners = new Set<(change: CoreChange) => void>();

  private readonly undoManager = new UndoManager();
  private undoSelections: (Sel | null)[] = [];
  private redoSelections: (Sel | null)[] = [];
  private pendingCheckpoint = true;
  private ownDepth = 0;
  private bypassUndo = false;
  private opCount = 0;
  private lastKind: ActionKind | null = null;
  private lastTime = 0;
  private lastSel: Sel | null = null;
  private lastTypedSpace = false;
  /** Lines to look for URLs in once the current action is done. */
  private touched: LocationRange | null = null;
  /** While set (IME composition), URL detection waits. */
  autolinkPaused = false;

  constructor(doc: EditorDocument, options: { now?: () => number } = {}) {
    this.doc = doc;
    this.now = options.now ?? (() => Date.now());
    this.unsubscribe = doc.onOp((event) => this.onOp(event.op, event.local));
  }

  destroy(): void {
    this.unsubscribe();
    this.listeners.clear();
  }

  onChange(listener: (change: CoreChange) => void): Unsubscribe {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  model(): DocModel {
    if (this.cached === null) {
      this.cached = parseDocument(this.doc.document());
    }
    return this.cached;
  }

  // ---- selection ----

  selection(): Sel | null {
    return this.sel;
  }

  range(): LocationRange | null {
    const s = this.sel;
    return s === null ? null : { start: Math.min(s.anchor, s.focus), end: Math.max(s.anchor, s.focus) };
  }

  /**
   * Sets the selection (clamped to where a caret can be). A change of
   * selection drops the caret's pending styles, as navigation does in the
   * original.
   */
  setSelection(sel: Sel | null): void {
    let next: Sel | null = null;
    if (sel !== null && this.model().lines.length > 0) {
      const m = this.model();
      const forward = sel.focus >= sel.anchor;
      next = {
        anchor: clampCaret(m, sel.anchor, forward ? "after" : "before"),
        focus: clampCaret(m, sel.focus, forward ? "before" : "after"),
      };
      if (sel.anchor === sel.focus) {
        next = { anchor: next.focus, focus: next.focus };
      }
    }
    if (!selEquals(next, this.sel)) {
      this.sel = next;
      this.caretStyles.clear();
    }
  }

  // ---- operations coming in ----

  private onOp(op: DocOp, local: boolean): void {
    const own = this.ownDepth > 0;
    const before = this.sel;
    this.cached = null;
    this.opCount++;
    if (before !== null) {
      this.sel = transformSel(before, op, own);
    }
    if (this.lastSel !== null) {
      this.lastSel = transformSel(this.lastSel, op, own);
    }
    if (!this.bypassUndo) {
      if (own) {
        if (this.pendingCheckpoint) {
          this.undoManager.checkpoint();
          this.undoSelections.push(before);
          this.pendingCheckpoint = false;
        }
        this.undoManager.undoableOp(op);
        this.redoSelections = [];
      } else {
        this.undoManager.nonUndoableOp(op);
      }
    }
    for (const listener of [...this.listeners]) {
      listener({ op, local, own });
    }
  }

  private edit<R>(change: (doc: MutableDocument) => R): R {
    this.ownDepth++;
    try {
      return this.doc.edit(change);
    } finally {
      this.ownDepth--;
    }
  }

  /** Applies a ready-made operation as an edit of this editor. */
  applyOwn(op: DocOp): void {
    this.ownDepth++;
    try {
      this.doc.applyOp(op);
    } finally {
      this.ownDepth--;
    }
  }

  /** Starts a new undo step with the next edit. */
  checkpoint(): void {
    this.pendingCheckpoint = true;
    this.lastKind = null;
  }

  /**
   * Runs one user action. Returns whether it changed the document.
   */
  private act(kind: ActionKind, action: () => void): boolean {
    const now = this.now();
    if (
      kind === "other" ||
      kind !== this.lastKind ||
      !selEquals(this.sel, this.lastSel) ||
      now - this.lastTime > TYPING_PAUSE_MS
    ) {
      this.pendingCheckpoint = true;
    }
    this.lastKind = kind;
    this.lastTime = now;
    const before = this.opCount;
    this.touched = null;
    action();
    if (this.opCount !== before && !this.autolinkPaused) {
      const r = this.touched ?? this.range();
      if (r !== null) {
        this.autolink(r.start, r.end);
      }
    }
    this.lastSel = this.sel;
    return this.opCount !== before;
  }

  // ---- reading ----

  private lineAt(location: number): { line: Line; index: number } | null {
    const m = this.model();
    if (m.lines.length === 0) {
      return null;
    }
    const index = lineIndexAt(m, location);
    return { line: m.lines[index]!, index };
  }

  private annotationsLeftOf(location: number): Map<string, string | null> {
    const d = this.doc.document();
    const result = new Map<string, string | null>();
    for (const key of RENDER_KEYS) {
      result.set(key, location > 0 ? d.getAnnotation(location - 1, key) : null);
    }
    return result;
  }

  /**
   * The annotations text typed at a location would get, before the caret's
   * pending styles: those to the left; at the start of a non-empty line the
   * styles to the right; no link across a link's boundary.
   */
  private inheritedAt(location: number): Map<string, string | null> {
    const d = this.doc.document();
    const result = this.annotationsLeftOf(location);
    const at = this.lineAt(location);
    if (at === null) {
      return result;
    }
    const hasRight = location < at.line.end;
    if (location === at.line.contentStart && hasRight) {
      for (const key of STYLE_KEYS) {
        result.set(key, d.getAnnotation(location, key));
      }
    }
    for (const key of BOUNDED_KEYS) {
      const right = hasRight ? d.getAnnotation(location, key) : null;
      if (result.get(key) !== right) {
        result.set(key, null);
      }
    }
    return result;
  }

  private effectiveAt(location: number): Map<string, string | null> {
    const result = this.inheritedAt(location);
    for (const [key, value] of this.caretStyles) {
      result.set(key, value);
    }
    return result;
  }

  /** The value of a key if all text in a range has it; undefined if mixed. */
  private uniform(start: number, end: number, key: string): string | null | undefined {
    const m = this.model();
    let value: string | null | undefined;
    let seen = false;
    for (let i = lineIndexAt(m, start); i < m.lines.length; i++) {
      const line = m.lines[i]!;
      if (line.contentStart >= end) {
        break;
      }
      for (const item of line.items) {
        const s = line.contentStart + item.offset;
        if (item.kind !== "text" || s >= end || s + item.size <= start) {
          continue;
        }
        const v = item.annotations[key] ?? null;
        if (seen && v !== value) {
          return undefined;
        }
        value = v;
        seen = true;
      }
    }
    return seen ? value : null;
  }

  canUndo(): boolean {
    return this.undoManager.canUndo();
  }

  canRedo(): boolean {
    return this.undoManager.canRedo();
  }

  state(editable: boolean, focused: boolean): EditorState {
    const r = this.range();
    const get = (key: string): string | null => {
      if (r === null) {
        return null;
      }
      if (r.start === r.end) {
        return this.effectiveAt(r.start).get(key) ?? null;
      }
      return this.uniform(r.start, r.end, key) ?? null;
    };
    const deco = decorations(get(STYLE_TEXT_DECORATION));
    const va = get(STYLE_VERTICAL_ALIGN);
    let heading: 0 | 1 | 2 | 3 | 4 = 0;
    let list: ListKind = "none";
    let alignment: Alignment = "left";
    let link: string | null = null;
    if (r !== null) {
      const lines = this.selectedLines();
      const first = lines[0];
      if (first !== undefined) {
        if (first.type.startsWith("h")) {
          heading = Number(first.type.slice(1)) as 1 | 2 | 3 | 4;
        }
        alignment = ALIGN_NAMES[first.align] ?? "left";
        if (lines.every((l) => l.type === "li" && l.decimal === first.decimal)) {
          list = first.decimal ? "numbered" : "bullet";
        }
      }
      link = this.linkRunAt(r.start)?.value ?? null;
    }
    return {
      editable,
      focused,
      hasRange: r !== null && r.start !== r.end,
      styles: {
        bold: isBold(get(STYLE_FONT_WEIGHT)),
        italic: isItalic(get(STYLE_FONT_STYLE)),
        underline: deco.underline,
        strikethrough: deco.strike,
        superscript: va === "super",
        subscript: va === "sub",
      },
      fontSize: get(STYLE_FONT_SIZE),
      fontFamily: get(STYLE_FONT_FAMILY),
      color: get(STYLE_COLOR),
      backgroundColor: get(STYLE_BG_COLOR),
      heading,
      list,
      alignment,
      link,
      canUndo: this.canUndo(),
      canRedo: this.canRedo(),
      mention: editable ? this.mentionQuery() : null,
    };
  }

  /**
   * The `@mention` typed right before a collapsed caret: an `@` at the
   * start of the line's text or after a space or opening bracket, then up
   * to 64 characters of the same line without atoms, at most two spaces,
   * not inside an existing mention or link.
   */
  mentionQuery(): MentionQuery | null {
    const r = this.range();
    if (r === null || r.start !== r.end) {
      return null;
    }
    const at = this.lineAt(r.start);
    if (at === null) {
      return null;
    }
    const { line } = at;
    const offset = r.start - line.contentStart;
    // The text of the line just before the caret, back to the last atom.
    let text = "";
    let textStart = 0;
    for (const item of line.items) {
      if (item.offset >= offset) {
        break;
      }
      if (item.kind !== "text") {
        text = "";
        textStart = item.offset + item.size;
        continue;
      }
      text += item.text.slice(0, Math.min(item.size, offset - item.offset));
    }
    const limit = Math.max(0, text.length - 65);
    for (let i = text.length - 1; i >= limit; i--) {
      if (text[i] !== "@") {
        continue;
      }
      if (i > 0 && !/[\s([{"'\u201c\u2018]/.test(text[i - 1]!)) {
        continue;
      }
      const query = text.slice(i + 1);
      if (/^\s/.test(query) || /\s\s/.test(query) || (query.match(/\s/g)?.length ?? 0) > 2) {
        return null;
      }
      const start = line.contentStart + textStart + i;
      const d = this.doc.document();
      for (let k = start; k < r.start; k++) {
        if (d.getAnnotation(k, MENTION_USER) !== null || LINK_KEYS.some((key) => d.getAnnotation(k, key) !== null)) {
          return null;
        }
      }
      return { start, end: r.start, query };
    }
    return null;
  }

  private selectedLines(): Line[] {
    const r = this.range();
    const m = this.model();
    if (r === null || m.lines.length === 0) {
      return [];
    }
    return m.lines.slice(lineIndexAt(m, r.start), lineIndexAt(m, r.end) + 1);
  }

  /** The linked stretch of text touching a location: its range and target. */
  private linkRunAt(location: number): { start: number; end: number; value: string } | null {
    const at = this.lineAt(location);
    if (at === null) {
      return null;
    }
    const { line } = at;
    const offset = location - line.contentStart;
    const items = line.items;
    const valueOf = (i: number): string | null => {
      const item = items[i];
      return item !== undefined && item.kind === "text" ? (linkOf(item.annotations)?.value ?? null) : null;
    };
    // Prefer the run the caret is inside or at the end of, then the one it is before.
    let hit = -1;
    for (let i = 0; i < items.length; i++) {
      const item = items[i]!;
      if (valueOf(i) !== null && offset >= item.offset && offset <= item.offset + item.size) {
        hit = i;
        if (offset > item.offset) {
          break;
        }
      }
    }
    if (hit < 0) {
      return null;
    }
    const value = valueOf(hit)!;
    let a = hit;
    let b = hit;
    while (valueOf(a - 1) === value) {
      a--;
    }
    while (valueOf(b + 1) === value) {
      b++;
    }
    return {
      start: line.contentStart + items[a]!.offset,
      end: line.contentStart + items[b]!.offset + items[b]!.size,
      value,
    };
  }

  /** The character of text at a location on its line; "" if none. */
  private charAt(location: number): string {
    const at = this.lineAt(location);
    if (at === null) {
      return "";
    }
    const offset = location - at.line.contentStart;
    for (const item of at.line.items) {
      if (item.kind === "text" && offset >= item.offset && offset < item.offset + item.size) {
        return item.text[offset - item.offset] ?? "";
      }
    }
    return "";
  }

  /** The text of a range, lines separated by newlines, atoms left out. */
  textOf(start: number, end: number): string {
    return this.blocksOf(start, end)
      .map((b) => b.runs.map((r) => r.text).join(""))
      .join("\n");
  }

  /** The content of a range as paste blocks (for copying). */
  blocksOf(start: number, end: number): PasteBlock[] {
    const m = this.model();
    const blocks: PasteBlock[] = [];
    if (m.lines.length === 0 || start >= end) {
      return blocks;
    }
    for (let i = lineIndexAt(m, start); i < m.lines.length; i++) {
      const line = m.lines[i]!;
      if (line.contentStart > end || (line.start >= end && blocks.length > 0)) {
        break;
      }
      const runs: PasteRun[] = [];
      for (const item of line.items) {
        if (item.kind !== "text") {
          continue;
        }
        const s = line.contentStart + item.offset;
        const from = Math.max(start, s);
        const to = Math.min(end, s + item.size);
        if (from < to) {
          runs.push({ text: item.text.slice(from - s, to - s), annotations: item.annotations });
        }
      }
      blocks.push({ type: line.type, decimal: line.decimal, indent: line.indent, runs });
    }
    return blocks;
  }

  // ---- low-level edits (inside an action) ----

  private caret(location: number): void {
    this.sel = { anchor: location, focus: location };
  }

  private setLineAttributes(lineStart: number, updates: readonly (readonly [string, string | null])[]): void {
    this.edit((d) => {
      const el = d.view().elementStartingAt(lineStart);
      if (el === null || d.view().tagName(el) !== "line") {
        return;
      }
      const changed = updates.filter(([name, value]) => d.view().attribute(el, name) !== value);
      if (changed.length > 0) {
        d.updateElementAttributes(el, changed);
      }
    });
  }

  private deleteRaw(start: number, end: number): void {
    if (start < end) {
      this.edit((d) => d.deleteRange(start, end));
    }
  }

  /** Inserts text whose rendering annotations are to be exactly `wanted`. */
  private insertAnnotated(location: number, text: string, wanted: ReadonlyMap<string, string | null>): void {
    if (text === "") {
      return;
    }
    const left = this.annotationsLeftOf(location);
    const b = Nindo.builder().skip(location);
    const started: string[] = [];
    for (const key of RENDER_KEYS) {
      const value = wanted.get(key) ?? null;
      if (value !== (left.get(key) ?? null)) {
        b.startAnnotation(key, value);
        started.push(key);
      }
    }
    b.characters(text);
    for (const key of started) {
      b.endAnnotation(key);
    }
    const nindo = b.build();
    this.edit((d) => d.consume(nindo));
  }

  private deleteSelectionRaw(): number | null {
    const r = this.range();
    if (r === null) {
      return null;
    }
    this.deleteRaw(r.start, r.end);
    this.caret(r.start);
    return r.start;
  }

  /** The stretch of text (no atoms) around an offset of a line. */
  private chunk(line: Line, offset: number, side: "before" | "after"): { text: string; start: number } | null {
    let text = "";
    let start = 0;
    for (const item of line.items) {
      if (item.kind === "text") {
        if (text === "") {
          start = item.offset;
        }
        text += item.text;
        continue;
      }
      const end = start + text.length;
      if (text !== "" && (side === "before" ? offset > start && offset <= end : offset >= start && offset < end)) {
        return { text, start };
      }
      text = "";
    }
    const end = start + text.length;
    if (text !== "" && (side === "before" ? offset > start && offset <= end : offset >= start && offset < end)) {
      return { text, start };
    }
    return null;
  }

  private joinWithPrevious(index: number): void {
    const m = this.model();
    const line = m.lines[index];
    const prev = m.lines[index - 1];
    if (line === undefined || prev === undefined || !line.hasElement) {
      return;
    }
    const prevEmpty = prev.end === prev.contentStart && prev.hasElement;
    this.edit((d) => {
      if (prevEmpty) {
        const el = d.view().elementStartingAt(line.start);
        const prevEl = d.view().elementStartingAt(prev.start);
        const attrs = el === null ? null : d.view().attributes(el);
        if (prevEl !== null && attrs !== null && !attrs.equals(d.view().attributes(prevEl) ?? Attributes.EMPTY)) {
          d.setElementAttributes(prevEl, attrs);
        }
      }
      const el = d.view().elementStartingAt(line.start);
      if (el !== null && d.view().tagName(el) === "line") {
        d.deleteNode(el);
      }
    });
    this.caret(line.start);
  }

  private indentLine(line: Line, delta: number): void {
    const next = Math.min(Math.max(0, line.indent + delta), MAX_INDENT);
    if (next !== line.indent && line.hasElement) {
      this.setLineAttributes(line.start, [["i", next === 0 ? null : String(next)]]);
    }
  }

  private backspaceAtLineStart(index: number): void {
    const line = this.model().lines[index]!;
    if (line.indent > 0) {
      this.indentLine(line, -1);
    } else if (line.type === "li") {
      this.setLineAttributes(line.start, [
        ["t", null],
        ["listyle", null],
      ]);
    } else if (index > 0) {
      this.joinWithPrevious(index);
    }
  }

  // ---- user actions ----

  /** Typed, dictated or replaced text; replaces the selection. */
  insertText(raw: string): boolean {
    if (/[\r\n\u2028\u2029]/.test(raw)) {
      return this.insertBlocks(
        splitLines(raw).map((text) => ({ runs: [{ text, annotations: {} }] })),
        false,
      );
    }
    const text = sanitizeText(raw);
    const r = this.range();
    if (r === null || (text === "" && r.start === r.end)) {
      return false;
    }
    const startsWord = this.lastTypedSpace && !/^\s/.test(text);
    if (startsWord) {
      this.lastKind = null;
    }
    this.lastTypedSpace = /\s$/.test(text);
    return this.act("type", () => {
      let wanted: Map<string, string | null>;
      if (r.start < r.end) {
        wanted = this.replacementAnnotations(r.start, r.end);
        this.deleteRaw(r.start, r.end);
      } else {
        wanted = this.inheritedAt(r.start);
      }
      for (const [key, value] of this.caretStyles) {
        wanted.set(key, value);
      }
      this.insertAnnotated(r.start, text, wanted);
      this.caretStyles.clear();
      this.caret(r.start + text.length);
    });
  }

  /** What text replacing a range gets: the annotations of its first character. */
  private replacementAnnotations(start: number, end: number): Map<string, string | null> {
    const d = this.doc.document();
    const result = new Map<string, string | null>();
    for (const key of RENDER_KEYS) {
      result.set(key, d.getAnnotation(start, key));
    }
    for (const key of BOUNDED_KEYS) {
      if (d.getAnnotation(end - 1, key) !== result.get(key)) {
        result.set(key, null);
      }
    }
    return result;
  }

  /**
   * Replaces a range with text (spellcheck and autocorrect replacements,
   * reconciled IME input). The selection ends up after the text.
   */
  replaceRange(start: number, end: number, raw: string, kind: "type" | "other" = "other"): boolean {
    const m = this.model();
    const s = clampCaret(m, Math.min(start, end), "after");
    const e = Math.max(s, clampCaret(m, Math.max(start, end), "before"));
    const text = sanitizeText(raw.replace(/[\r\n\u2028\u2029]/g, " "));
    if (s === e && text === "") {
      return false;
    }
    return this.act(kind, () => {
      const wanted = s < e ? this.replacementAnnotations(s, e) : this.inheritedAt(s);
      if (s === e) {
        for (const [key, value] of this.caretStyles) {
          wanted.set(key, value);
        }
        this.caretStyles.clear();
      }
      this.deleteRaw(s, e);
      this.insertAnnotated(s, text, wanted);
      this.caret(s + text.length);
    });
  }

  deleteSelection(): boolean {
    const r = this.range();
    if (r === null || r.start === r.end) {
      return false;
    }
    return this.act("other", () => {
      this.deleteSelectionRaw();
    });
  }

  /** Deletes a range given by the browser (word and soft-line deletions). */
  deleteRange(start: number, end: number): boolean {
    const m = this.model();
    const s = clampCaret(m, Math.min(start, end), "after");
    const e = Math.max(s, clampCaret(m, Math.max(start, end), "before"));
    if (s === e) {
      return false;
    }
    return this.act("delete", () => {
      this.deleteRaw(s, e);
      this.caret(s);
    });
  }

  deleteBackward(unit: DeleteUnit = "char"): boolean {
    const r = this.range();
    if (r === null) {
      return false;
    }
    if (r.start !== r.end) {
      return this.deleteSelection();
    }
    const at = this.lineAt(r.start)!;
    const offset = r.start - at.line.contentStart;
    if (offset <= 0) {
      return this.act("other", () => this.backspaceAtLineStart(at.index));
    }
    const chunk = this.chunk(at.line, offset, "before");
    if (chunk === null) {
      // Right after an atom.
      const atom = at.line.items.find((i) => i.offset + i.size === offset);
      if (atom === undefined || atom.kind === "reply") {
        return false;
      }
      return this.act("delete", () => {
        this.deleteRaw(at.line.contentStart + atom.offset, r.start);
        this.caret(at.line.contentStart + atom.offset);
      });
    }
    const local = offset - chunk.start;
    const from =
      unit === "char" ? prevGrapheme(chunk.text, local) : unit === "word" ? prevWord(chunk.text, local) : 0;
    const start = at.line.contentStart + chunk.start + from;
    return this.act("delete", () => {
      this.deleteRaw(start, r.start);
      this.caret(start);
    });
  }

  deleteForward(unit: DeleteUnit = "char"): boolean {
    const r = this.range();
    if (r === null) {
      return false;
    }
    if (r.start !== r.end) {
      return this.deleteSelection();
    }
    const at = this.lineAt(r.start)!;
    const offset = r.start - at.line.contentStart;
    if (r.start >= at.line.end) {
      if (at.index + 1 >= this.model().lines.length) {
        return false;
      }
      return this.act("other", () => {
        this.joinWithPrevious(at.index + 1);
        this.caret(r.start);
      });
    }
    const chunk = this.chunk(at.line, offset, "after");
    if (chunk === null) {
      const atom = at.line.items.find((i) => i.offset === offset);
      if (atom === undefined || atom.kind === "reply") {
        return false;
      }
      return this.act("delete", () => {
        this.deleteRaw(r.start, r.start + atom.size);
        this.caret(r.start);
      });
    }
    const local = offset - chunk.start;
    const to =
      unit === "char"
        ? nextGrapheme(chunk.text, local)
        : unit === "word"
          ? nextWord(chunk.text, local)
          : chunk.text.length;
    return this.act("delete", () => {
      this.deleteRaw(r.start, at.line.contentStart + chunk.start + to);
      this.caret(r.start);
    });
  }

  /** Enter: a new line at the caret. */
  enter(): boolean {
    if (this.range() === null) {
      return false;
    }
    return this.act("other", () => {
      const location = this.deleteSelectionRaw()!;
      const at = this.lineAt(location)!;
      const line = at.line;
      if (line.type === "li" && line.end === line.contentStart) {
        this.backspaceAtLineStart(at.index);
        return;
      }
      const attrs: [string, string][] = [];
      if (line.type === "li") {
        attrs.push(["t", "li"]);
        if (line.decimal) {
          attrs.push(["listyle", "decimal"]);
        }
      }
      if (line.indent > 0) {
        attrs.push(["i", String(line.indent)]);
      }
      if (line.align !== "") {
        attrs.push(["a", line.align]);
      }
      if (line.dir !== "") {
        attrs.push(["d", line.dir]);
      }
      this.edit((d) => d.createElement(location, "line", attrs));
      this.touched = { start: location, end: location + 2 };
      this.caret(location + 2);
    });
  }

  /** Tab and Shift-Tab. */
  indent(delta: 1 | -1): boolean {
    return this.act("other", () => {
      for (const line of this.selectedLines()) {
        this.indentLine(line, delta);
      }
    });
  }

  /** Pasted content, replacing the selection. */
  insertBlocks(blocks: readonly PasteBlock[], rich: boolean): boolean {
    if (this.range() === null || blocks.length === 0) {
      return false;
    }
    return this.act("other", () => {
      const location = this.deleteSelectionRaw()!;
      const line = this.lineAt(location)!.line;
      const kept: Record<string, string> = {};
      if (line.align !== "") {
        kept.a = line.align;
      }
      if (line.dir !== "") {
        kept.d = line.dir;
      }
      const plainAttrs: Record<string, string> = { ...kept };
      if (line.type === "li") {
        plainAttrs.t = "li";
        if (line.decimal) {
          plainAttrs.listyle = "decimal";
        }
      }
      if (line.indent > 0) {
        plainAttrs.i = String(line.indent);
      }
      const blockAttrs = (block: PasteBlock): Record<string, string> => {
        if (!rich) {
          return plainAttrs;
        }
        const attrs: Record<string, string> = { ...kept };
        if (block.type !== undefined && block.type !== "") {
          attrs.t = block.type;
          if (block.type === "li" && block.decimal === true) {
            attrs.listyle = "decimal";
          }
        }
        const indent = Math.min(block.indent ?? 0, MAX_INDENT);
        if (indent > 0) {
          attrs.i = String(indent);
        }
        return attrs;
      };
      const first = blocks[0]!;
      if (rich && line.hasElement && line.end === line.contentStart && (first.type ?? "") !== "") {
        const a = blockAttrs(first);
        this.setLineAttributes(line.start, [
          ["t", a.t ?? null],
          ["listyle", a.listyle ?? null],
          ["i", a.i ?? null],
        ]);
      }
      const base = this.effectiveAt(location);
      const b = Nindo.builder().skip(location);
      const explicit = new Map<string, string | null>();
      let length = 0;
      blocks.forEach((block, i) => {
        if (i > 0) {
          b.elementStart("line", Attributes.fromRecord(blockAttrs(block)));
          b.elementEnd();
          length += 2;
        }
        for (const run of block.runs) {
          const text = sanitizeText(run.text.replace(/[\r\n\u2028\u2029]/g, " "));
          if (text === "") {
            continue;
          }
          for (const key of RENDER_KEYS) {
            const value = rich ? (run.annotations[key] ?? null) : (base.get(key) ?? null);
            if (!explicit.has(key) || explicit.get(key) !== value) {
              b.startAnnotation(key, value);
              explicit.set(key, value);
            }
          }
          b.characters(text);
          length += text.length;
        }
      });
      for (const key of explicit.keys()) {
        b.endAnnotation(key);
      }
      if (length > 0) {
        const nindo = b.build();
        this.edit((d) => d.consume(nindo));
      }
      this.caretStyles.clear();
      this.touched = { start: location, end: location + length };
      this.caret(location + length);
    });
  }

  // ---- commands ----

  exec(command: EditorCommand): boolean {
    switch (command.type) {
      case "undo":
        return this.undo();
      case "redo":
        return this.redo();
      case "insertText":
        return this.insertText(command.text);
      case "indent":
        return this.indent(1);
      case "outdent":
        return this.indent(-1);
      default:
        break;
    }
    const r = this.range();
    if (r === null) {
      return false;
    }
    const collapsed = r.start === r.end;
    const annotate = (key: string, value: string | null): void => {
      if (collapsed) {
        this.caretStyles.set(key, value);
      } else {
        this.edit((d) => d.setAnnotation(r.start, r.end, key, value));
      }
    };
    const eachLine = (updates: (line: Line) => readonly (readonly [string, string | null])[]): void => {
      for (const line of this.selectedLines()) {
        if (line.hasElement) {
          this.setLineAttributes(line.start, updates(line));
        }
      }
    };
    return this.act("other", () => {
      const keep = this.sel;
      switch (command.type) {
        case "toggleStyle": {
          const [key, value] = STYLE_VALUES[command.style];
          const current = collapsed
            ? (this.effectiveAt(r.start).get(key) ?? null)
            : this.uniform(r.start, r.end, key);
          annotate(key, current === value ? null : value);
          break;
        }
        case "setFontSize":
          annotate(STYLE_FONT_SIZE, command.value);
          break;
        case "setFontFamily":
          annotate(STYLE_FONT_FAMILY, command.value);
          break;
        case "setColor":
          annotate(STYLE_COLOR, command.value);
          break;
        case "setBackgroundColor":
          annotate(STYLE_BG_COLOR, command.value);
          break;
        case "setHeading": {
          const t = command.level === 0 ? null : `h${command.level}`;
          eachLine(() => [
            ["t", t],
            ["listyle", null],
          ]);
          break;
        }
        case "toggleList": {
          const decimal = command.list === "numbered";
          const lines = this.selectedLines();
          const on = !lines.every((l) => l.type === "li" && l.decimal === decimal);
          eachLine(() => [
            ["t", on ? "li" : null],
            ["listyle", on && decimal ? "decimal" : null],
          ]);
          break;
        }
        case "setAlignment":
          eachLine(() => [["a", ALIGN_CODES[command.value]]]);
          break;
        case "setLink": {
          const href = normalizeLink(command.href);
          const target = collapsed ? this.linkRunAt(r.start) : r;
          if (href === null || target === null) {
            break;
          }
          this.edit((d) => {
            for (const key of LINK_KEYS) {
              if (key !== LINK_MANUAL && d.findAnnotationChange(target.start, target.end, key, null) !== null) {
                d.setAnnotation(target.start, target.end, key, null);
              }
            }
            d.setAnnotation(target.start, target.end, LINK_MANUAL, href);
          });
          break;
        }
        case "removeLink": {
          const target = collapsed ? this.linkRunAt(r.start) : r;
          if (target === null) {
            break;
          }
          this.edit((d) => {
            for (const key of LINK_KEYS) {
              if (d.findAnnotationChange(target.start, target.end, key, null) !== null) {
                d.setAnnotation(target.start, target.end, key, null);
              }
            }
          });
          // URL detection would put an automatic link straight back.
          this.touched = { start: 0, end: 0 };
          break;
        }
        case "clearFormatting":
          if (collapsed) {
            for (const key of STYLE_KEYS) {
              this.caretStyles.set(key, null);
            }
          } else {
            this.edit((d) => {
              for (const key of STYLE_KEYS) {
                if (d.findAnnotationChange(r.start, r.end, key, null) !== null) {
                  d.setAnnotation(r.start, r.end, key, null);
                }
              }
            });
          }
          eachLine(() => [["t", null]]);
          break;
        case "insertMention": {
          const m = this.model();
          const start = clampCaret(m, Math.min(command.start, command.end), "after");
          const end = Math.max(start, clampCaret(m, Math.max(command.start, command.end), "before"));
          const text = sanitizeText(command.text.replace(/[\r\n\u2028\u2029]/g, " "));
          const address = command.address.trim();
          if (text === "" || address === "") {
            break;
          }
          const wanted = this.inheritedAt(start);
          for (const [key, value] of this.caretStyles) {
            wanted.set(key, value);
          }
          this.caretStyles.clear();
          for (const key of LINK_KEYS) {
            wanted.set(key, null);
          }
          this.deleteRaw(start, end);
          wanted.set(MENTION_USER, address);
          this.insertAnnotated(start, text, wanted);
          let caret = start + text.length;
          // A space after it, unless one follows already.
          if (!/^\s/.test(this.charAt(caret))) {
            wanted.set(MENTION_USER, null);
            this.insertAnnotated(caret, " ", wanted);
          }
          caret += 1;
          this.caret(caret);
          return;
        }
        case "insertAttachment": {
          const at = this.sel!.focus;
          const caption = sanitizeText(command.caption.replace(/[\r\n\u2028\u2029]/g, " "));
          const xml = XmlFragment.text(caption)
            .wrap("caption")
            .wrap("image", Attributes.fromRecord({ attachment: command.attachmentId }));
          this.edit((d) => d.insertXml(at, xml));
          this.caret(at + caption.length + 4);
          return;
        }
        default:
          break;
      }
      // Formatting commands leave the selection where it was.
      this.sel = keep;
    });
  }

  undo(): boolean {
    return this.undoRedo(true);
  }

  redo(): boolean {
    return this.undoRedo(false);
  }

  private undoRedo(isUndo: boolean): boolean {
    let pair: [DocOp, DocOp | null] | null;
    try {
      pair = isUndo ? this.undoManager.undoPlus() : this.undoManager.redoPlus();
    } catch {
      this.resetUndo();
      return false;
    }
    if (pair === null) {
      return false;
    }
    const from = isUndo ? this.undoSelections : this.redoSelections;
    const to = isUndo ? this.redoSelections : this.undoSelections;
    to.push(this.sel);
    let restored = from.pop() ?? null;
    this.bypassUndo = true;
    try {
      this.applyOwn(pair[0]);
    } catch {
      this.resetUndo();
      return false;
    } finally {
      this.bypassUndo = false;
    }
    if (restored !== null && pair[1] !== null) {
      restored = transformSel(restored, pair[1], false);
    }
    const target = isUndo ? restored : changeEnd(pair[0]);
    if (target !== null) {
      const m = this.model();
      this.sel = {
        anchor: clampCaret(m, typeof target === "number" ? target : target.anchor),
        focus: clampCaret(m, typeof target === "number" ? target : target.focus),
      };
    }
    this.caretStyles.clear();
    this.pendingCheckpoint = true;
    this.lastKind = null;
    this.lastSel = this.sel;
    return true;
  }

  private resetUndo(): void {
    this.undoManager.clear();
    this.undoSelections = [];
    this.redoSelections = [];
    this.pendingCheckpoint = true;
  }

  // ---- URL detection ----

  /**
   * Makes `link/auto` of the lines touching a range be exactly the URLs in
   * their text: sets it where a URL is, removes it where none is any more.
   */
  autolink(from: number, to: number): void {
    const m = this.model();
    if (m.lines.length === 0 || (from === 0 && to === 0)) {
      return;
    }
    const firstIndex = lineIndexAt(m, from);
    const lastIndex = lineIndexAt(m, to);
    const changes: { start: number; end: number; value: string | null }[] = [];
    for (let i = firstIndex; i <= lastIndex; i++) {
      const line = m.lines[i]!;
      let text = "";
      const current: (string | null)[] = [];
      for (const item of line.items) {
        if (item.kind === "text") {
          text += item.text;
          const v = item.annotations[LINK_AUTO] ?? null;
          for (let k = 0; k < item.size; k++) {
            current.push(v);
          }
        } else {
          text += "\uFFFC".repeat(item.size);
          for (let k = 0; k < item.size; k++) {
            current.push(null);
          }
        }
      }
      const wanted: (string | null)[] = new Array<string | null>(text.length).fill(null);
      for (const url of findUrls(text)) {
        wanted.fill(url.href, url.start, url.end);
      }
      let k = 0;
      while (k < text.length) {
        if (current[k] === wanted[k]) {
          k++;
          continue;
        }
        const value = wanted[k]!;
        let e = k + 1;
        while (e < text.length && wanted[e] === value && current[e] !== value) {
          e++;
        }
        changes.push({ start: line.contentStart + k, end: line.contentStart + e, value });
        k = e;
      }
    }
    if (changes.length > 0) {
      this.edit((d) => {
        for (const c of changes) {
          d.setAnnotation(c.start, c.end, LINK_AUTO, c.value);
        }
      });
    }
  }

  /** The first and last caret locations. */
  bounds(): LocationRange {
    const m = this.model();
    return { start: firstCaret(m), end: lastCaret(m) };
  }
}

export function selEquals(a: Sel | null, b: Sel | null): boolean {
  return a === b || (a !== null && b !== null && a.anchor === b.anchor && a.focus === b.focus);
}

/**
 * Carries a selection across an operation. A caret follows text inserted at
 * it by this editor and stays put for anybody else's; the ends of a range
 * keep inserted content outside.
 */
export function transformSel(sel: Sel, op: DocOp, own: boolean): Sel {
  if (sel.anchor === sel.focus) {
    const at = transformLocation(sel.anchor, op, own ? "after" : "before");
    return { anchor: at, focus: at };
  }
  const forward = sel.anchor < sel.focus;
  const start = transformLocation(Math.min(sel.anchor, sel.focus), op, "after");
  const end = Math.max(start, transformLocation(Math.max(sel.anchor, sel.focus), op, "before"));
  return forward ? { anchor: start, focus: end } : { anchor: end, focus: start };
}

/** The location just after the last thing an operation inserted or deleted. */
function changeEnd(op: DocOp): number | null {
  let position = 0;
  let result: number | null = null;
  for (const c of op.components) {
    switch (c.type) {
      case "characters":
        position += c.characters.length;
        result = position;
        break;
      case "elementStart":
      case "elementEnd":
        position += 1;
        result = position;
        break;
      case "deleteCharacters":
      case "deleteElementStart":
      case "deleteElementEnd":
        result = position;
        break;
      case "retain":
        position += c.itemCount;
        break;
      case "replaceAttributes":
      case "updateAttributes":
        position += 1;
        break;
      default:
        break;
    }
  }
  return result;
}
