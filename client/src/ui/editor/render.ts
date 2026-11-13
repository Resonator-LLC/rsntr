// The DOM projection of a document model, and the mapping between document
// locations and DOM positions.
//
// Structure (all of it built with DOM APIs; text only through text nodes):
//
//   root.doc
//     div.doc-line            one per line, in order; nothing else
//       span.doc-run | a.doc-run.doc-link      one text node each
//       span.doc-anchor / span.doc-image / span.doc-chip   atoms,
//                                              contenteditable=false
//       br.doc-filler         if the line is empty or ends with an atom
//
// Location → DOM uses the segments recorded when a line was rendered.
// DOM → location walks whatever is in the line element now, counting text
// and the known atoms, so it also works on a line the browser has edited
// (IME composition), where runs may have been split or text nodes added.

import type { AnchorHost, AttachmentInfo, AttachmentSource } from "./api";
import { type AtomItem, type DocModel, type Item, type Line, MENTION_USER, lineIndexAt } from "./model";
import { openLightbox } from "./lightbox";
import { applyRunStyle, hrefOf, linkOf } from "./styles";

export interface RenderEnv {
  readonly anchors?: AnchorHost | undefined;
  readonly attachments?: AttachmentSource | undefined;
  readonly waveLinkHref: (value: string) => string;
}

interface Seg {
  readonly item: Item;
  /** The run element or the atom element; a child of the line element. */
  readonly node: HTMLElement;
  /** The text node of a run. */
  readonly text: Text | null;
  /** Identity for reuse: style key of a run, kind and content of an atom. */
  readonly key: string;
}

interface LineView {
  readonly el: HTMLElement;
  line: Line;
  segs: Seg[];
}

export interface DomPoint {
  readonly node: Node;
  readonly offset: number;
}

/** Size in items of each atom element. */
const atomSizes = new WeakMap<Node, number>();

/** How long to wait before asking again for an attachment whose server was unreachable. */
const UNAVAILABLE_RETRY_MS = 30_000;

export function isAtomElement(node: Node): boolean {
  return atomSizes.has(node);
}

const INDENT_UNIT_PX = 22; // ParagraphRenderingConstants.INDENT_UNIT_SIZE_PX
const BULLETS = ["\u2022", "\u25E6", "\u25AA"]; // disc, circle, square

function atomKey(item: AtomItem): string {
  switch (item.kind) {
    case "reply":
      return `r\u0001${item.id}`;
    case "image":
      return `i\u0001${item.attachment}\u0001${item.full ? 1 : 0}\u0001${item.caption}`;
    case "chip":
      return `c\u0001${item.tag}\u0001${item.label}`;
  }
}

