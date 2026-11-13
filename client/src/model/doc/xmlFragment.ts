// A balanced piece of document content to insert, and its XML parser
// (`XmlFragment` in the Rust `doc/mod.rs`; a separate module here only to
// keep the module graph free of cycles).
//
// TypeScript port of the Rust crate `resonator-model` (module `doc`), itself
// derived from Apache Wave (`XmlStringBuilder`), licensed under the Apache
// License, Version 2.0.

import { Attributes, type DocInitialization } from "../docop";
import { isXmlName } from "../docop/utf16";
import { DocError } from "./error";
import { attributesFromPairs } from "./nindo";

/** One item of an {@link XmlFragment}. */
export type XmlItem =
  | { readonly type: "text"; readonly text: string }
  | { readonly type: "start"; readonly tag: string; readonly attributes: Attributes }
  | { readonly type: "end" };

const END: XmlItem = { type: "end" };

function pushText(items: XmlItem[], text: string): void {
  if (text.length === 0) {
    return;
  }
  const last = items[items.length - 1];
  if (last !== undefined && last.type === "text") {
    items[items.length - 1] = { type: "text", text: last.text + text };
  } else {
    items.push({ type: "text", text });
  }
}

// Unicode White_Space, as Rust's `char::is_whitespace`.
const WS = "\\t-\\r \\u0085\\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000";
const FIRST_WS = new RegExp(`[${WS}]`);
const LEADING_WS = new RegExp(`^[${WS}]+`);
const TRAILING_WS = new RegExp(`[${WS}]+$`);

const trimStart = (s: string): string => s.replace(LEADING_WS, "");
const trimEnd = (s: string): string => s.replace(TRAILING_WS, "");

/**
 * A balanced piece of document content to insert (`XmlStringBuilder`).
 *
 * The original holds an XML string that is parsed when inserted; this holds
 * the parsed items, so text needs no escaping. Immutable: the builder
 * methods return new fragments.
 */
export class XmlFragment {
  private static readonly EMPTY = new XmlFragment([]);

  /** The items, adjacent text merged. */
  readonly items: readonly XmlItem[];

  private constructor(items: readonly XmlItem[]) {
    this.items = items;
  }

  /** `XmlStringBuilder.createEmpty`. */
  static empty(): XmlFragment {
    return XmlFragment.EMPTY;
  }

  /** `XmlStringBuilder.createText`. */
  static text(text: string): XmlFragment {
    return XmlFragment.EMPTY.appendText(text);
  }

  /** The content of a document initialization, without its annotations. */
  static fromInitialization(content: DocInitialization): XmlFragment {
    const items: XmlItem[] = [];
    for (const c of content.components) {
      switch (c.type) {
        case "characters":
          pushText(items, c.characters);
          break;
        case "elementStart":
          items.push({ type: "start", tag: c.tag, attributes: c.attributes });
          break;
        case "elementEnd":
          items.push(END);
          break;
        default:
          break;
      }
    }
    return new XmlFragment(items);
  }

  /** Appends text. */
  appendText(text: string): XmlFragment {
    const items = [...this.items];
    pushText(items, text);
    return new XmlFragment(items);
  }

  /** Appends another fragment (`append`). */
  append(other: XmlFragment): XmlFragment {
    const items = [...this.items];
    for (const item of other.items) {
      if (item.type === "text") {
        pushText(items, item.text);
      } else {
        items.push(item);
      }
    }
    return new XmlFragment(items);
  }

  /** Wraps the fragment in an element (`wrap`), by default without attributes. */
  wrap(tag: string, attributes: Attributes = Attributes.EMPTY): XmlFragment {
    return new XmlFragment([{ type: "start", tag, attributes }, ...this.items, END]);
  }

  /** Same as {@link wrap} with attributes. */
  wrapWithAttributes(tag: string, attributes: Attributes): XmlFragment {
    return this.wrap(tag, attributes);
  }

  isEmpty(): boolean {
    return this.items.length === 0;
  }

  /** Number of document items the fragment inserts (`getLength`). */
  itemCount(): number {
    let count = 0;
    for (const item of this.items) {
      count += item.type === "text" ? item.text.length : 1;
    }
    return count;
  }

  equals(other: XmlFragment): boolean {
    return (
      this.items.length === other.items.length &&
      this.items.every((a, i) => {
        const b = other.items[i];
        if (b === undefined || a.type !== b.type) {
          return false;
        }
        switch (a.type) {
          case "text":
            return b.type === "text" && a.text === b.text;
          case "start":
            return b.type === "start" && a.tag === b.tag && a.attributes.equals(b.attributes);
          case "end":
            return true;
        }
      })
    );
  }

