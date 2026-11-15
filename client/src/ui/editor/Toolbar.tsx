// The edit toolbar (the original's `EditToolbar`): attachment first, then
// text styles, heading, lists, indentation, alignment, link, colours, font,
// clear formatting, undo/redo, inline reply, Done. State comes from the view's
// `onStateChange`; buttons never take the focus from the document on mouse
// down, and the bar is one tab stop with arrow-key navigation (WAI-ARIA
// toolbar pattern).

import type { ComponentChildren, JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";

import type { Alignment, BlipView, EditorCommand, EditorState, TextStyle } from "./api";

export interface EditToolbarProps {
  view: BlipView;
  onInsertAttachment?: () => void;
  /** Inserts a wave app (a bundle chosen by the user). */
  onInsertApp?: () => void;
  onInsertInlineReply?: () => void;
  onDone?: () => void;
}

function Icon({ children }: { children: ComponentChildren }) {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

const ICONS: Record<string, () => JSX.Element> = {
  bold: () => (
    <Icon>
      <path d="M7 5h6a3.5 3.5 0 0 1 0 7H7zM7 12h7a3.5 3.5 0 0 1 0 7H7z" />
    </Icon>
  ),
  italic: () => (
    <Icon>
      <path d="M10 5h8M6 19h8M14 5l-4 14" />
    </Icon>
  ),
  underline: () => (
    <Icon>
      <path d="M7 4v7a5 5 0 0 0 10 0V4M5 20h14" />
    </Icon>
  ),
  strikethrough: () => (
    <Icon>
      <path d="M4 12h16M16 6.5C15 5 13.6 4.5 12 4.5 9.5 4.5 8 5.8 8 7.6c0 1.3.8 2.2 2.4 2.8M8 17.5c1 1.5 2.4 2 4 2 2.5 0 4-1.3 4-3.1 0-.8-.3-1.5-.9-2" />
    </Icon>
  ),
  superscript: () => (
    <Icon>
      <path d="M4 8l8 10M12 8l-8 10M16 4.5a1.8 1.8 0 1 1 3 1.4L16 9h4" />
    </Icon>
  ),
  subscript: () => (
    <Icon>
      <path d="M4 6l8 10M12 6l-8 10M16 15.5a1.8 1.8 0 1 1 3 1.4L16 20h4" />
    </Icon>
  ),
  bullet: () => (
    <Icon>
      <circle cx="5" cy="7" r="1.2" />
      <circle cx="5" cy="12" r="1.2" />
      <circle cx="5" cy="17" r="1.2" />
      <path d="M9 7h11M9 12h11M9 17h11" />
    </Icon>
  ),
  numbered: () => (
    <Icon>
      <path d="M4 5h1.5v4M4 9h3M4 14.5a1.3 1.3 0 1 1 2.2 1L4 18h3M10 7h10M10 12h10M10 17h10" />
    </Icon>
  ),
  indent: () => (
    <Icon>
      <path d="M4 5h16M10 10h10M10 14h10M4 19h16M4 9.5l3 2.5-3 2.5" />
    </Icon>
  ),
  outdent: () => (
    <Icon>
      <path d="M4 5h16M10 10h10M10 14h10M4 19h16M7 9.5L4 12l3 2.5" />
    </Icon>
  ),
  left: () => (
    <Icon>
      <path d="M4 6h16M4 10h10M4 14h16M4 18h10" />
    </Icon>
  ),
  center: () => (
    <Icon>
      <path d="M4 6h16M7 10h10M4 14h16M7 18h10" />
    </Icon>
  ),
  right: () => (
    <Icon>
      <path d="M4 6h16M10 10h10M4 14h16M10 18h10" />
    </Icon>
  ),
  justify: () => (
    <Icon>
      <path d="M4 6h16M4 10h16M4 14h16M4 18h16" />
    </Icon>
  ),
  link: () => (
    <Icon>
      <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" />
    </Icon>
  ),
  unlink: () => (
    <Icon>
      <path d="M15.7 14l3-3a4 4 0 0 0-5.7-5.7l-1 1M8.3 10l-3 3a4 4 0 0 0 5.7 5.7l1-1M4 4l16 16" />
    </Icon>
  ),
  color: () => (
    <Icon>
      <path d="M7 16l5-12 5 12M8.7 12h6.6" />
      <path class="icon-swatch" d="M4 19.5h16" />
    </Icon>
  ),
  highlight: () => (
    <Icon>
      <path d="M8 14l7-9 4 3-7 9H8zM5 19h14" />
    </Icon>
  ),
  clear: () => (
    <Icon>
      <path d="M6 5h12M12 5l-3 14M4 4l16 16" />
    </Icon>
  ),
  undo: () => (
    <Icon>
      <path d="M9 7L5 11l4 4M5 11h9a5 5 0 0 1 0 10h-2" />
    </Icon>
  ),
  redo: () => (
    <Icon>
      <path d="M15 7l4 4-4 4M19 11h-9a5 5 0 0 0 0 10h2" />
    </Icon>
  ),
  inlineReply: () => (
    <Icon>
      <path d="M4 6h16v9H11l-4 4v-4H4z" />
      <path d="M8 10.5h8" />
    </Icon>
  ),
  attach: () => (
    <Icon>
      <path d="M16.5 7.5l-7 7a2 2 0 0 0 2.8 2.8l7.5-7.5a4 4 0 0 0-5.7-5.7L6.6 11.6a6 6 0 0 0 8.5 8.5L20 15.2" />
    </Icon>
  ),
  app: () => (
    <Icon>
      <rect x="4" y="4" width="7" height="7" rx="1.5" />
      <rect x="13" y="4" width="7" height="7" rx="1.5" />
      <rect x="4" y="13" width="7" height="7" rx="1.5" />
      <path d="M16.5 13.5v6M13.5 16.5h6" />
    </Icon>
  ),
  done: () => (
    <Icon>
      <path d="M5 12.5l4.5 4.5L19 7.5" />
    </Icon>
  ),
};

/** The original's colour palette, reduced (`ColorPicker`). */
const COLORS: readonly (readonly [string, string])[] = [
  ["Black", "#000000"],
  ["Gray", "#666666"],
  ["Red", "#cc0000"],
  ["Orange", "#e69138"],
  ["Yellow", "#f1c232"],
  ["Green", "#38761d"],
  ["Blue", "#1155cc"],
  ["Purple", "#674ea7"],
];
const HIGHLIGHTS: readonly (readonly [string, string])[] = [
  ["Yellow", "#fff2cc"],
  ["Green", "#d9ead3"],
  ["Blue", "#cfe2f3"],
  ["Pink", "#ead1dc"],
  ["Orange", "#fce5cd"],
  ["Gray", "#eeeeee"],
];
const SIZES = ["8pt", "10pt", "13pt", "16pt", "20pt", "28pt"];
const FAMILIES: readonly (readonly [string, string])[] = [
  ["Sans serif", "sans-serif"],
  ["Serif", "serif"],
  ["Monospace", "monospace"],
];

function shortcut(key: string): string {
  const mac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
  return mac ? key.replace("Ctrl+", "⌘").replace("Alt+", "⌥").replace("Shift+", "⇧") : key;
}

const STYLE_BUTTONS: readonly (readonly [TextStyle, string, string])[] = [
  ["bold", "Bold", "Ctrl+B"],
  ["italic", "Italic", "Ctrl+I"],
  ["underline", "Underline", "Ctrl+U"],
  ["strikethrough", "Strikethrough", "Ctrl+Shift+X"],
  ["superscript", "Superscript", ""],
  ["subscript", "Subscript", ""],
];

const ALIGNMENTS: readonly (readonly [Alignment, string])[] = [
  ["left", "Align left"],
  ["center", "Align centre"],
  ["right", "Align right"],
  ["justify", "Justify"],
];

type Popover = "link" | "color" | "highlight" | null;

export function EditToolbar({ view, onInsertAttachment, onInsertApp, onInsertInlineReply, onDone }: EditToolbarProps) {
  const [state, setState] = useState<EditorState>(() => view.state());
  const [popover, setPopover] = useState<Popover>(null);
  const [linkValue, setLinkValue] = useState("");
  const barRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setState(view.state());
    return view.onStateChange(setState);
  }, [view]);

  const exec = (command: EditorCommand): void => {
    setPopover(null);
    view.exec(command);
  };
  // Keeps the focus (and the selection) in the document on mouse down.
  const keep = (event: Event): void => {
    const target = event.target as HTMLElement;
    if (target.closest("input, select") === null) {
      event.preventDefault();
    }
  };
  // Roving focus between the controls: one tab stop, arrows move.
  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.key === "Escape") {
      if (popover !== null) {
        setPopover(null);
        event.preventDefault();
      } else {
        view.focus(view.selection()?.end ?? "end");
      }
      return;
    }
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight" && event.key !== "Home" && event.key !== "End") {
      return;
    }
    const target = event.target as HTMLElement;
    if (target.closest(".edit-popover") !== null || target.tagName === "SELECT") {
      return;
    }
    const items = Array.from(barRef.current?.querySelectorAll<HTMLElement>(".edit-row > .edit-control") ?? []);
    const index = items.indexOf(target);
    if (index < 0) {
      return;
    }
    event.preventDefault();
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? items.length - 1
          : (index + (event.key === "ArrowRight" ? 1 : -1) + items.length) % items.length;
    for (const item of items) {
      item.tabIndex = -1;
    }
    items[next]!.tabIndex = 0;
    items[next]!.focus();
  };

  // The first control is the bar's tab stop.
  const first = onInsertAttachment === undefined ? "bold" : "attach";
  const button = (
    name: string,
    label: string,
    onClick: () => void,
    options: { pressed?: boolean; disabled?: boolean; keys?: string; cls?: string; text?: string } = {},
  ) => {
    const IconFn = ICONS[name];
    const title = options.keys ? `${label} (${shortcut(options.keys)})` : label;
    return (
      <button
        type="button"
        class={`edit-control edit-button ${options.cls ?? ""}`}
        data-command={name}
        tabIndex={name === first ? 0 : -1}
        aria-label={label}
        title={title}
        aria-pressed={options.pressed === undefined ? undefined : options.pressed}
        disabled={options.disabled === true || !state.editable}
        onClick={onClick}
      >
        {IconFn === undefined ? null : <IconFn />}
        {options.text === undefined ? null : <span class="edit-button-text">{options.text}</span>}
      </button>
    );
  };

  const applyLink = (event: Event): void => {
    event.preventDefault();
    const value = linkValue.trim();
    if (value === "") {
      exec({ type: "removeLink" });
    } else {
      exec({ type: "setLink", href: value });
    }
  };

  const palette = (kind: "color" | "highlight") => {
    const colors = kind === "color" ? COLORS : HIGHLIGHTS;
    const current = kind === "color" ? state.color : state.backgroundColor;
    const set = (value: string | null): EditorCommand =>
      kind === "color" ? { type: "setColor", value } : { type: "setBackgroundColor", value };
    return (
      <div class="edit-popover edit-palette" role="group" aria-label={kind === "color" ? "Text colour" : "Highlight colour"}>
        {colors.map(([name, value]) => (
          <button
            type="button"
            key={value}
            class="edit-swatch"
            ref={(el) => {
              if (el !== null) {
                el.style.backgroundColor = value;
              }
            }}
            aria-label={name}
            title={name}
            aria-pressed={current === value}
            onClick={() => exec(set(value))}
          />
        ))}
        <button type="button" class="btn btn-ghost edit-swatch-none" onClick={() => exec(set(null))}>
          {kind === "color" ? "Default" : "None"}
        </button>
      </div>
    );
  };

  return (
    // biome-ignore lint: the toolbar container handles keys for its controls
    <div class="edit-toolbar" ref={barRef} onMouseDown={keep} onKeyDown={onKeyDown}>
      <div class="edit-row" role="toolbar" aria-label="Formatting">
        {onInsertAttachment === undefined ? null : (
          <>
            {button("attach", "Insert attachment", onInsertAttachment, { cls: "edit-wide" })}
            {onInsertApp === undefined ? null : button("app", "Insert app", onInsertApp)}
            <span class="edit-sep" aria-hidden="true" />
          </>
        )}
        {STYLE_BUTTONS.map(([style, label, keys]) =>
          button(style, label, () => exec({ type: "toggleStyle", style }), {
            pressed: state.styles[style],
            keys,
          }),
        )}
        <span class="edit-sep" aria-hidden="true" />
        <select
          class="edit-control edit-select"
          tabIndex={-1}
          aria-label="Paragraph style"
          disabled={!state.editable}
          value={String(state.heading)}
          onChange={(e) => exec({ type: "setHeading", level: Number(e.currentTarget.value) as 0 | 1 | 2 | 3 | 4 })}
        >
          <option value="0">Normal</option>
          <option value="1">Heading 1</option>
          <option value="2">Heading 2</option>
          <option value="3">Heading 3</option>
          <option value="4">Heading 4</option>
        </select>
        {button("bullet", "Bulleted list", () => exec({ type: "toggleList", list: "bullet" }), {
          pressed: state.list === "bullet",
          keys: "Ctrl+Shift+8",
        })}
        {button("numbered", "Numbered list", () => exec({ type: "toggleList", list: "numbered" }), {
          pressed: state.list === "numbered",
          keys: "Ctrl+Shift+7",
        })}
        {button("outdent", "Decrease indent", () => exec({ type: "outdent" }), { keys: "Shift+Tab" })}
        {button("indent", "Increase indent", () => exec({ type: "indent" }), { keys: "Tab" })}
        <span class="edit-sep" aria-hidden="true" />
        {ALIGNMENTS.map(([value, label]) =>
          button(value, label, () => exec({ type: "setAlignment", value }), {
            pressed: state.alignment === value,
            cls: "edit-align",
          }),
        )}
        <span class="edit-sep" aria-hidden="true" />
        {button(
          "link",
          state.link === null ? "Insert link" : "Edit link",
          () => {
            setLinkValue(state.link ?? "");
            setPopover(popover === "link" ? null : "link");
          },
          { pressed: state.link !== null, disabled: !state.hasRange && state.link === null },
        )}
        {state.link !== null ? button("unlink", "Remove link", () => exec({ type: "removeLink" })) : null}
        {button("color", "Text colour", () => setPopover(popover === "color" ? null : "color"), {
          pressed: popover === "color",
        })}
        {button("highlight", "Highlight colour", () => setPopover(popover === "highlight" ? null : "highlight"), {
          pressed: popover === "highlight",
        })}
        <select
          class="edit-control edit-select edit-select-narrow"
          tabIndex={-1}
          aria-label="Font size"
          disabled={!state.editable}
          value={state.fontSize ?? ""}
          onChange={(e) => exec({ type: "setFontSize", value: e.currentTarget.value === "" ? null : e.currentTarget.value })}
        >
          <option value="">Size</option>
          {SIZES.map((s) => (
            <option key={s} value={s}>
              {s.replace("pt", "")}
            </option>
          ))}
        </select>
        <select
          class="edit-control edit-select edit-select-narrow"
          tabIndex={-1}
          aria-label="Font"
          disabled={!state.editable}
          value={state.fontFamily ?? ""}
          onChange={(e) =>
            exec({ type: "setFontFamily", value: e.currentTarget.value === "" ? null : e.currentTarget.value })
          }
        >
          <option value="">Font</option>
          {FAMILIES.map(([name, value]) => (
            <option key={value} value={value}>
              {name}
            </option>
          ))}
        </select>
        {button("clear", "Clear formatting", () => exec({ type: "clearFormatting" }), { keys: "Ctrl+\\" })}
        <span class="edit-sep" aria-hidden="true" />
        {button("undo", "Undo", () => exec({ type: "undo" }), { disabled: !state.canUndo, keys: "Ctrl+Z" })}
        {button("redo", "Redo", () => exec({ type: "redo" }), { disabled: !state.canRedo, keys: "Ctrl+Shift+Z" })}
        <span class="edit-sep" aria-hidden="true" />
        {onInsertInlineReply === undefined
          ? null
          : button("inlineReply", "Inline reply at the caret", onInsertInlineReply, { cls: "edit-wide" })}
        <span class="edit-gap" />
        {onDone === undefined
          ? null
          : button("done", "Done", onDone, { cls: "edit-done", text: "Done" })}
      </div>
      {popover === "link" ? (
        <form class="edit-popover edit-link-form" onSubmit={applyLink}>
          <label class="edit-link-label">
            <span class="visually-hidden">Link address</span>
            <input
              class="input edit-link-input"
              type="text"
              inputMode="url"
              placeholder="https://… or a wave id"
              value={linkValue}
              ref={(el) => el?.focus()}
              onInput={(e) => setLinkValue(e.currentTarget.value)}
            />
          </label>
          <button type="submit" class="btn btn-primary">
            Apply
          </button>
          <button type="button" class="btn btn-ghost" onClick={() => setPopover(null)}>
            Cancel
          </button>
        </form>
      ) : null}
      {popover === "color" ? palette("color") : null}
      {popover === "highlight" ? palette("highlight") : null}
    </div>
  );
}
