// The editable DOM view of a blip: `BlipView` over `EditorCore` (the model
// side: document, selection, undo) and `Renderer` (the DOM projection).
//
// The document is the source of truth; the DOM is a projection of it. Input
// strategy, per kind of input:
//
// * Everything `beforeinput` reports outside an IME composition (typing,
//   Enter, Backspace/Delete in all units, spellcheck replacements,
//   formatting shortcuts, history) is cancelled with `preventDefault` and
//   applied to the document through the core; the renderer then patches the
//   lines that changed and the DOM selection is put back from the model.
// * IME composition (and Android keyboards, which compose everything) is
//   left to the browser: the composed line is "frozen" (the renderer does
//   not touch its DOM while remote operations arrive; other lines keep
//   updating), and at `compositionend` the line's DOM text is diffed
//   against its text at composition start. The diff is carried through the
//   remote operations that arrived meanwhile and applied as one
//   replacement; then the line is re-rendered from the model.
// * Any `input` that was not cancelled and is not part of a composition
//   (a browser that does not support cancelling some input type) is
//   reconciled the same way, line by line; if the browser broke the line
//   structure, the DOM is rebuilt from the model (the input is lost).
// * Clipboard: copy/cut write text/plain and clean text/html from the model;
//   paste reads text/html through the sanitiser (`paste.ts`) into blocks
//   with Wave annotations, or text/plain; never raw HTML. Drag and drop of
//   content inside the editor is disabled.
//
// Remote operations re-render at once; the selection is carried across
// them by the core (`transformSel`, bias "before" for remote ops) and put
// back into the DOM if the view has focus.

import type { LocationRange } from "../../client/locations";
import { transformLocation } from "../../client/locations";
import type { DocOp } from "../../model/docop";
import type {
  BlipView,
  BlipViewOptions,
  EditorCommand,
  EditorState,
  TextStyle,
  Unsubscribe,
} from "./api";
import { type CoreChange, EditorCore, type Sel, transformSel } from "./core";
import { lineIndexAt, lineText } from "./model";
import { blocksToHtml, blocksToText, isPlain, parseHtml } from "./paste";
import { Renderer } from "./render";
import { diffText } from "./text";

const FORMAT_INPUTS: Readonly<Record<string, TextStyle>> = {
  formatBold: "bold",
  formatItalic: "italic",
  formatUnderline: "underline",
  formatStrikeThrough: "strikethrough",
  formatSuperscript: "superscript",
  formatSubscript: "subscript",
};

interface Composition {
  /** The line element being composed in. */
  readonly el: HTMLElement;
  /** The line's text (atoms as U+FFFC) when the composition started. */
  readonly base: string;
  /** The line's content start then. */
  readonly contentStart: number;
  /** The line's start, carried across remote operations. */
  lineStart: number;
  /** Remote operations applied since the composition started. */
  readonly ops: DocOp[];
}

export class EditorView implements BlipView {
  readonly element: HTMLElement;
  private readonly options: BlipViewOptions;
  private readonly core: EditorCore;
  private readonly renderer: Renderer;
  private readonly cleanups: (() => void)[] = [];
  private readonly stateListeners = new Set<(state: EditorState) => void>();
  private readonly editListeners = new Set<() => void>();
  private isEditable = false;
  private actionDepth = 0;
  private dirty = false;
  private composition: Composition | null = null;
  private plainPaste = false;
  private statePending = false;
  private destroyed = false;

