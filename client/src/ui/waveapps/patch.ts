// Draws what a wave app renders into its (closed) shadow root. The app
// sends a flat list of nodes in document order, each naming its parent by
// index. Only whitelisted tags and attributes are built, with
// `createElement` and `textContent`, never markup; no app code runs here.
// The listeners are ours: they send the events a node asks for (`on`).

import type { AppPair, AppUiNode } from "../../wire";

/** Tags an app may use. `surface` is reserved (a later WebGL scene). */
export const TAGS: ReadonlySet<string> = new Set([
  "div",
  "span",
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "ul",
  "ol",
  "li",
  "button",
  "label",
  "input",
  "select",
  "option",
  "textarea",
  "fieldset",
  "legend",
  "form",
  "img",
  "canvas",
  "table",
  "tr",
  "td",
  "th",
  "hr",
  "br",
  "strong",
  "em",
  "code",
  "pre",
  "progress",
]);

/** Attributes an app may set. No `on*`, `style` or `href`. */
const ATTRS: ReadonlySet<string> = new Set([
  "class",
  "id",
  "name",
  "value",
  "placeholder",
  "type",
  "checked",
  "disabled",
  "min",
  "max",
  "step",
  "for",
  "alt",
  "src",
  "width",
  "height",
  "rows",
  "cols",
  "maxlength",
  "selected",
  "readonly",
  "required",
  "title",
  "colspan",
  "rowspan",
]);

const INPUT_TYPES: ReadonlySet<string> = new Set([
  "text",
  "number",
  "checkbox",
  "radio",
  "range",
  "date",
  "password",
  "submit",
  "button",
]);

/** Events an app may ask for. */
export const EVENTS: ReadonlySet<string> = new Set(["click", "input", "change", "submit", "keydown"]);

/** How long `input` events wait for more typing. */
export const INPUT_DEBOUNCE_MS = 150;

const BOOLEAN_ATTRS: ReadonlySet<string> = new Set(["checked", "disabled", "selected", "readonly", "required"]);

/** An event for the app. */
export interface AppEventOut {
  readonly node: number;
  readonly name: string;
  readonly value?: string;
  readonly fields?: readonly AppPair[];
}

export interface PatchOptions {
  /** Sends an event to the app. */
  readonly send: (event: AppEventOut) => void;
  /** The URL of an attachment an `img` names; null drops the `src`. */
  readonly attachmentUrl?: (id: string) => string | null;
  /** Named in the hint of secret fields ("Sent to <app>"). */
  readonly appName?: string;
}

/** Whether a value names an attachment (`<domain>/<id>` or `<id>`). */
function isAttachmentId(value: string): boolean {
  return /^([A-Za-z0-9.-]+\/)?[A-Za-z0-9_+~.-]{1,200}$/.test(value);
}

interface Built {
  readonly el: Element;
  readonly tag: string;
  readonly parent: number | undefined;
  /** The text node holding the node's text, if it has one. */
  text: Text | null;
  /** The `value` the app last sent, to keep what the user typed meanwhile. */
  appValue: string | null;
  appChecked: boolean | null;
  listening: Set<string>;
}

/** Draws one app view into `root` and keeps it up to date. */
export class AppPatcher {
  /** By node index; null where a node was left out. */
  private built: (Built | null)[] = [];
  private readonly index = new WeakMap<Element, number>();
  private readonly wanted = new WeakMap<Element, ReadonlySet<string>>();
  private readonly timers = new Map<Element, ReturnType<typeof setTimeout>>();

  constructor(
    private readonly root: ParentNode & Node,
    private readonly options: PatchOptions,
  ) {}

  /** The elements built, by node index (tests). */
  elements(): readonly Element[] {
    return this.built.map((b) => b?.el ?? null).filter((el): el is Element => el !== null);
  }

