// Clipboard content in and out.
//
// In: `text/html` is parsed by `DOMParser` into an inert document (no
// scripts run, nothing loads) and walked; only text, a fixed set of inline
// styles, links with a safe target and block structure survive, as
// `PasteBlock`s. Nothing of the HTML itself is ever inserted anywhere.
//
// Out: the selected blocks as clean HTML, built with DOM APIs.

import type { PasteBlock, PasteRun } from "./core";
import {
  LINK_MANUAL,
  STYLE_BG_COLOR,
  STYLE_COLOR,
  STYLE_FONT_STYLE,
  STYLE_FONT_WEIGHT,
  STYLE_TEXT_DECORATION,
  STYLE_VERTICAL_ALIGN,
} from "./model";
import { decorationValue, decorations, isBold, isItalic, linkOf, safeColor } from "./styles";
import { normalizeLink } from "./text";

const SKIP = new Set([
  "SCRIPT", "STYLE", "HEAD", "TITLE", "META", "LINK", "NOSCRIPT", "IFRAME", "OBJECT", "EMBED", "SVG",
  "MATH", "TEMPLATE", "IMG", "PICTURE", "VIDEO", "AUDIO", "CANVAS", "INPUT", "BUTTON", "SELECT",
  "TEXTAREA", "FRAME", "FRAMESET", "APPLET", "BASE", "MAP", "AREA", "DIALOG",
]);

const BLOCK = new Set([
  "P", "DIV", "H1", "H2", "H3", "H4", "H5", "H6", "LI", "UL", "OL", "BLOCKQUOTE", "PRE", "TR", "TABLE",
  "SECTION", "ARTICLE", "HEADER", "FOOTER", "ASIDE", "NAV", "MAIN", "FIGURE", "FIGCAPTION", "DL", "DT",
  "DD", "ADDRESS", "HR", "FORM", "FIELDSET", "DETAILS", "SUMMARY", "TBODY", "THEAD", "TFOOT", "CENTER",
]);

const HEADING: Readonly<Record<string, "h1" | "h2" | "h3" | "h4">> = {
  H1: "h1", H2: "h2", H3: "h3", H4: "h4", H5: "h4", H6: "h4",
};

interface Inline {
  bold: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
  vertical: "super" | "sub" | null;
  color: string | null;
  background: string | null;
  link: string | null;
}

interface BlockContext {
  type: "" | "h1" | "h2" | "h3" | "h4" | "li";
  decimal: boolean;
  indent: number;
  pre: boolean;
}

const PLAIN: Inline = {
  bold: false, italic: false, underline: false, strike: false, vertical: null, color: null,
  background: null, link: null,
};

function annotationsOf(s: Inline): Record<string, string> {
  const a: Record<string, string> = {};
  if (s.bold) {
    a[STYLE_FONT_WEIGHT] = "bold";
  }
  if (s.italic) {
    a[STYLE_FONT_STYLE] = "italic";
  }
  const deco = decorationValue(s.underline && !s.strike, s.strike);
  if (deco !== null) {
    a[STYLE_TEXT_DECORATION] = deco;
  }
  if (s.vertical !== null) {
    a[STYLE_VERTICAL_ALIGN] = s.vertical;
  }
  if (s.color !== null) {
    a[STYLE_COLOR] = s.color;
  }
  if (s.background !== null) {
    a[STYLE_BG_COLOR] = s.background;
  }
  if (s.link !== null) {
    a[LINK_MANUAL] = s.link;
  }
  return a;
}

function sameAnnotations(a: Readonly<Record<string, string>>, b: Readonly<Record<string, string>>): boolean {
  const ka = Object.keys(a);
  return ka.length === Object.keys(b).length && ka.every((k) => a[k] === b[k]);
}

