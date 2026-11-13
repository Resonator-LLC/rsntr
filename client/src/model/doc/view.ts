// A read-only element tree over a document.
//
// TypeScript port of the Rust crate `resonator-model` (module `doc::view`),
// itself derived from Apache Wave (`DocHelper`, `Point`, `DocIterate`),
// licensed under the Apache License, Version 2.0.

import { Attributes, type BootstrapDocument } from "../docop";
import { isHighSurrogate, isLowSurrogate } from "../docop/utf16";

declare const nodeIdBrand: unique symbol;

/**
 * A node of a {@link DocView}. Only meaningful for the view it came from:
 * any change to the document invalidates it.
 */
export type NodeId = number & { readonly [nodeIdBrand]: true };

type NodeData =
  | { readonly type: "element"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "text"; text: string };

interface Node {
  readonly data: NodeData;
  readonly parent: NodeId | null;
  readonly children: NodeId[];
  readonly indexInParent: number;
  /** Location just before the node (for the root: 0). */
  readonly location: number;
  /** Number of document items the node spans, tags included. */
  size: number;
}

/** A place between two items of a document (`Point`). */
export type Point =
  /** Inside a text node, `offset` UTF-16 code units from its start. */
  | { readonly type: "inText"; readonly node: NodeId; readonly offset: number }
  /**
   * Inside `container`, before `nodeAfter`, or at the end of the container
   * if `null`.
   */
  | { readonly type: "inElement"; readonly container: NodeId; readonly nodeAfter: NodeId | null };

/** Constructors and equality for {@link Point}s. */
export const Point = {
  inText: (node: NodeId, offset: number): Point => ({ type: "inText", node, offset }),
  inElement: (container: NodeId, nodeAfter: NodeId | null = null): Point => ({
    type: "inElement",
    container,
    nodeAfter,
  }),
  equals: (a: Point, b: Point): boolean =>
    a.type === "inText"
      ? b.type === "inText" && a.node === b.node && a.offset === b.offset
      : b.type === "inElement" && a.container === b.container && a.nodeAfter === b.nodeAfter,
} as const;

/** The tag name the original gives the implicit root element. */
export const ROOT_TAG = "doc";

const NO_CHILDREN: readonly NodeId[] = [];

/** Replaces unpaired surrogates by U+FFFD (Rust's `from_utf16_lossy`). */
function lossy(text: string): string {
  let result = "";
  let copied = 0;
  for (let i = 0; i < text.length; i++) {
    const unit = text.charCodeAt(i);
    if (isHighSurrogate(unit) && i + 1 < text.length && isLowSurrogate(text.charCodeAt(i + 1))) {
      i++;
    } else if (isHighSurrogate(unit) || isLowSurrogate(unit)) {
      result += text.slice(copied, i) + "�";
      copied = i + 1;
    }
  }
  return copied === 0 ? text : result + text.slice(copied);
}

/**
 * The element tree of a document at one moment, with the location of every
 * node. Locations count items as document operations do: one per UTF-16
 * code unit of text, one per element start and one per element end.
 *
 * The root node stands for the document itself (the original's implicit
 * `doc` element); it has no tags, so the inside of the root spans locations
 * `0..=size`.
 *
 * A view is a snapshot: it does not follow later changes of the document.
 */
export class DocView {
  private readonly nodes: Node[];

  /** Builds the tree of `doc`. */
  constructor(doc: BootstrapDocument) {
    const nodes: Node[] = [
      {
        data: { type: "element", tag: ROOT_TAG, attributes: Attributes.EMPTY },
        parent: null,
        children: [],
        indexInParent: 0,
        location: 0,
        size: 0,
      },
    ];
    let current = nodes[0]!;
    let currentId = 0;
    let location = 0;
    const add = (data: NodeData, size: number): number => {
      const id = nodes.length;
      const indexInParent = current.children.length;
      current.children.push(id as NodeId);
      nodes.push({
        data,
        parent: currentId as NodeId,
        children: [],
        indexInParent,
        location,
        size,
      });
      return id;
    };
    for (const c of doc.asOperation().components) {
      switch (c.type) {
        case "characters": {
          const length = c.characters.length;
          // Text split by annotation boundaries is one node.
          const lastId = current.children[current.children.length - 1];
          const last = lastId === undefined ? undefined : nodes[lastId];
          if (last !== undefined && last.data.type === "text") {
            last.data.text += c.characters;
            last.size += length;
          } else {
            add({ type: "text", text: c.characters }, length);
          }
          location += length;
          break;
        }
        case "elementStart":
          currentId = add({ type: "element", tag: c.tag, attributes: c.attributes }, 0);
          current = nodes[currentId]!;
          location += 1;
          break;
        case "elementEnd":
          location += 1;
          current.size = location - current.location;
          // A document is balanced, so there is always a parent.
          currentId = current.parent ?? 0;
          current = nodes[currentId]!;
          break;
        default:
          break;
      }
    }
    nodes[0]!.size = location;
    this.nodes = nodes;
  }