  /** Draws a new render. Nodes that are not allowed are left out, with what is under them. */
  apply(nodes: readonly AppUiNode[]): void {
    const doc = this.root.ownerDocument ?? (this.root as unknown as Document);
    const next: (Built | null)[] = [];
    nodes.forEach((node, i) => {
      const parent = node.parent;
      const parentOk = parent === undefined || (parent >= 0 && parent < i && next[parent] !== null);
      if (!TAGS.has(node.tag) || !parentOk) {
        next.push(null);
        return;
      }
      const old = this.built[i];
      const reuse = old !== undefined && old !== null && old.tag === node.tag && old.parent === parent ? old : null;
      const built: Built = reuse ?? {
        el: doc.createElement(node.tag),
        tag: node.tag,
        parent,
        text: null,
        appValue: null,
        appChecked: null,
        listening: new Set(),
      };
      this.update(built, node, doc);
      this.index.set(built.el, i);
      next.push(built);
    });
    // Put every element under its parent, in order.
    const children = new Map<Node, Node[]>();
    children.set(this.root, []);
    next.forEach((built) => {
      if (built === null) {
        return;
      }
      const parentNode: Node = built.parent === undefined ? this.root : next[built.parent]!.el;
      let list = children.get(parentNode);
      if (list === undefined) {
        list = [];
        children.set(parentNode, list);
      }
      list.push(built.el);
      children.set(built.el, children.get(built.el) ?? []);
    });
    for (const built of next) {
      if (built !== null && built.text !== null) {
        children.get(built.el)!.unshift(built.text);
      }
    }
    for (const [parentNode, wantedChildren] of children) {
      syncChildren(parentNode, wantedChildren, parentNode === this.root);
    }
    for (const old of this.built) {
      if (old !== null && !next.includes(old)) {
        const timer = this.timers.get(old.el);
        if (timer !== undefined) {
          clearTimeout(timer);
          this.timers.delete(old.el);
        }
      }
    }
    this.built = next;
  }

  private update(built: Built, node: AppUiNode, doc: Document): void {
    const el = built.el;
    const attrs = new Map<string, string>();
    for (const { name, value } of node.attrs) {
      const key = name.toLowerCase();
      if (ATTRS.has(key)) {
        attrs.set(key, value);
      }
    }
    let secret = false;
    if (built.tag === "input") {
      const type = (attrs.get("type") ?? "text").toLowerCase();
      if (!INPUT_TYPES.has(type)) {
        attrs.set("type", "text");
      } else if (type === "password") {
        // Masked by CSS, not a password field: the browser's password
        // manager must never offer the user's own password to an app.
        attrs.set("type", "text");
        secret = true;
      }
    }
    if (built.tag === "img") {
      const src = attrs.get("src");
      const url = src !== undefined && isAttachmentId(src) ? (this.options.attachmentUrl?.(src) ?? null) : null;
      if (url === null) {
        attrs.delete("src");
      } else {
        attrs.set("src", url);
      }
    } else {
      attrs.delete("src");
    }
    const classes = attrs.get("class") ?? "";
    if (secret) {
      attrs.set("class", classes === "" ? "app-secret" : `${classes} app-secret`);
      attrs.set("autocomplete", "off");
      attrs.set("data-lpignore", "true");
      attrs.set("title", `Sent to ${this.options.appName ?? "the app"}`);
    }
    const isField = built.tag === "input" || built.tag === "textarea" || built.tag === "select";
    if (isField && built.tag === "input" && (attrs.get("type") === "text" || attrs.get("type") === undefined)) {
      attrs.set("autocomplete", "off");
    }
    // Attributes.
    for (const existing of Array.from(el.attributes)) {
      if (!attrs.has(existing.name)) {
        el.removeAttribute(existing.name);
      }
    }
    for (const [name, value] of attrs) {
      if (name === "value" && isField) {
        continue;
      }
      if (BOOLEAN_ATTRS.has(name) && (value === "false" || value === "0")) {
        el.removeAttribute(name);
        continue;
      }
      if (el.getAttribute(name) !== value) {
        el.setAttribute(name, value);
      }
    }
    // The value of a field: only when the app changed it, so typing is kept.
    if (isField) {
      const field = el as HTMLInputElement;
      const value = attrs.get("value") ?? null;
      if (value !== built.appValue) {
        built.appValue = value;
        field.value = value ?? "";
      }
      if (built.tag === "input") {
        const checkedAttr = attrs.get("checked");
        const checked = checkedAttr !== undefined && checkedAttr !== "false" && checkedAttr !== "0";
        if (checked !== built.appChecked) {
          built.appChecked = checked;
          field.checked = checked;
        }
      }
    }
    // Text.
    const text = built.tag === "input" || built.tag === "br" || built.tag === "hr" || built.tag === "img" ? undefined : node.text;
    if (text === undefined) {
      built.text = null;
    } else if (built.text === null) {
      built.text = doc.createTextNode(text);
    } else if (built.text.data !== text) {
      built.text.data = text;
    }
    // Events.
    const want = new Set(node.on.filter((name) => EVENTS.has(name)));
    this.wanted.set(el, want);
    for (const name of want) {
      this.listen(built, name);
    }
    if (built.tag === "form") {
      // Never navigate, whether the app wants the submit or not.
      this.listen(built, "submit");
    }
  }