/** Wave lines and annotations from clipboard HTML. */
export function parseHtml(html: string): PasteBlock[] {
  let parsed: Document;
  try {
    parsed = new DOMParser().parseFromString(html, "text/html");
  } catch {
    return [];
  }
  const blocks: { type: BlockContext["type"]; decimal: boolean; indent: number; runs: PasteRun[] }[] = [];
  let open = false;
  const openBlock = (ctx: BlockContext): PasteRun[] => {
    if (!open) {
      blocks.push({ type: ctx.type, decimal: ctx.decimal, indent: ctx.indent, runs: [] });
      open = true;
    }
    return blocks[blocks.length - 1]!.runs;
  };
  const closeBlock = (): void => {
    open = false;
  };
  const addText = (raw: string, ctx: BlockContext, inline: Inline): void => {
    const parts = ctx.pre ? raw.split(/\r\n|\r|\n/) : [raw.replace(/[ \t\r\n\f]+/g, " ")];
    parts.forEach((part, i) => {
      if (i > 0) {
        if (!open) {
          openBlock(ctx);
        }
        closeBlock();
      }
      let text = part;
      const runs = open ? blocks[blocks.length - 1]!.runs : null;
      const last = runs === null ? undefined : runs[runs.length - 1];
      if (!ctx.pre && (last === undefined || last.text.endsWith(" "))) {
        text = text.replace(/^ /, "");
      }
      if (text === "") {
        return;
      }
      const target = openBlock(ctx);
      const annotations = annotationsOf(inline);
      const prev = target[target.length - 1];
      if (prev !== undefined && sameAnnotations(prev.annotations, annotations)) {
        target[target.length - 1] = { text: prev.text + text, annotations };
      } else {
        target.push({ text, annotations });
      }
    });
  };
  const walk = (node: Node, ctx: BlockContext, inline: Inline, listDepth: number): void => {
    for (let child = node.firstChild; child !== null; child = child.nextSibling) {
      if (child.nodeType === 3) {
        addText((child as Text).data, ctx, inline);
        continue;
      }
      if (child.nodeType !== 1) {
        continue;
      }
      const el = child as HTMLElement;
      const tag = el.tagName.toUpperCase();
      if (SKIP.has(tag)) {
        continue;
      }
      if (tag === "BR") {
        openBlock(ctx);
        closeBlock();
        continue;
      }
      const next: Inline = { ...inline };
      switch (tag) {
        case "B":
        case "STRONG":
          next.bold = true;
          break;
        case "I":
        case "EM":
          next.italic = true;
          break;
        case "U":
        case "INS":
          next.underline = true;
          break;
        case "S":
        case "STRIKE":
        case "DEL":
          next.strike = true;
          break;
        case "SUP":
          next.vertical = "super";
          break;
        case "SUB":
          next.vertical = "sub";
          break;
        case "A": {
          const href = el.getAttribute("href");
          const link = href === null ? null : normalizeLink(href);
          if (link !== null && !/^wave:/i.test(link)) {
            next.link = link;
          }
          break;
        }
        default:
          break;
      }
      const style = el.style as CSSStyleDeclaration | undefined;
      if (style !== undefined && el.hasAttribute("style")) {
        const weight = style.fontWeight;
        if (weight !== "") {
          next.bold = isBold(weight) || Number.parseInt(weight, 10) >= 600;
        }
        if (style.fontStyle !== "") {
          next.italic = isItalic(style.fontStyle);
        }
        const deco = decorations(`${style.textDecoration} ${style.textDecorationLine ?? ""}`);
        next.underline = next.underline || deco.underline;
        next.strike = next.strike || deco.strike;
        const color = safeColor(style.color);
        if (color !== null) {
          next.color = color;
        }
        const background = safeColor(style.backgroundColor);
        if (background !== null) {
          next.background = background;
        }
      }
      if (!BLOCK.has(tag)) {
        walk(el, ctx, next, listDepth);
        continue;
      }
      closeBlock();
      let childCtx: BlockContext = ctx;
      let depth = listDepth;
      if (tag in HEADING) {
        childCtx = { type: HEADING[tag]!, decimal: false, indent: 0, pre: ctx.pre };
      } else if (tag === "UL" || tag === "OL") {
        depth = listDepth + 1;
        childCtx = { type: "li", decimal: tag === "OL", indent: Math.max(depth - 1, 0), pre: ctx.pre };
      } else if (tag === "LI") {
        childCtx = ctx.type === "li" ? ctx : { type: "li", decimal: false, indent: 0, pre: ctx.pre };
      } else if (tag === "PRE") {
        childCtx = { ...ctx, pre: true };
      } else if (tag === "P" || tag === "DIV") {
        // A paragraph inside a list item stays part of the list.
        childCtx = ctx;
      }
      walk(el, childCtx, next, depth);
      closeBlock();
    }
  };
  if (parsed.body !== null) {
    walk(parsed.body, { type: "", decimal: false, indent: 0, pre: false }, PLAIN, 0);
  }
  // Trailing whitespace of a block is markup formatting.
  return blocks.map((b) => {
    const runs = [...b.runs];
    const last = runs[runs.length - 1];
    if (last !== undefined && last.text.endsWith(" ")) {
      const text = last.text.replace(/ +$/, "");
      if (text === "") {
        runs.pop();
      } else {
        runs[runs.length - 1] = { text, annotations: last.annotations };
      }
    }
    return { type: b.type, decimal: b.decimal, indent: b.indent, runs };
  });
}