  private node(id: NodeId): Node | undefined {
    return this.nodes[id];
  }

  /** The node standing for the whole document. */
  root(): NodeId {
    return 0 as NodeId;
  }

  /** Number of items in the document. */
  size(): number {
    return this.nodes[0]!.size;
  }

  isElement(node: NodeId): boolean {
    return this.node(node)?.data.type === "element";
  }

  isText(node: NodeId): boolean {
    return this.node(node)?.data.type === "text";
  }

  /** The tag name of an element; `null` for text nodes. */
  tagName(node: NodeId): string | null {
    const data = this.node(node)?.data;
    return data !== undefined && data.type === "element" ? data.tag : null;
  }

  hasTag(node: NodeId, tag: string): boolean {
    return this.tagName(node) === tag;
  }

  /** The attributes of an element; `null` for text nodes. */
  attributes(node: NodeId): Attributes | null {
    const data = this.node(node)?.data;
    return data !== undefined && data.type === "element" ? data.attributes : null;
  }

  attribute(node: NodeId, name: string): string | null {
    return this.attributes(node)?.get(name) ?? null;
  }

  /** The data of a text node; `null` for elements. */
  text(node: NodeId): string | null {
    const data = this.node(node)?.data;
    return data !== undefined && data.type === "text" ? data.text : null;
  }

  parent(node: NodeId): NodeId | null {
    return this.node(node)?.parent ?? null;
  }

  children(node: NodeId): readonly NodeId[] {
    return this.node(node)?.children ?? NO_CHILDREN;
  }

  /** The element children of a node. */
  childElements(node: NodeId): NodeId[] {
    return this.children(node).filter((child) => this.isElement(child));
  }

  firstChild(node: NodeId): NodeId | null {
    return this.children(node)[0] ?? null;
  }

  lastChild(node: NodeId): NodeId | null {
    const children = this.children(node);
    return children[children.length - 1] ?? null;
  }

  nextSibling(node: NodeId): NodeId | null {
    const n = this.node(node);
    if (n === undefined || n.parent === null) {
      return null;
    }
    return this.children(n.parent)[n.indexInParent + 1] ?? null;
  }

  previousSibling(node: NodeId): NodeId | null {
    const n = this.node(node);
    if (n === undefined || n.parent === null || n.indexInParent === 0) {
      return null;
    }
    return this.children(n.parent)[n.indexInParent - 1] ?? null;
  }

  /** `DocHelper.getFirstChildElement`. */
  firstChildElement(node: NodeId): NodeId | null {
    for (const child of this.children(node)) {
      if (this.isElement(child)) {
        return child;
      }
    }
    return null;
  }

  /** `DocHelper.getNextSiblingElement`. */
  nextSiblingElement(node: NodeId): NodeId | null {
    let next = this.nextSibling(node);
    while (next !== null) {
      if (this.isElement(next)) {
        return next;
      }
      next = this.nextSibling(next);
    }
    return null;
  }

  /**
   * The node itself if an element, else its parent
   * (`Point.enclosingElement`).
   */
  enclosingElement(node: NodeId): NodeId {
    return this.isElement(node) ? node : (this.parent(node) ?? this.root());
  }

  /** Location just before a node (`Locator.before`); 0 for the root. */
  location(node: NodeId): number {
    return this.node(node)?.location ?? 0;
  }

  /** Location just after a node (`Locator.after`). */
  locationAfter(node: NodeId): number {
    const n = this.node(node);
    return n === undefined ? 0 : n.location + n.size;
  }

  /** Location of the start of an element's content (`Locator.start`). */
  contentStart(element: NodeId): number {
    return element === this.root() ? 0 : this.location(element) + 1;
  }