  private listen(built: Built, name: string): void {
    if (built.listening.has(name)) {
      return;
    }
    built.listening.add(name);
    const el = built.el;
    el.addEventListener(name, (event) => {
      if (name === "submit") {
        event.preventDefault();
      }
      if (!this.wanted.get(el)?.has(name)) {
        return;
      }
      const node = this.index.get(el);
      if (node === undefined) {
        return;
      }
      switch (name) {
        case "click":
          if (el.tagName === "BUTTON" && el.closest("form") !== null) {
            // A button in a form would also submit it; the app asked for the click.
            event.preventDefault();
          }
          this.options.send({ node, name });
          break;
        case "input": {
          const previous = this.timers.get(el);
          if (previous !== undefined) {
            clearTimeout(previous);
          }
          this.timers.set(
            el,
            setTimeout(() => {
              this.timers.delete(el);
              this.options.send({ node, name, value: valueOf(el) });
            }, INPUT_DEBOUNCE_MS),
          );
          break;
        }
        case "change":
          this.options.send({ node, name, value: valueOf(el) });
          break;
        case "keydown":
          this.options.send({ node, name, value: (event as KeyboardEvent).key });
          break;
        case "submit":
          this.options.send({ node, name, fields: fieldsOf(el) });
          break;
      }
    });
  }
}

function valueOf(el: Element): string {
  const field = el as HTMLInputElement;
  if (field.type === "checkbox" || field.type === "radio") {
    return field.checked ? "true" : "false";
  }
  return field.value ?? "";
}

/** The named fields of a form, as a form submission would send them. */
function fieldsOf(form: Element): AppPair[] {
  const fields: AppPair[] = [];
  for (const el of Array.from(form.querySelectorAll("input, select, textarea"))) {
    const field = el as HTMLInputElement;
    const name = field.getAttribute("name");
    if (name === null || name === "" || field.disabled) {
      continue;
    }
    if ((field.type === "checkbox" || field.type === "radio") && !field.checked) {
      continue;
    }
    fields.push({ name, value: field.value ?? "" });
  }
  return fields;
}

/** Makes `parent`'s children exactly `wanted`, moving as little as it can. */
function syncChildren(parent: Node, wanted: readonly Node[], isRoot: boolean): void {
  const keep = new Set(wanted);
  for (const child of Array.from(parent.childNodes)) {
    if (!keep.has(child) && !(isRoot && isFixed(child))) {
      parent.removeChild(child);
    }
  }
  let cursor: Node | null = firstMovable(parent, isRoot);
  for (const node of wanted) {
    if (cursor === node) {
      cursor = cursor.nextSibling;
      continue;
    }
    parent.insertBefore(node, cursor);
  }
}

/** Nodes of the root that are ours (the style sheet). */
function isFixed(node: Node): boolean {
  return node.nodeType === 1 && (node as Element).hasAttribute("data-app-fixed");
}

function firstMovable(parent: Node, isRoot: boolean): Node | null {
  let node = parent.firstChild;
  while (isRoot && node !== null && isFixed(node)) {
    node = node.nextSibling;
  }
  return node;
}
