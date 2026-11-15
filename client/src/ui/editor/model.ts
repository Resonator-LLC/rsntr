// The blip document as the editor sees it: a list of lines, each a sequence
// of styled text runs and atomic inline elements. Built from the document in
// one pass; everything the renderer and the input logic need to know about
// structure comes from here.

import type { BootstrapDocument } from "../../model/docop";

/** The key the original's toolbar writes; rendered and removed like a manual link. */
export const LINK_PLAIN = "link";
export const LINK_MANUAL = "link/manual";
export const LINK_AUTO = "link/auto";
export const LINK_WAVE = "link/wave";
/** All link keys (`Link.LINK_KEYS`). */
export const LINK_KEYS = [LINK_PLAIN, LINK_AUTO, LINK_MANUAL, LINK_WAVE] as const;

export const STYLE_PREFIX = "style/";
export const STYLE_FONT_WEIGHT = "style/fontWeight";
export const STYLE_FONT_STYLE = "style/fontStyle";
export const STYLE_TEXT_DECORATION = "style/textDecoration";
export const STYLE_COLOR = "style/color";
export const STYLE_BG_COLOR = "style/backgroundColor";
export const STYLE_FONT_FAMILY = "style/fontFamily";
export const STYLE_FONT_SIZE = "style/fontSize";
export const STYLE_VERTICAL_ALIGN = "style/verticalAlign";

/** The style annotation keys (`StyleAnnotationHandler`, `AnnotationConstants`). */
export const STYLE_KEYS = [
  STYLE_BG_COLOR,
  STYLE_COLOR,
  STYLE_FONT_FAMILY,
  STYLE_FONT_SIZE,
  STYLE_FONT_STYLE,
  STYLE_FONT_WEIGHT,
  STYLE_TEXT_DECORATION,
  STYLE_VERTICAL_ALIGN,
] as const;

/** An `@name` mention of a participant; the value is their address. */
export const MENTION_USER = "mention/user";

/**
 * Keys that mark a stretch of text as one thing (a link, a mention): typed
 * text never extends them at either end.
 */
export const BOUNDED_KEYS: readonly string[] = [...LINK_KEYS, MENTION_USER];

/** Every annotation key that changes how text is rendered. */
export const RENDER_KEYS: readonly string[] = [...STYLE_KEYS, ...LINK_KEYS, MENTION_USER];
const RENDER_KEY_SET = new Set(RENDER_KEYS);

/** The rendering-relevant annotations of a run of text: key → value. */
export type RunAnnotations = Readonly<Record<string, string>>;

export interface TextItem {
  readonly kind: "text";
  /** Offset from the line's content start, in items. */
  readonly offset: number;
  readonly size: number;
  readonly text: string;
  readonly annotations: RunAnnotations;
  /** Canonical string of `annotations`; equal keys render the same. */
  readonly key: string;
}

export interface ReplyItem {
  readonly kind: "reply";
  readonly offset: number;
  readonly size: number;
  readonly id: string;
}

export interface ImageItem {
  readonly kind: "image";
  readonly offset: number;
  readonly size: number;
  readonly attachment: string;
  readonly caption: string;
  readonly full: boolean;
}

/**
 * A gadget with a wave app (`<gadget app="sha256:..." bundle="..." id="...">`):
 * the embedder draws the app in its element.
 */
export interface AppItem {
  readonly kind: "app";
  readonly offset: number;
  readonly size: number;
  /** The gadget id. */
  readonly id: string;
  /** The app id, `sha256:<hex>`. */
  readonly app: string;
  /** The bundle's attachment id. */
  readonly bundle: string;
}

/** An element the editor does not understand (gadget, form element, ...). */
export interface ChipItem {
  readonly kind: "chip";
  readonly offset: number;
  readonly size: number;
  readonly tag: string;
  readonly label: string;
}

export type AtomItem = ReplyItem | ImageItem | AppItem | ChipItem;
export type Item = TextItem | AtomItem;

export type LineType = "" | "h1" | "h2" | "h3" | "h4" | "li";

export interface Line {
  /** Location of the `<line>` element; equals `contentStart` if implicit. */
  readonly start: number;
  /** Location of the first item of the line's content. */
  readonly contentStart: number;
  /** Location just after the line's content. */
  readonly end: number;
  /** False for content of a body that precedes its first `<line>`. */
  readonly hasElement: boolean;
  readonly type: LineType;
  readonly indent: number;
  /** `a` attribute: l, c, r, j, or "" if unset. */
  readonly align: string;
  /** `d` attribute: l, r, or "" if unset. */
  readonly dir: string;
  readonly decimal: boolean;
  readonly items: readonly Item[];
  /** The number of a numbered list item, else 0. Set by `numberLines`. */
  number: number;
  /** Everything that determines the line's rendering. */
  sig: string;
}

export interface DocModel {
  /** Location of the `<body>` start tag, or null if there is no body. */
  readonly bodyStart: number | null;
  /** Location of the end of the body's content. */
  readonly bodyEnd: number;
  readonly lines: readonly Line[];
  readonly size: number;
}