  /**
   * Parses a piece of XML: elements with attributes and text
   * (`XmlStringBuilder.createFromXmlString`). The five predefined entities
   * and numeric character references are understood; comments, processing
   * instructions, CDATA sections and doctypes are rejected.
   */
  static parse(xml: string): XmlFragment {
    const bad = (message: string): DocError =>
      DocError.invalidArgument(`Ill-formed xml: ${message}`);
    const unescape = (text: string): string => {
      const result = unescapeXml(text);
      if (result === null) {
        throw bad("bad entity");
      }
      return result;
    };
    const items: XmlItem[] = [];
    const open: string[] = [];
    let rest = xml;
    while (rest.length > 0) {
      if (!rest.startsWith("<")) {
        const lt = rest.indexOf("<");
        const end = lt < 0 ? rest.length : lt;
        pushText(items, unescape(rest.slice(0, end)));
        rest = rest.slice(end);
        continue;
      }
      const afterLt = rest.slice(1);
      const end = afterLt.indexOf(">");
      if (end < 0) {
        throw bad("unterminated tag");
      }
      let tag = afterLt.slice(0, end);
      rest = afterLt.slice(end + 1);
      if (tag.startsWith("/")) {
        if (open.pop() !== trimEnd(tag.slice(1))) {
          throw bad("mismatched end tag");
        }
        items.push(END);
        continue;
      }
      if (tag.startsWith("!") || tag.startsWith("?")) {
        throw bad("unsupported markup");
      }
      const selfClosing = tag.endsWith("/");
      if (selfClosing) {
        tag = tag.slice(0, -1);
      }
      const ws = tag.search(FIRST_WS);
      const nameEnd = ws < 0 ? tag.length : ws;
      const name = tag.slice(0, nameEnd);
      if (!isXmlName(name)) {
        throw bad("bad element name");
      }
      const attributes: [string, string][] = [];
      let attrs = trimStart(tag.slice(nameEnd));
      while (attrs.length > 0) {
        const eq = attrs.indexOf("=");
        if (eq < 0) {
          throw bad("attribute without value");
        }
        const attrName = trimEnd(attrs.slice(0, eq));
        if (!isXmlName(attrName)) {
          throw bad("bad attribute name");
        }
        const value = trimStart(attrs.slice(eq + 1));
        const quote = value.charAt(0);
        if (quote !== '"' && quote !== "'") {
          throw bad("unquoted attribute value");
        }
        const close = value.indexOf(quote, 1);
        if (close < 0) {
          throw bad("unterminated attribute value");
        }
        const unescaped = unescape(value.slice(1, close));
        if (attributes.some(([n]) => n === attrName)) {
          throw bad("duplicate attribute");
        }
        attributes.push([attrName, unescaped]);
        attrs = trimStart(value.slice(close + 1));
      }
      items.push({ type: "start", tag: name, attributes: attributesFromPairs(attributes) });
      if (selfClosing) {
        items.push(END);
      } else {
        open.push(name);
      }
    }
    if (open.length > 0) {
      throw bad("unclosed element");
    }
    return new XmlFragment(items);
  }
}

const ENTITIES: Readonly<Record<string, string>> = {
  lt: "<",
  gt: ">",
  amp: "&",
  quot: '"',
  apos: "'",
};

/** `null` on an unknown or unterminated entity. */
function unescapeXml(text: string): string | null {
  let result = "";
  let rest = text;
  for (;;) {
    const amp = rest.indexOf("&");
    if (amp < 0) {
      break;
    }
    result += rest.slice(0, amp);
    const semi = rest.indexOf(";", amp);
    if (semi < 0) {
      return null;
    }
    const entity = rest.slice(amp + 1, semi);
    const named = Object.hasOwn(ENTITIES, entity) ? ENTITIES[entity] : undefined;
    if (named !== undefined) {
      result += named;
    } else {
      if (!entity.startsWith("#")) {
        return null;
      }
      const code = entity.slice(1);
      let value: number;
      if (code.startsWith("x")) {
        if (!/^\+?[0-9a-fA-F]+$/.test(code.slice(1))) {
          return null;
        }
        value = Number.parseInt(code.slice(1), 16);
      } else {
        if (!/^\+?[0-9]+$/.test(code)) {
          return null;
        }
        value = Number.parseInt(code, 10);
      }
      // A Unicode scalar value: in range and not a surrogate.
      if (!(value <= 0x10ffff) || (value >= 0xd800 && value <= 0xdfff)) {
        return null;
      }
      result += String.fromCodePoint(value);
    }
    rest = rest.slice(semi + 1);
  }
  return result + rest;
}
