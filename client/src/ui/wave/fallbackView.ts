// A minimal stand-in for the blip editor: renders a blip document as plain
// text lines with containers for the anchors of inline replies, and keeps
// the rendering current. It implements `BlipView` so that the wave panel
// can be developed and tested without the editor; "editing" only marks
// the view (text can be appended through `appendText`, which the tests
// and the development harness use).

import { THREAD_INLINE_ANCHOR_ID_ATTR, THREAD_INLINE_ANCHOR_TAGNAME } from "../../model/conversation/blips";
import { DocView, LINE_TAGNAME, type NodeId } from "../../model/doc";
import type {
  BlipView,
  BlipViewOptions,
  EditorCommand,
  EditorState,
  Unsubscribe,
} from "../editor/api";
import type { LocationRange } from "../../client/locations";

export interface FallbackBlipView extends BlipView {
  /** Appends text at the end of the body as if the user typed it. */
  appendText(text: string): void;
}

const NO_STYLES = {
  bold: false,
  italic: false,
  underline: false,
  strikethrough: false,
  superscript: false,
  subscript: false,
} as const;

export function createFallbackBlipView(options: BlipViewOptions): FallbackBlipView {
  const { host, doc } = options;
  const ownerDocument = host.ownerDocument;
  const element = ownerDocument.createElement("div");
  element.className = "fallback-doc";
  host.replaceChildren(element);

  const anchors = new Map<string, HTMLElement>();
  const stateListeners = new Set<(state: EditorState) => void>();
  const editListeners = new Set<() => void>();
  let editable = false;
  let destroyed = false;

  const bodyEnd = (): number | null => {
    const view = new DocView(doc.document());
    const body = view.elementWithTagName("body");
    return body === null ? null : view.contentEnd(body);
  };

  const render = (): void => {
    const view = new DocView(doc.document());
    const body = view.elementWithTagName("body");
    const lines: HTMLElement[] = [];
    const seen = new Set<string>();
    let line: HTMLElement | null = null;
    const currentLine = (): HTMLElement => {
      if (line === null) {
        line = ownerDocument.createElement("div");
        line.className = "fallback-line";
        lines.push(line);
      }
      return line;
    };
    const visit = (node: NodeId): void => {
      if (view.isText(node)) {
        currentLine().append(ownerDocument.createTextNode(view.text(node) ?? ""));
        return;
      }
      if (view.hasTag(node, LINE_TAGNAME)) {
        line = null;
        currentLine();
        return;
      }
      if (view.hasTag(node, THREAD_INLINE_ANCHOR_TAGNAME)) {
        const threadId = view.attribute(node, THREAD_INLINE_ANCHOR_ID_ATTR);
        if (threadId !== null && !seen.has(threadId)) {
          seen.add(threadId);
          let container = anchors.get(threadId);
          if (container === undefined) {
            container = ownerDocument.createElement("span");
            container.className = "blip-anchor";
            container.contentEditable = "false";
            anchors.set(threadId, container);
            currentLine().append(container);
            options.anchors?.attach(threadId, container);
          } else {
            currentLine().append(container);
          }
        }
        return;
      }
      for (const child of view.children(node)) {
        visit(child);
      }
    };
    if (body !== null) {
      for (const child of view.children(body)) {
        visit(child);
      }
    }
    for (const [threadId, container] of [...anchors]) {
      if (!seen.has(threadId)) {
        anchors.delete(threadId);
        options.anchors?.detach(threadId, container);
      }
    }
    element.replaceChildren(...lines);
  };

  const state = (): EditorState => ({
    editable,
    focused: ownerDocument.activeElement === element,
    hasRange: false,
    styles: NO_STYLES,
    fontSize: null,
    fontFamily: null,
    color: null,
    backgroundColor: null,
    heading: 0,
    list: "none",
    alignment: "left",
    link: null,
    canUndo: false,
    canRedo: false,
  });

  const announce = (): void => {
    const current = state();
    for (const listener of [...stateListeners]) {
      listener(current);
    }
  };

  const appendText = (text: string): void => {
    const end = bodyEnd();
    if (end === null || text === "") {
      return;
    }
    doc.edit((mutable) => mutable.insertText(end, text));
    for (const listener of [...editListeners]) {
      listener();
    }
  };

  const onKeyDown = (event: KeyboardEvent): void => {
    if (!editable) {
      return;
    }
    if (options.onKeyDown?.(event) === true) {
      return;
    }
    // Enough of an editor to try things out: printable keys append.
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      event.stopPropagation();
      appendText(event.key);
    }
  };
  element.addEventListener("keydown", onKeyDown);

  render();
  const unsubscribe = doc.onOp(() => {
    if (!destroyed) {
      render();
    }
  });

  return {
    element,
    get editable() {
      return editable;
    },
    setEditable(value: boolean): void {
      if (editable === value) {
        return;
      }
      editable = value;
      element.classList.toggle("fallback-editing", value);
      if (value) {
        element.tabIndex = 0;
        element.setAttribute("role", "textbox");
        element.setAttribute("aria-multiline", "true");
        element.setAttribute("data-editing", "true");
        if (options.ariaLabel !== undefined) {
          element.setAttribute("aria-label", options.ariaLabel);
        }
      } else {
        element.removeAttribute("tabindex");
        element.removeAttribute("role");
        element.removeAttribute("aria-multiline");
        element.removeAttribute("data-editing");
        element.removeAttribute("aria-label");
      }
      announce();
    },
    focus(): void {
      if (editable) {
        element.focus({ preventScroll: true });
      }
    },
    selection(): LocationRange | null {
      if (!editable) {
        return null;
      }
      const end = bodyEnd();
      return end === null ? null : { start: end, end };
    },
    setSelection(): void {},
    state,
    onStateChange(listener): Unsubscribe {
      stateListeners.add(listener);
      return () => stateListeners.delete(listener);
    },
    onLocalEdit(listener): Unsubscribe {
      editListeners.add(listener);
      return () => editListeners.delete(listener);
    },
    exec(command: EditorCommand): void {
      if (command.type === "insertText") {
        appendText(command.text);
      } else if (command.type === "insertAttachment") {
        appendText(`[${command.caption}]`);
      }
    },
    appendText,
    destroy(): void {
      if (destroyed) {
        return;
      }
      destroyed = true;
      unsubscribe();
      element.removeEventListener("keydown", onKeyDown);
      for (const [threadId, container] of [...anchors]) {
        options.anchors?.detach(threadId, container);
      }
      anchors.clear();
      host.replaceChildren();
    },
  };
}