const EMPTY_ANNOTATIONS: RunAnnotations = Object.freeze({});
const LINE_TYPES = new Set(["h1", "h2", "h3", "h4", "li"]);

interface LineDraft {
  start: number;
  contentStart: number;
  hasElement: boolean;
  type: LineType;
  indent: number;
  align: string;
  dir: string;
  decimal: boolean;
  items: Item[];
}

function parseIndent(value: string | null): number {
  if (value === null || !/^\d{1,4}$/.test(value)) {
    return 0;
  }
  return Number.parseInt(value, 10);
}

/** Builds the line model of a document. */
export function parseDocument(doc: BootstrapDocument): DocModel {
  const lines: Line[] = [];
  let bodyStart: number | null = null;
  let bodyEnd = 0;
  let location = 0;
  // Element nesting: 0 = top level; inside body = 1.
  let depth = 0;
  let inBody = false;
  let bodyDone = false;
  let current: LineDraft | null = null;
  // The atom being read (an element directly in the body, not a line).
  let atom: { tag: string; start: number; depth: number; attrs: Map<string, string>; text: string } | null =
    null;
  let inLineElement = false;

  const annotations = new Map<string, string>();
  let runAnnotations: RunAnnotations = EMPTY_ANNOTATIONS;
  let runKey = "";
  const refreshRun = (): void => {
    if (annotations.size === 0) {
      runAnnotations = EMPTY_ANNOTATIONS;
      runKey = "";
      return;
    }
    const record: Record<string, string> = {};
    let key = "";
    for (const name of RENDER_KEYS) {
      const value = annotations.get(name);
      if (value !== undefined) {
        record[name] = value;
        key += `${name}\u0001${value}\u0002`;
      }
    }
    runAnnotations = key === "" ? EMPTY_ANNOTATIONS : record;
    runKey = key;
  };

  const finishLine = (end: number): void => {
    if (current === null) {
      return;
    }
    lines.push({
      start: current.start,
      contentStart: current.contentStart,
      end,
      hasElement: current.hasElement,
      type: current.type,
      indent: current.indent,
      align: current.align,
      dir: current.dir,
      decimal: current.decimal,
      items: current.items,
      number: 0,
      sig: "",
    });
    current = null;
  };
  const ensureLine = (): LineDraft => {
    if (current === null) {
      current = {
        start: location,
        contentStart: location,
        hasElement: false,
        type: "",
        indent: 0,
        align: "",
        dir: "",
        decimal: false,
        items: [],
      };
    }
    return current;
  };

  for (const c of doc.asOperation().components) {
    switch (c.type) {
      case "annotationBoundary": {
        let relevant = false;
        for (const key of c.boundary.endKeys) {
          if (RENDER_KEY_SET.has(key)) {
            annotations.delete(key);
            relevant = true;
          }
        }
        for (const change of c.boundary.changes) {
          if (RENDER_KEY_SET.has(change.key)) {
            if (change.newValue === null) {
              annotations.delete(change.key);
            } else {
              annotations.set(change.key, change.newValue);
            }
            relevant = true;
          }
        }
        if (relevant) {
          refreshRun();
        }
        break;
      }
      case "characters": {
        const text = c.characters;
        if (inBody && depth === 1) {
          const line = ensureLine();
          const last = line.items[line.items.length - 1];
          const offset = location - line.contentStart;
          if (last !== undefined && last.kind === "text" && last.key === runKey) {
            line.items[line.items.length - 1] = {
              ...last,
              text: last.text + text,
              size: last.size + text.length,
            };
          } else {
            line.items.push({
              kind: "text",
              offset,
              size: text.length,
              text,
              annotations: runAnnotations,
              key: runKey,
            });
          }
        } else if (atom !== null) {
          atom.text += text;
        }
        location += text.length;
        break;
      }
      case "elementStart": {
        if (!inBody) {
          if (depth === 0 && c.tag === "body" && !bodyDone) {
            inBody = true;
            bodyStart = location;
          }
        } else if (depth === 1) {
          if (c.tag === "line") {
            finishLine(location);
            const type = c.attributes.get("t") ?? "";
            current = {
              start: location,
              contentStart: location + 2,
              hasElement: true,
              type: LINE_TYPES.has(type) ? (type as LineType) : "",
              indent: parseIndent(c.attributes.get("i")),
              align: c.attributes.get("a") ?? "",
              dir: c.attributes.get("d") ?? "",
              decimal: c.attributes.get("listyle") === "decimal",
              items: [],
            };
            inLineElement = true;
          } else {
            ensureLine();
            const attrs = new Map<string, string>();
            for (const a of c.attributes.entries) {
              attrs.set(a.name, a.value);
            }
            atom = { tag: c.tag, start: location, depth, attrs, text: "" };
          }
        }
        depth++;
        location++;
        break;
      }
      case "elementEnd": {
        depth--;
        location++;
        if (inBody && depth === 0) {
          inBody = false;
          bodyDone = true;
          bodyEnd = location - 1;
          finishLine(bodyEnd);
        } else if (inBody && depth === 1) {
          if (inLineElement) {
            inLineElement = false;
            // Anything inside a <line> element is not ours; skip past it.
            if (current !== null) {
              current.contentStart = location;
            }
          } else if (atom !== null && current !== null) {
            const line: LineDraft = current;
            const offset = atom.start - line.contentStart;
            const size = location - atom.start;
            if (atom.tag === "reply") {
              line.items.push({ kind: "reply", offset, size, id: atom.attrs.get("id") ?? "" });
            } else if (atom.tag === "image" && atom.attrs.has("attachment")) {
              line.items.push({
                kind: "image",
                offset,
                size,
                attachment: atom.attrs.get("attachment") ?? "",
                caption: atom.text,
                full: atom.attrs.get("style") === "full",
              });
            } else if (atom.tag === "gadget" && atom.attrs.has("app") && (atom.attrs.get("id") ?? "") !== "") {
              line.items.push({
                kind: "app",
                offset,
                size,
                id: atom.attrs.get("id") ?? "",
                app: atom.attrs.get("app") ?? "",
                bundle: atom.attrs.get("bundle") ?? "",
              });
            } else {
              line.items.push({
                kind: "chip",
                offset,
                size,
                tag: atom.tag,
                label: atom.attrs.get("title") ?? atom.attrs.get("name") ?? "",
              });
            }
            atom = null;
          }
        }
        break;
      }
      default:
        break;
    }
  }
  numberLines(lines);
  for (const line of lines) {
    line.sig = signature(line);
  }
  return { bodyStart, bodyEnd, lines, size: location };
}