/** Whether pasted blocks carry anything beyond one line of plain text. */
export function isPlain(blocks: readonly PasteBlock[]): boolean {
  return blocks.every(
    (b) => (b.type ?? "") === "" && (b.indent ?? 0) === 0 && b.runs.every((r) => Object.keys(r.annotations).length === 0),
  );
}

export function blocksToText(blocks: readonly PasteBlock[]): string {
  return blocks.map((b) => b.runs.map((r) => r.text).join("")).join("\n");
}

/** Clean HTML for the clipboard. */
export function blocksToHtml(blocks: readonly PasteBlock[], doc: Document): string {
  const container = doc.createElement("div");
  let list: { el: HTMLElement; decimal: boolean } | null = null;
  for (const block of blocks) {
    const type = block.type ?? "";
    let el: HTMLElement;
    if (type === "li") {
      const decimal = block.decimal === true;
      if (list === null || list.decimal !== decimal) {
        list = { el: doc.createElement(decimal ? "ol" : "ul"), decimal };
        container.appendChild(list.el);
      }
      el = doc.createElement("li");
      list.el.appendChild(el);
    } else {
      list = null;
      el = doc.createElement(type === "" ? "p" : type);
      container.appendChild(el);
    }
    for (const run of block.runs) {
      const a = run.annotations;
      let node: Node = doc.createTextNode(run.text);
      const wrap = (tag: string): HTMLElement => {
        const w = doc.createElement(tag);
        w.appendChild(node);
        node = w;
        return w;
      };
      const deco = decorations(a[STYLE_TEXT_DECORATION]);
      if (isBold(a[STYLE_FONT_WEIGHT])) {
        wrap("b");
      }
      if (isItalic(a[STYLE_FONT_STYLE])) {
        wrap("i");
      }
      if (deco.underline) {
        wrap("u");
      }
      if (deco.strike) {
        wrap("s");
      }
      if (a[STYLE_VERTICAL_ALIGN] === "super") {
        wrap("sup");
      } else if (a[STYLE_VERTICAL_ALIGN] === "sub") {
        wrap("sub");
      }
      const color = safeColor(a[STYLE_COLOR]);
      const background = safeColor(a[STYLE_BG_COLOR]);
      if (color !== null || background !== null) {
        const span = wrap("span");
        if (color !== null) {
          span.style.color = color;
        }
        if (background !== null) {
          span.style.backgroundColor = background;
        }
      }
      const link = linkOf(a);
      if (link !== null && /^(?:https?:\/\/|mailto:)/i.test(link.value)) {
        wrap("a").setAttribute("href", link.value);
      }
      el.appendChild(node);
    }
    if (block.runs.length === 0) {
      el.appendChild(doc.createElement("br"));
    }
  }
  return container.innerHTML;
}