export function formatBytes(size: number): string {
  if (!(size >= 0)) {
    return "";
  }
  if (size < 1024) {
    return `${size} B`;
  }
  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(size < 10240 ? 1 : 0)} KB`;
  }
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

export class Renderer {
  readonly root: HTMLElement;
  private readonly env: RenderEnv;
  private readonly doc: Document;
  private views: LineView[] = [];
  private current: DocModel = { bodyStart: null, bodyEnd: 0, lines: [], size: 0 };
  /** Anchor containers by thread id; the same node for the anchor's life. */
  private readonly anchorEls = new Map<string, HTMLElement>();
  private destroyed = false;

  constructor(root: HTMLElement, env: RenderEnv) {
    this.root = root;
    this.env = env;
    this.doc = root.ownerDocument;
  }

  model(): DocModel {
    return this.current;
  }

  lineElement(index: number): HTMLElement | null {
    return this.views[index]?.el ?? null;
  }

  lineIndexOfElement(el: Node): number {
    return this.views.findIndex((v) => v.el === el);
  }

  /**
   * Makes the DOM show `model`, touching only the lines that changed.
   * `frozen` names a line element that must be left alone (it is being
   * composed in) and the index of its line in the new model.
   */
  render(model: DocModel, frozen?: { el: HTMLElement; index: number }): void {
    const oldViews = this.views;
    const newLines = model.lines;
    let result: LineView[];
    const frozenOld = frozen === undefined ? -1 : oldViews.findIndex((v) => v.el === frozen.el);
    if (frozen !== undefined && frozenOld >= 0 && frozen.index >= 0 && frozen.index < newLines.length) {
      const view = oldViews[frozenOld]!;
      const before = this.patch(oldViews.slice(0, frozenOld), newLines.slice(0, frozen.index), view.el);
      const after = this.patch(oldViews.slice(frozenOld + 1), newLines.slice(frozen.index + 1), null);
      // The frozen line keeps its DOM; only its bookkeeping moves on.
      view.line = newLines[frozen.index]!;
      result = [...before, view, ...after];
    } else {
      result = this.patch(oldViews, newLines, null);
    }
    this.views = result;
    this.current = model;
    this.sweepAnchors();
  }

  /** Re-renders one line from the model whatever its DOM holds now. */
  rerenderLine(index: number): void {
    const view = this.views[index];
    if (view !== undefined) {
      this.fillLine(view, view.line, true);
      this.sweepAnchors();
    }
  }

  /** Whether the DOM has exactly the line elements the renderer made. */
  structureIntact(): boolean {
    const children = this.root.childNodes;
    if (children.length !== this.views.length) {
      return false;
    }
    for (let i = 0; i < children.length; i++) {
      if (children[i] !== this.views[i]!.el) {
        return false;
      }
    }
    return true;
  }

  /** Puts back the line elements, in order, and drops anything else. */
  restoreStructure(): void {
    const wanted = this.views.map((v) => v.el);
    syncChildren(this.root, wanted);
  }

  destroy(): void {
    this.destroyed = true;
    for (const [id, el] of this.anchorEls) {
      this.env.anchors?.detach(id, el);
    }
    this.anchorEls.clear();
    this.views = [];
    this.root.replaceChildren();
  }

  // ---- patching ----

  private patch(oldViews: LineView[], newLines: readonly Line[], before: Node | null): LineView[] {
    const oldLen = oldViews.length;
    const newLen = newLines.length;
    let prefix = 0;
    while (prefix < oldLen && prefix < newLen && oldViews[prefix]!.line.sig === newLines[prefix]!.sig) {
      prefix++;
    }
    let suffix = 0;
    while (
      suffix < oldLen - prefix &&
      suffix < newLen - prefix &&
      oldViews[oldLen - 1 - suffix]!.line.sig === newLines[newLen - 1 - suffix]!.sig
    ) {
      suffix++;
    }
    const result: LineView[] = [];
    for (let i = 0; i < prefix; i++) {
      const view = oldViews[i]!;
      view.line = newLines[i]!;
      result.push(view);
    }
    const oldMidEnd = oldLen - suffix;
    const newMidEnd = newLen - suffix;
    const anchorNode: Node | null = suffix > 0 ? oldViews[oldMidEnd]!.el : before;
    let o = prefix;
    for (let n = prefix; n < newMidEnd; n++) {
      const line = newLines[n]!;
      if (o < oldMidEnd) {
        const view = oldViews[o++]!;
        this.fillLine(view, line, false);
        result.push(view);
      } else {
        const view: LineView = { el: this.doc.createElement("div"), line, segs: [] };
        this.fillLine(view, line, true);
        this.root.insertBefore(view.el, anchorNode);
        result.push(view);
      }
    }
    for (; o < oldMidEnd; o++) {
      oldViews[o]!.el.remove();
    }
    for (let i = 0; i < suffix; i++) {
      const view = oldViews[oldMidEnd + i]!;
      view.line = newLines[newMidEnd + i]!;
      result.push(view);
    }
    return result;
  }

  private sweepAnchors(): void {
    if (this.anchorEls.size === 0) {
      return;
    }
    const live = new Set<string>();
    for (const view of this.views) {
      for (const item of view.line.items) {
        if (item.kind === "reply") {
          live.add(item.id);
        }
      }
    }
    for (const [id, el] of this.anchorEls) {
      if (!live.has(id)) {
        this.anchorEls.delete(id);
        this.env.anchors?.detach(id, el);
        el.remove();
      }
    }
  }

  private setLineAttributes(el: HTMLElement, line: Line): void {
    let cls = "doc-line";
    if (line.type === "li") {
      cls += line.decimal ? " doc-li doc-li-num" : " doc-li doc-li-bullet";
    } else if (line.type !== "") {
      cls += ` doc-${line.type}`;
    }
    if (line.align !== "" && "lcrj".includes(line.align)) {
      cls += ` doc-align-${line.align}`;
    }
    if (el.className !== cls) {
      el.className = cls;
    }
    const dir = line.dir === "r" ? "rtl" : line.dir === "l" ? "ltr" : "auto";
    if (el.getAttribute("dir") !== dir) {
      el.setAttribute("dir", dir);
    }
    let marker: string | null = null;
    if (line.type === "li") {
      marker = line.decimal ? `${line.number}.` : BULLETS[line.indent % 3]!;
    }
    if (marker === null) {
      el.removeAttribute("data-marker");
    } else if (el.getAttribute("data-marker") !== marker) {
      el.setAttribute("data-marker", marker);
    }
    if (line.type.startsWith("h")) {
      el.setAttribute("role", "heading");
      el.setAttribute("aria-level", line.type.slice(1));
    } else {
      el.removeAttribute("role");
      el.removeAttribute("aria-level");
    }
    // The original indents list items one unit more (`DefaultParagraphHtmlRenderer`).
    const units = line.indent + (line.type === "li" ? 1 : 0);
    const margin = units > 0 ? `${units * INDENT_UNIT_PX}px` : "";
    if (el.style.marginInlineStart !== margin) {
      el.style.marginInlineStart = margin;
    }
  }

  /** Makes a line element show a line; `force` distrusts the old segments. */
  private fillLine(view: LineView, line: Line, force: boolean): void {
    const el = view.el;
    this.setLineAttributes(el, line);
    const old = force ? [] : view.segs;
    const oldAtoms = new Map<string, Seg[]>();
    for (const seg of view.segs) {
      if (seg.item.kind !== "text" && seg.item.kind !== "reply") {
        const list = oldAtoms.get(seg.key);
        if (list === undefined) {
          oldAtoms.set(seg.key, [seg]);
        } else {
          list.push(seg);
        }
      }
    }
    const segs: Seg[] = [];
    const seenReplies = new Set<string>();
    line.items.forEach((item, i) => {
      if (item.kind === "text") {
        const prev = old[i];
        if (prev !== undefined && prev.text !== null && prev.key === item.key && prev.node.parentNode === el) {
          if (prev.text.data !== item.text) {
            prev.text.data = item.text;
          }
          if (prev.node.firstChild !== prev.text || prev.node.childNodes.length !== 1) {
            prev.node.replaceChildren(prev.text);
          }
          segs.push({ item, node: prev.node, text: prev.text, key: item.key });
        } else {
          segs.push(this.createRun(item));
        }
        return;
      }
      const key = atomKey(item);
      if (item.kind === "reply") {
        const duplicate = seenReplies.has(item.id) || item.id === "";
        seenReplies.add(item.id);
        segs.push({ item, node: duplicate ? this.createChip("reply", "") : this.anchorFor(item.id, item.size), text: null, key });
        return;
      }
      const reused = oldAtoms.get(key)?.shift();
      if (reused !== undefined) {
        atomSizes.set(reused.node, item.size);
        segs.push({ item, node: reused.node, text: null, key });
        return;
      }
      const node = item.kind === "image" ? this.createImage(item) : this.createChip(item.tag, item.label);
      atomSizes.set(node, item.size);
      segs.push({ item, node, text: null, key });
    });
    const wanted: Node[] = segs.map((s) => s.node);
    const last = segs[segs.length - 1];
    if (last === undefined || last.text === null) {
      let filler: Node | null = null;
      for (const child of Array.from(el.childNodes)) {
        if (child.nodeName === "BR" && (child as HTMLElement).className === "doc-filler") {
          filler = child;
        }
      }
      if (filler === null) {
        const br = this.doc.createElement("br");
        br.className = "doc-filler";
        filler = br;
      }
      wanted.push(filler);
    }
    syncChildren(el, wanted);
    view.line = line;
    view.segs = segs;
    // Attach anchors only once they are in the document.
    for (const seg of segs) {
      if (seg.item.kind === "reply" && pendingAttach.has(seg.node)) {
        pendingAttach.delete(seg.node);
        if (!this.destroyed) {
          this.env.anchors?.attach(seg.item.id, seg.node);
        }
      }
    }
  }

  private createRun(item: Item & { kind: "text" }): Seg {
    const link = linkOf(item.annotations);
    const href = link === null ? null : hrefOf(link, this.env.waveLinkHref);
    let node: HTMLElement;
    if (href !== null) {
      const a = this.doc.createElement("a");
      a.className = "doc-run doc-link";
      a.setAttribute("href", href.href);
      a.setAttribute("rel", "noopener noreferrer");
      if (href.external) {
        a.setAttribute("target", "_blank");
      }
      node = a;
    } else {
      node = this.doc.createElement("span");
      node.className = "doc-run";
    }
    if (item.key !== "") {
      applyRunStyle(node, item.annotations);
    }
    const mention = item.annotations[MENTION_USER];
    if (mention !== undefined) {
      node.classList.add("doc-mention");
      node.setAttribute("data-mention", mention);
      if (href === null) {
        node.setAttribute("title", mention);
      }
    }
    const text = this.doc.createTextNode(item.text);
    node.appendChild(text);
    return { item, node, text, key: item.key };
  }

  private anchorFor(id: string, size: number): HTMLElement {
    let el = this.anchorEls.get(id);
    if (el === undefined) {
      el = this.doc.createElement("span");
      el.className = "doc-anchor";
      el.setAttribute("contenteditable", "false");
      el.setAttribute("data-thread", id);
      this.anchorEls.set(id, el);
      pendingAttach.add(el);
    }
    atomSizes.set(el, size);
    return el;
  }

  private createChip(tag: string, label: string): HTMLElement {
    const el = this.doc.createElement("span");
    el.className = "doc-chip";
    el.setAttribute("contenteditable", "false");
    el.setAttribute("data-tag", tag);
    el.textContent = label === "" ? tag : `${tag}: ${label}`;
    atomSizes.set(el, 2);
    return el;
  }

  private createImage(item: Item & { kind: "image" }): HTMLElement {
    const el = this.doc.createElement("span");
    el.className = item.full ? "doc-image doc-image-full" : "doc-image";
    el.setAttribute("contenteditable", "false");
    const source = this.env.attachments;
    const caption = this.doc.createElement("span");
    caption.className = "doc-image-caption";
    caption.textContent = item.caption;
    if (source === undefined || item.attachment === "") {
      el.classList.add("doc-image-pending");
      el.appendChild(caption);
      return el;
    }
    const link = this.doc.createElement("a");
    link.className = "doc-image-link";
    link.setAttribute("href", source.attachmentUrl(item.attachment));
    link.setAttribute("target", "_blank");
    link.setAttribute("rel", "noopener noreferrer");
    link.setAttribute("draggable", "false");
    const meta = this.doc.createElement("span");
    meta.className = "doc-image-meta";
    link.appendChild(caption);
    el.append(link, meta);
    el.classList.add("doc-image-pending");
    const show = (info: AttachmentInfo | null): void => {
      if (this.destroyed) {
        return;
      }
      el.classList.remove("doc-image-pending");
      if (info === null) {
        el.classList.add("doc-image-unknown");
        return;
      }
      if (info.unavailableFrom !== undefined) {
        // Another server's attachment, its server unreachable: a
        // placeholder, and another try later.
        el.classList.add("doc-image-unavailable");
        meta.textContent = `unavailable from ${info.unavailableFrom}`;
        const retry = setTimeout(() => {
          if (!this.destroyed) {
            source.info(item.attachment).then(show, () => show(null));
          }
        }, UNAVAILABLE_RETRY_MS);
        (retry as { unref?: () => void }).unref?.();
        return;
      }
      el.classList.remove("doc-image-unavailable");
      if (info.fileName !== "" && item.caption === "") {
        caption.textContent = info.fileName;
      }
      link.setAttribute("title", info.fileName);
      meta.textContent = formatBytes(info.size);
      if (info.mimeType.startsWith("image/") || info.thumbnail !== undefined) {
        const img = this.doc.createElement("img");
        img.className = "doc-image-thumb";
        img.setAttribute("alt", item.caption === "" ? info.fileName : item.caption);
        img.setAttribute("loading", "lazy");
        img.setAttribute("draggable", "false");
        const dims = item.full ? (info.image ?? info.thumbnail) : (info.thumbnail ?? undefined);
        if (dims !== undefined && dims.width > 0 && dims.height > 0) {
          img.setAttribute("width", String(dims.width));
          img.setAttribute("height", String(dims.height));
        }
        img.setAttribute(
          "src",
          item.full ? source.attachmentUrl(item.attachment) : source.thumbnailUrl(item.attachment),
        );
        link.insertBefore(img, caption);
        // A plain click shows the image in the viewer, over the page; with
        // a modifier (new tab, save) the browser does what it usually does.
        link.addEventListener("click", (event) => {
          if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
            return;
          }
          event.preventDefault();
          openLightbox(this.doc, {
            src: source.attachmentUrl(item.attachment),
            placeholder: img.currentSrc !== "" ? img.currentSrc : img.src,
            alt: img.alt,
            fileName: info.fileName !== "" ? info.fileName : item.caption,
            meta: formatBytes(info.size),
            downloadHref: source.attachmentUrl(item.attachment),
          });
        });
      } else {
        el.classList.add("doc-file");
      }
    };
    source.info(item.attachment).then(show, () => show(null));
    return el;
  }

  // ---- positions ----

  /** The DOM position of a document location (clamped into the lines). */
  domAt(location: number): DomPoint | null {
    const model = this.current;
    if (this.views.length === 0) {
      return null;
    }
    const index = lineIndexAt(model, location);
    const view = this.views[index]!;
    const line = view.line;
    const offset = Math.min(Math.max(location - line.contentStart, 0), line.end - line.contentStart);
    const segs = view.segs;
    for (let i = 0; i < segs.length; i++) {
      const seg = segs[i]!;
      const start = seg.item.offset;
      const end = start + seg.item.size;
      if (offset > end) {
        continue;
      }
      if (seg.text !== null) {
        // At a boundary between two runs this is the end of the left one.
        return { node: seg.text, offset: Math.min(offset - start, seg.text.data.length) };
      }
      if (offset <= start) {
        return { node: view.el, offset: childIndex(seg.node) };
      }
      // After (or inside) an atom: the start of a following run, else
      // the gap after the atom.
      const next = segs[i + 1];
      if (next !== undefined && next.text !== null) {
        return { node: next.text, offset: 0 };
      }
      return { node: view.el, offset: childIndex(seg.node) + 1 };
    }
    return { node: view.el, offset: 0 };
  }

  /**
   * The document location of a DOM position inside the root. Null if the
   * position is not in a line of this rendering. `inAnchor` is set if the
   * position is inside an inline reply container (another view's ground).
   */
  locationOf(node: Node, offset: number): { location: number; inAnchor: boolean } | null {
    if (node === this.root) {
      const view = this.views[offset];
      if (view !== undefined) {
        return { location: view.line.contentStart, inAnchor: false };
      }
      const lastView = this.views[this.views.length - 1];
      return lastView === undefined ? null : { location: lastView.line.end, inAnchor: false };
    }
    let lineEl: Node | null = node;
    while (lineEl !== null && lineEl.parentNode !== this.root) {
      lineEl = lineEl.parentNode;
    }
    if (lineEl === null) {
      return null;
    }
    const index = this.lineIndexOfElement(lineEl);
    if (index < 0) {
      return null;
    }
    const view = this.views[index]!;
    const r = offsetInLine(view.el, node, offset);
    const size = view.line.end - view.line.contentStart;
    return { location: view.line.contentStart + Math.min(r.offset, size), inAnchor: r.inAnchor };
  }

  /** The content of a line as the DOM has it now: text with U+FFFC per atom. */
  domLineText(index: number): { text: string; atoms: number } | null {
    const view = this.views[index];
    if (view === undefined) {
      return null;
    }
    let text = "";
    let atoms = 0;
    const walk = (parent: Node): void => {
      for (let child = parent.firstChild; child !== null; child = child.nextSibling) {
        if (isAtomElement(child)) {
          text += "\uFFFC";
          atoms++;
        } else if (child.nodeType === 3) {
          text += (child as Text).data;
        } else if (child.nodeType === 1) {
          walk(child);
        }
      }
    };
    walk(view.el);
    return { text, atoms };
  }
}

const pendingAttach = new WeakSet<Node>();

function childIndex(node: Node): number {
  let i = 0;
  for (let n = node.previousSibling; n !== null; n = n.previousSibling) {
    i++;
  }
  return i;
}

/**
 * Makes `parent`'s children exactly `wanted`, without ever detaching a
 * node that stays (an anchor container may hold a focused editor).
 */
function syncChildren(parent: Node, wanted: readonly Node[]): void {
  const keep = new Set(wanted);
  for (const child of Array.from(parent.childNodes)) {
    if (!keep.has(child)) {
      parent.removeChild(child);
    }
  }
  for (let i = 0; i < wanted.length; i++) {
    const at = parent.childNodes[i] ?? null;
    if (at !== wanted[i]) {
      parent.insertBefore(wanted[i]!, at);
    }
  }
}

/** Items before a DOM position within a line element. */
function offsetInLine(lineEl: Node, node: Node, offset: number): { offset: number; inAnchor: boolean } {
  let total = 0;
  let inAnchor = false;
  // Returns true when the position was reached.
  const walk = (parent: Node): boolean => {
    if (parent === node && parent.nodeType !== 3) {
      let i = 0;
      for (let child = parent.firstChild; child !== null; child = child.nextSibling, i++) {
        if (i >= offset) {
          return true;
        }
        total += sizeOf(child);
      }
      return true;
    }
    for (let child = parent.firstChild; child !== null; child = child.nextSibling) {
      if (isAtomElement(child)) {
        if (child === node || child.contains(node)) {
          // Inside an atom counts as after it, except at its very start.
          const atStart = child === node && offset === 0;
          if (!atStart) {
            total += atomSizes.get(child) ?? 0;
          }
          inAnchor = child !== node && (child as HTMLElement).classList.contains("doc-anchor");
          return true;
        }
        total += atomSizes.get(child) ?? 0;
      } else if (child.nodeType === 3) {
        const length = (child as Text).data.length;
        if (child === node) {
          total += Math.min(offset, length);
          return true;
        }
        total += length;
      } else if (child.nodeType === 1) {
        if (walk(child)) {
          return true;
        }
      }
    }
    return false;
  };
  walk(lineEl);
  return { offset: total, inAnchor };
}

function sizeOf(node: Node): number {
  if (isAtomElement(node)) {
    return atomSizes.get(node) ?? 0;
  }
  if (node.nodeType === 3) {
    return (node as Text).data.length;
  }
  let total = 0;
  for (let child = node.firstChild; child !== null; child = child.nextSibling) {
    total += sizeOf(child);
  }
  return total;
}