/**
 * Numbers the numbered list items as the original does
 * (`OrderedListRenumberer`): one counter per indent level; a line that is
 * not a numbered list item resets the counters of its own and all deeper
 * levels; a numbered item resets the counters of the deeper levels.
 */
export function numberLines(lines: readonly Line[]): void {
  const counters: number[] = [];
  for (const line of lines) {
    const level = line.indent;
    if (line.type === "li" && line.decimal) {
      const n = (counters[level] ?? 0) + 1;
      counters[level] = n;
      counters.length = level + 1;
      line.number = n;
    } else {
      counters.length = Math.min(counters.length, level);
      line.number = 0;
    }
  }
}

function signature(line: Line): string {
  let sig = `${line.type}|${line.indent}|${line.align}|${line.dir}|${line.decimal ? 1 : 0}|${line.number}`;
  for (const item of line.items) {
    switch (item.kind) {
      case "text":
        sig += `\u0003t${item.key}\u0004${item.text}`;
        break;
      case "reply":
        sig += `\u0003r${item.id}`;
        break;
      case "image":
        sig += `\u0003i${item.attachment}\u0004${item.full ? 1 : 0}\u0004${item.caption}`;
        break;
      case "app":
        sig += `\u0003a${item.id}\u0004${item.app}\u0004${item.bundle}`;
        break;
      case "chip":
        sig += `\u0003c${item.tag}\u0004${item.label}`;
        break;
    }
  }
  return sig;
}

/** The index of the line whose content holds a location (clamped). */
export function lineIndexAt(model: DocModel, location: number): number {
  const lines = model.lines;
  let lo = 0;
  let hi = lines.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (lines[mid]!.contentStart <= location) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  return Math.max(lo, 0);
}

/** The first location a caret can be at (start of the first line's content). */
export function firstCaret(model: DocModel): number {
  return model.lines[0]?.contentStart ?? 0;
}

/** The last location a caret can be at (end of the body's content). */
export function lastCaret(model: DocModel): number {
  return model.lines.length === 0 ? 0 : model.bodyEnd;
}

/**
 * The nearest location a caret may be at: inside the body, after the first
 * line element, not inside a `<line>` element and not inside an atom.
 * `bias` chooses the side when the location is inside something atomic.
 */
export function clampCaret(model: DocModel, location: number, bias: "before" | "after" = "after"): number {
  if (model.lines.length === 0) {
    return 0;
  }
  const first = firstCaret(model);
  if (location <= first) {
    return first;
  }
  if (location >= model.bodyEnd) {
    return model.bodyEnd;
  }
  const line = model.lines[lineIndexAt(model, location)]!;
  if (location > line.end) {
    // Inside the next line's element.
    return bias === "before" ? line.end : (model.lines[lineIndexAt(model, location) + 1]?.contentStart ?? line.end);
  }
  const offset = location - line.contentStart;
  for (const item of line.items) {
    if (item.kind !== "text" && offset > item.offset && offset < item.offset + item.size) {
      return line.contentStart + (bias === "before" ? item.offset : item.offset + item.size);
    }
  }
  return location;
}

/** The text of a line with every atom as U+FFFC. */
export function lineText(line: Line): string {
  let text = "";
  for (const item of line.items) {
    text += item.kind === "text" ? item.text : "\uFFFC";
  }
  return text;
}