  constructor(options: BlipViewOptions) {
    this.options = options;
    const doc = options.host.ownerDocument;
    const root = doc.createElement("div");
    root.className = "doc";
    root.setAttribute("translate", "no");
    options.host.replaceChildren(root);
    this.element = root;
    this.core = new EditorCore(options.doc);
    this.renderer = new Renderer(root, {
      anchors: options.anchors,
      attachments: options.attachments,
      waveLinkHref: options.waveLinkHref ?? ((value) => `#${value}`),
    });
    this.renderer.render(this.core.model());
    this.cleanups.push(this.core.onChange((change) => this.onCoreChange(change)));

    const on = <K extends keyof HTMLElementEventMap>(
      type: K,
      handler: (event: HTMLElementEventMap[K]) => void,
    ): void => {
      root.addEventListener(type, handler);
      this.cleanups.push(() => root.removeEventListener(type, handler));
    };
    on("beforeinput", (e) => this.onBeforeInput(e));
    on("input", (e) => this.onInput(e as InputEvent));
    on("keydown", (e) => this.onKeyDown(e));
    on("compositionstart", (e) => this.onCompositionStart(e));
    on("compositionend", (e) => this.onCompositionEnd(e));
    on("copy", (e) => this.onCopy(e, false));
    on("cut", (e) => this.onCopy(e, true));
    on("paste", (e) => this.onPaste(e));
    on("dragstart", (e) => {
      if (this.ownsTarget(e.target)) {
        e.preventDefault();
      }
    });
    on("drop", (e) => {
      if (this.isEditable && this.ownsTarget(e.target)) {
        e.preventDefault();
      }
    });
    on("focus", () => this.emitState());
    on("blur", () => {
      if (this.composition !== null) {
        this.finishComposition(null);
      }
      this.emitState();
    });
    const onSelectionChange = (): void => this.onSelectionChange();
    doc.addEventListener("selectionchange", onSelectionChange);
    this.cleanups.push(() => doc.removeEventListener("selectionchange", onSelectionChange));
    this.setEditable(false);
  }

  // ---- BlipView ----

  get editable(): boolean {
    return this.isEditable;
  }

  setEditable(editable: boolean): void {
    if (!editable && this.composition !== null) {
      this.finishComposition(null);
    }
    this.isEditable = editable;
    const root = this.element;
    root.contentEditable = editable ? "true" : "false";
    root.classList.toggle("doc-editable", editable);
    if (editable) {
      root.setAttribute("role", "textbox");
      root.setAttribute("aria-multiline", "true");
      root.setAttribute("spellcheck", "true");
      if (this.options.ariaLabel !== undefined) {
        root.setAttribute("aria-label", this.options.ariaLabel);
      }
    } else {
      root.removeAttribute("contenteditable");
      root.removeAttribute("role");
      root.removeAttribute("aria-multiline");
      root.removeAttribute("aria-label");
      root.removeAttribute("spellcheck");
    }
    this.emitState();
  }

  focus(at: "start" | "end" | number = "end"): void {
    const bounds = this.core.bounds();
    const location = at === "start" ? bounds.start : at === "end" ? bounds.end : at;
    this.element.focus({ preventScroll: true });
    this.core.setSelection({ anchor: location, focus: location });
    this.writeDomSelection(true);
    this.emitState();
  }

  selection(): LocationRange | null {
    return this.core.range();
  }

  setSelection(range: LocationRange): void {
    this.core.setSelection({ anchor: range.start, focus: range.end });
    this.writeDomSelection(false);
    this.emitState();
  }

  state(): EditorState {
    return this.core.state(this.isEditable, this.hasFocus());
  }