  /** Location of the end of an element's content (`Locator.end`). */
  contentEnd(element: NodeId): number {
    return element === this.root() ? this.size() : Math.max(this.locationAfter(element) - 1, 0);
  }

  /** Number of items a node spans, including its tags. */
  itemSize(node: NodeId): number {
    return this.node(node)?.size ?? 0;
  }

  /**
   * The location of a point, or `null` if the point is not valid for this
   * view.
   */
  pointLocation(point: Point): number | null {
    if (point.type === "inText") {
      const n = this.node(point.node);
      if (n === undefined || n.data.type !== "text") {
        return null;
      }
      const offset = point.offset;
      return Number.isInteger(offset) && offset >= 0 && offset <= n.size
        ? n.location + offset
        : null;
    }
    if (!this.isElement(point.container)) {
      return null;
    }
    if (point.nodeAfter === null) {
      return this.contentEnd(point.container);
    }
    return this.parent(point.nodeAfter) === point.container
      ? this.location(point.nodeAfter)
      : null;
  }

  /**
   * The point at a location: inside a text node if the location is strictly
   * inside one, otherwise between nodes. `null` if the location is beyond
   * the document.
   */
  locate(location: number): Point | null {
    if (!Number.isInteger(location) || location < 0 || location > this.size()) {
      return null;
    }
    let container = this.root();
    for (;;) {
      let descended = false;
      for (const child of this.children(container)) {
        const start = this.location(child);
        if (location <= start) {
          return { type: "inElement", container, nodeAfter: child };
        }
        if (location < this.locationAfter(child)) {
          if (this.isText(child)) {
            return { type: "inText", node: child, offset: location - start };
          }
          container = child;
          descended = true;
          break;
        }
      }
      if (!descended) {
        return { type: "inElement", container, nodeAfter: null };
      }
    }
  }

  /** The element whose content directly holds a location. */
  elementAt(location: number): NodeId | null {
    const point = this.locate(location);
    if (point === null) {
      return null;
    }
    return point.type === "inText" ? this.enclosingElement(point.node) : point.container;
  }

  /** The node starting exactly at a location, if any. */
  nodeStartingAt(location: number): NodeId | null {
    const point = this.locate(location);
    if (point === null || point.type === "inText" || point.nodeAfter === null) {
      return null;
    }
    return this.location(point.nodeAfter) === location ? point.nodeAfter : null;
  }

  /** The element starting exactly at a location, if any. */
  elementStartingAt(location: number): NodeId | null {
    const node = this.nodeStartingAt(location);
    return node !== null && this.isElement(node) ? node : null;
  }

  /** All nodes below `root` in document order (`DocIterate.deep`). */
  descendants(root: NodeId): NodeId[] {
    const result: NodeId[] = [];
    const stack: NodeId[] = [...this.children(root)].reverse();
    for (let node = stack.pop(); node !== undefined; node = stack.pop()) {
      result.push(node);
      const children = this.children(node);
      for (let i = children.length - 1; i >= 0; i--) {
        stack.push(children[i]!);
      }
    }
    return result;
  }

  /**
   * All elements below `root` in document order
   * (`DocIterate.deepElements`).
   */
  descendantElements(root: NodeId): NodeId[] {
    return this.descendants(root).filter((node) => this.isElement(node));
  }

  /**
   * The first element below `root` (default: the whole document), in
   * document order, with a tag name (`DocHelper.getElementWithTagName`).
   */
  elementWithTagName(tag: string, root: NodeId = this.root()): NodeId | null {
    return this.descendants(root).find((node) => this.hasTag(node, tag)) ?? null;
  }

  /**
   * The text at locations `start..end`, skipping element tags
   * (`DocHelper.getText`). Out-of-range bounds are clamped. A surrogate
   * pair cut by a bound yields U+FFFD.
   */
  textBetween(start: number, end: number): string {
    let result = "";
    for (const node of this.descendants(this.root())) {
      const text = this.text(node);
      if (text === null) {
        continue;
      }
      const location = this.location(node);
      if (location >= end) {
        break;
      }
      result += text.slice(Math.max(start - location, 0), Math.max(end - location, 0));
    }
    return lossy(result);
  }

  /** All text below a node (`DocHelper.getText` of an element). */
  textContent(node: NodeId): string {
    const own = this.text(node);
    if (own !== null) {
      return own;
    }
    let result = "";
    for (const n of this.descendants(node)) {
      result += this.text(n) ?? "";
    }
    return result;
  }
}