  onStateChange(listener: (state: EditorState) => void): Unsubscribe {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  onLocalEdit(listener: () => void): Unsubscribe {
    this.editListeners.add(listener);
    return () => this.editListeners.delete(listener);
  }

  exec(command: EditorCommand): void {
    if (!this.isEditable) {
      return;
    }
    if (this.composition !== null) {
      this.finishComposition(null);
    }
    if (!this.hasFocus()) {
      this.element.focus({ preventScroll: true });
    }
    this.runAction(() => this.core.exec(command));
  }

  destroy(): void {
    if (this.destroyed) {
      return;
    }
    this.destroyed = true;
    for (const cleanup of this.cleanups) {
      cleanup();
    }
    this.stateListeners.clear();
    this.editListeners.clear();
    this.core.destroy();
    this.renderer.destroy();
    this.element.remove();
  }

  // ---- model → DOM ----

  private onCoreChange(change: CoreChange): void {
    const comp = this.composition;
    if (comp !== null && !change.own) {
      comp.ops.push(change.op);
      comp.lineStart = transformLocation(comp.lineStart, change.op, "after");
    }
    if (this.actionDepth > 0) {
      this.dirty = true;
      return;
    }
    // A remote operation (or an edit through the document by somebody
    // else locally). The DOM selection may be newer than the core's (the
    // selectionchange event is asynchronous): read it against the old
    // rendering and carry it across the operation.
    if (comp === null && this.hasFocus()) {
      const domSel = this.readDomSelection();
      if (domSel !== null) {
        const carried = transformSel(domSel, change.op, false);
        const current = this.core.selection();
        if (current === null || carried.anchor !== current.anchor || carried.focus !== current.focus) {
          this.core.setSelection(carried);
        }
      }
    }
    this.render();
    if (comp === null) {
      this.writeDomSelection(false);
    }
    this.emitState();
  }

  private render(): void {
    const model = this.core.model();
    const comp = this.composition;
    if (comp !== null && model.lines.length > 0) {
      const index = lineIndexAt(model, comp.lineStart);
      this.renderer.render(model, { el: comp.el, index });
    } else {
      if (!this.renderer.structureIntact()) {
        this.renderer.restoreStructure();
      }
      this.renderer.render(model);
    }
    this.dirty = false;
  }

  private hasFocus(): boolean {
    const doc = this.element.ownerDocument;
    return doc.activeElement === this.element && doc.hasFocus();
  }

  /** Puts the core's selection into the DOM, if this view has focus. */
  private writeDomSelection(scroll: boolean): void {
    if (!this.hasFocus() && !scroll) {
      return;
    }
    const sel = this.core.selection();
    const domSel = this.element.ownerDocument.getSelection();
    if (sel === null || domSel === null) {
      return;
    }
    const a = this.renderer.domAt(sel.anchor);
    const f = this.renderer.domAt(sel.focus);
    if (a === null || f === null) {
      return;
    }
    if (
      domSel.anchorNode !== a.node ||
      domSel.anchorOffset !== a.offset ||
      domSel.focusNode !== f.node ||
      domSel.focusOffset !== f.offset
    ) {
      try {
        domSel.setBaseAndExtent(a.node, a.offset, f.node, f.offset);
      } catch {
        // A position the browser does not accept; leave the selection.
      }
    }
    if (scroll) {
      const line = this.renderer.lineElement(lineIndexAt(this.core.model(), sel.focus));
      line?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
    }
  }

  // ---- DOM → model ----

  /** Whether an event target is this view's editing host. */
  private ownsTarget(target: EventTarget | null): boolean {
    if (target === this.element) {
      return true;
    }
    // Text node or element inside: the nearest editor root must be ours.
    const node = target as Node | null;
    if (node === null || !this.element.contains(node)) {
      return false;
    }
    const el = node.nodeType === 1 ? (node as Element) : node.parentElement;
    return el?.closest(".doc") === this.element && el.closest(".doc-anchor") === null;
  }

  /** The DOM selection as locations, or null if it is not (all) in this view. */
  private readDomSelection(): Sel | null {
    const domSel = this.element.ownerDocument.getSelection();
    if (domSel === null || domSel.rangeCount === 0 || domSel.anchorNode === null || domSel.focusNode === null) {
      return null;
    }
    if (!this.ownsTarget(domSel.anchorNode) || !this.ownsTarget(domSel.focusNode)) {
      return null;
    }
    const a = this.renderer.locationOf(domSel.anchorNode, domSel.anchorOffset);
    const f = this.renderer.locationOf(domSel.focusNode, domSel.focusOffset);
    if (a === null || f === null || a.inAnchor || f.inAnchor) {
      return null;
    }
    return { anchor: a.location, focus: f.location };
  }

  private syncFromDom(): void {
    const sel = this.readDomSelection();
    if (sel !== null) {
      this.core.setSelection(sel);
    }
  }

  private onSelectionChange(): void {
    if (this.destroyed || this.composition !== null || this.actionDepth > 0) {
      return;
    }
    const sel = this.readDomSelection();
    if (sel === null) {
      return;
    }
    const before = this.core.selection();
    this.core.setSelection(sel);
    const after = this.core.selection();
    if (before === null || after === null || before.anchor !== after.anchor || before.focus !== after.focus) {
      this.emitState();
    }
  }

  /** Runs a user action and brings the DOM up to date. */
  private runAction(action: () => boolean): boolean {
    let changed = false;
    this.actionDepth++;
    try {
      changed = action();
    } catch (error) {
      console.error("editor: an edit failed", error);
      this.dirty = true;
    } finally {
      this.actionDepth--;
    }
    if (this.dirty || changed) {
      this.render();
    }
    this.writeDomSelection(true);
    if (changed) {
      for (const listener of [...this.editListeners]) {
        listener();
      }
    }
    this.emitState();
    return changed;
  }

  private onBeforeInput(e: InputEvent): void {
    if (!this.isEditable || e.target !== this.element) {
      return;
    }
    const type = e.inputType;
    if (e.isComposing || this.composition !== null || type === "insertCompositionText") {
      return; // reconciled at compositionend
    }
    if (type === "deleteCompositionText" || type === "insertFromComposition") {
      return; // Safari's end of a composition; reconciled by `input`
    }
    if (e.cancelable) {
      e.preventDefault();
    } else {
      return; // reconciled by `input`
    }
    this.syncFromDom();
    const core = this.core;
    const style = FORMAT_INPUTS[type];
    if (style !== undefined) {
      this.runAction(() => core.exec({ type: "toggleStyle", style }));
      return;
    }
    switch (type) {
      case "insertText": {
        const text = e.data ?? e.dataTransfer?.getData("text/plain") ?? "";
        this.runAction(() => core.insertText(text));
        return;
      }
      case "insertReplacementText": {
        const text = e.dataTransfer?.getData("text/plain") ?? e.data ?? "";
        const range = e.getTargetRanges()[0];
        const s = range === undefined ? null : this.renderer.locationOf(range.startContainer, range.startOffset);
        const t = range === undefined ? null : this.renderer.locationOf(range.endContainer, range.endOffset);
        this.runAction(() =>
          s !== null && t !== null ? core.replaceRange(s.location, t.location, text) : core.insertText(text),
        );
        return;
      }
      case "insertParagraph":
      case "insertLineBreak":
        this.runAction(() => core.enter());
        return;
      case "deleteContentBackward":
        this.runAction(() => core.deleteBackward("char"));
        return;
      case "deleteWordBackward":
        this.runAction(() => core.deleteBackward("word"));
        return;
      case "deleteSoftLineBackward":
      case "deleteHardLineBackward":
        this.runAction(() => core.deleteBackward("line"));
        return;
      case "deleteContentForward":
      case "deleteContent":
        this.runAction(() => core.deleteForward("char"));
        return;
      case "deleteWordForward":
        this.runAction(() => core.deleteForward("word"));
        return;
      case "deleteSoftLineForward":
      case "deleteHardLineForward":
        this.runAction(() => core.deleteForward("line"));
        return;
      case "historyUndo":
        this.runAction(() => core.undo());
        return;
      case "historyRedo":
        this.runAction(() => core.redo());
        return;
      case "insertOrderedList":
        this.runAction(() => core.exec({ type: "toggleList", list: "numbered" }));
        return;
      case "insertUnorderedList":
        this.runAction(() => core.exec({ type: "toggleList", list: "bullet" }));
        return;
      case "formatIndent":
        this.runAction(() => core.indent(1));
        return;
      case "formatOutdent":
        this.runAction(() => core.indent(-1));
        return;
      default:
        // Paste and cut come through their clipboard events, drops are
        // refused, other formatting input has no Wave equivalent.
        return;
    }
  }

  private onInput(e: InputEvent): void {
    if (!this.isEditable || e.target !== this.element || this.composition !== null || e.isComposing) {
      return;
    }
    this.reconcileAll();
  }

  /** Brings the document in line with whatever the browser did to the DOM. */
  private reconcileAll(): void {
    if (!this.renderer.structureIntact()) {
      this.renderer.restoreStructure();
      const model = this.core.model();
      this.renderer.render(model);
      model.lines.forEach((_, i) => this.renderer.rerenderLine(i));
      this.writeDomSelection(true);
      return;
    }
    const model = this.core.model();
    const domSel = this.element.ownerDocument.getSelection();
    const edits: { start: number; end: number; text: string }[] = [];
    model.lines.forEach((line, i) => {
      const dom = this.renderer.domLineText(i);
      if (dom === null) {
        return;
      }
      let caret: number | undefined;
      if (domSel?.focusNode != null && this.renderer.lineElement(i)?.contains(domSel.focusNode)) {
        caret = (this.renderer.locationOf(domSel.focusNode, domSel.focusOffset)?.location ?? line.contentStart) -
          line.contentStart;
      }
      const diff = diffText(lineText(line), dom.text, caret);
      if (diff !== null) {
        const start = line.contentStart + diff.start;
        edits.push({ start, end: start + diff.removed, text: diff.inserted.replace(/￼/g, "") });
      }
    });
    this.runAction(() => {
      let changed = false;
      // Last first, so earlier locations stay valid.
      for (const edit of edits.reverse()) {
        changed = this.core.replaceRange(edit.start, edit.end, edit.text, "type") || changed;
      }
      model.lines.forEach((_, i) => this.renderer.rerenderLine(i));
      return changed;
    });
  }

  // ---- IME ----

  private onCompositionStart(e: CompositionEvent): void {
    if (!this.isEditable || e.target !== this.element) {
      return;
    }
    this.syncFromDom();
    const range = this.core.range();
    const model = this.core.model();
    if (range === null || model.lines.length === 0) {
      return;
    }
    const startLine = lineIndexAt(model, range.start);
    if (lineIndexAt(model, range.end) !== startLine) {
      // A selection over several lines: delete it first, so that the
      // composition stays within one line.
      this.runAction(() => this.core.deleteSelection());
    }
    const m = this.core.model();
    const index = lineIndexAt(m, this.core.range()?.start ?? range.start);
    const line = m.lines[index]!;
    const el = this.renderer.lineElement(index);
    if (el === null) {
      return;
    }
    this.core.autolinkPaused = true;
    this.composition = {
      el,
      base: lineText(line),
      contentStart: line.contentStart,
      lineStart: line.start,
      ops: [],
    };
    this.emitState();
  }

  private onCompositionEnd(e: CompositionEvent): void {
    if (this.composition === null) {
      return;
    }
    this.finishComposition(e.data);
  }

  /** Applies what was composed to the document and re-renders the line. */
  private finishComposition(data: string | null): void {
    const comp = this.composition;
    if (comp === null) {
      return;
    }
    this.composition = null;
    this.core.autolinkPaused = false;
    const index = this.renderer.lineIndexOfElement(comp.el);
    if (!this.renderer.structureIntact() || index < 0) {
      // The browser broke the line structure; rebuild from the model and
      // insert what was composed at the selection of the composition start.
      this.renderer.restoreStructure();
      this.core.model().lines.forEach((_, i) => this.renderer.rerenderLine(i));
      this.runAction(() => (data === null || data === "" ? false : this.core.insertText(data)));
      return;
    }
    const dom = this.renderer.domLineText(index);
    let caret: number | undefined;
    const domSel = this.element.ownerDocument.getSelection();
    if (domSel?.focusNode != null && comp.el.contains(domSel.focusNode)) {
      const loc = this.renderer.locationOf(domSel.focusNode, domSel.focusOffset);
      const line = this.core.model().lines[index];
      if (loc !== null && line !== undefined) {
        caret = loc.location - line.contentStart;
      }
    }
    const diff = dom === null ? null : diffText(comp.base, dom.text, caret);
    this.runAction(() => {
      let changed = false;
      if (diff !== null) {
        let start = comp.contentStart + diff.start;
        let end = start + diff.removed;
        for (const op of comp.ops) {
          if (start === end) {
            start = end = transformLocation(start, op, "after");
          } else {
            start = transformLocation(start, op, "after");
            end = Math.max(start, transformLocation(end, op, "before"));
          }
        }
        changed = this.core.replaceRange(start, end, diff.inserted.replace(/￼/g, ""), "type");
      }
      this.render();
      const now = this.renderer.lineIndexOfElement(comp.el);
      if (now >= 0) {
        this.renderer.rerenderLine(now);
      }
      return changed;
    });
  }

  // ---- keys ----

  private onKeyDown(e: KeyboardEvent): void {
    if (e.target !== this.element) {
      return;
    }
    if (this.options.onKeyDown?.(e) === true) {
      return;
    }
    if (!this.isEditable || e.isComposing || e.keyCode === 229) {
      return;
    }
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    const run = (command: EditorCommand): void => {
      e.preventDefault();
      this.syncFromDom();
      this.runAction(() => this.core.exec(command));
    };
    if (key === "Tab" && !mod && !e.altKey) {
      this.syncFromDom();
      if (!this.core.tabIndents(e.shiftKey ? -1 : 1)) {
        return;
      }
      e.preventDefault();
      this.runAction(() => this.core.indent(e.shiftKey ? -1 : 1));
      return;
    }
    if (!mod) {
      return;
    }
    if (!e.altKey && !e.shiftKey) {
      switch (key) {
        case "b":
          return run({ type: "toggleStyle", style: "bold" });
        case "i":
          return run({ type: "toggleStyle", style: "italic" });
        case "u":
          return run({ type: "toggleStyle", style: "underline" });
        case "z":
          return run({ type: "undo" });
        case "y":
          return run({ type: "redo" });
        case "\\":
          return run({ type: "clearFormatting" });
        default:
          return;
      }
    }
    if (e.shiftKey && !e.altKey) {
      if (key === "z") {
        return run({ type: "redo" });
      }
      if (key === "v") {
        this.plainPaste = true;
        return;
      }
      if (e.code === "Digit7") {
        return run({ type: "toggleList", list: "numbered" });
      }
      if (e.code === "Digit8") {
        return run({ type: "toggleList", list: "bullet" });
      }
      if (e.code === "Digit5" || key === "x") {
        return run({ type: "toggleStyle", style: "strikethrough" });
      }
      return;
    }
    if (e.altKey && !e.shiftKey) {
      const digit = /^Digit([0-4])$/.exec(e.code);
      if (digit !== null) {
        return run({ type: "setHeading", level: Number(digit[1]) as 0 | 1 | 2 | 3 | 4 });
      }
    }
  }

  // ---- clipboard ----

  private onCopy(e: ClipboardEvent, cut: boolean): void {
    const data = e.clipboardData;
    if (data === null || this.composition !== null) {
      return;
    }
    const sel = this.readDomSelection();
    if (sel === null || sel.anchor === sel.focus) {
      return;
    }
    const start = Math.min(sel.anchor, sel.focus);
    const end = Math.max(sel.anchor, sel.focus);
    const blocks = this.core.blocksOf(start, end);
    data.setData("text/plain", blocksToText(blocks));
    data.setData("text/html", blocksToHtml(blocks, this.element.ownerDocument));
    e.preventDefault();
    if (cut && this.isEditable) {
      this.core.setSelection(sel);
      this.runAction(() => this.core.deleteSelection());
    }
  }

  private onPaste(e: ClipboardEvent): void {
    const plainOnly = this.plainPaste;
    this.plainPaste = false;
    if (!this.ownsTarget(e.target)) {
      return;
    }
    e.preventDefault();
    if (!this.isEditable || this.composition !== null) {
      return;
    }
    const data = e.clipboardData;
    if (data === null) {
      return;
    }
    this.syncFromDom();
    const html = plainOnly ? "" : data.getData("text/html");
    const text = data.getData("text/plain");
    if (html !== "") {
      const blocks = parseHtml(html);
      if (blocks.length > 0 && !isPlain(blocks)) {
        this.runAction(() => this.core.insertBlocks(blocks, true));
        return;
      }
      if (text === "" && blocks.length > 0) {
        this.runAction(() => this.core.insertText(blocksToText(blocks)));
        return;
      }
    }
    if (text !== "") {
      this.runAction(() => this.core.insertText(text));
    }
  }

  // ---- state ----

  private emitState(): void {
    if (this.statePending || this.destroyed) {
      return;
    }
    this.statePending = true;
    queueMicrotask(() => {
      this.statePending = false;
      if (this.destroyed || this.stateListeners.size === 0) {
        return;
      }
      const state = this.state();
      for (const listener of [...this.stateListeners]) {
        listener(state);
      }
    });
  }
}

export function createEditorView(options: BlipViewOptions): BlipView {
  return new EditorView(options);
}
