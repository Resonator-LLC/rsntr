// The keyboard shortcuts, opened with "?" or from Settings. The keys work
// where they are listed: in the open wave, in the wave list, or while
// editing a message (WCAG 2.1.4: none is a single key that acts anywhere).

import { Dialog } from "../wave/Dialog";

const MOD = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform) ? "Cmd" : "Ctrl";

const GROUPS: readonly { title: string; keys: readonly [string, string][] }[] = [
  {
    title: "In a wave",
    keys: [
      ["Up / Down", "Previous / next message"],
      ["Space", "Next unread message"],
      ["Shift+Space", "Previous message"],
      ["Enter", "Reply to the message"],
      ["Shift+Enter", "Write at the end of the thread"],
      ["Ctrl+E", "Edit the message"],
      ["Shift+Delete", "Delete the message"],
    ],
  },
  {
    title: "In the wave list",
    keys: [
      ["Up / Down", "Previous / next wave"],
      ["Home / End", "First / last wave"],
      ["Enter", "Open the wave"],
    ],
  },
  {
    title: "While writing",
    keys: [
      ["Shift+Enter or Escape", "Done"],
      [`${MOD}+B / I / U`, "Bold / italic / underline"],
      [`${MOD}+Shift+X`, "Strikethrough"],
      [`${MOD}+Shift+8 / 7`, "Bulleted / numbered list"],
      ["Tab / Shift+Tab", "Indent a list item / leave the editor"],
      [`${MOD}+Z / ${MOD}+Shift+Z`, "Undo / redo"],
      [`${MOD}+Shift+V`, "Paste as plain text"],
    ],
  },
  {
    title: "Anywhere",
    keys: [["?", "This list"]],
  },
];

export function ShortcutsDialog({ onClose }: { readonly onClose: () => void }) {
  return (
    <Dialog title="Keyboard shortcuts" onClose={onClose} class="shortcuts-dialog">
      {/* It scrolls on small screens: reachable by keyboard (WCAG 2.1.1). */}
      <div class="dialog-body" tabIndex={0} role="region" aria-label="Shortcuts">
        {GROUPS.map((group) => (
          <section key={group.title} class="shortcuts-group" aria-label={group.title}>
            <h3 class="shortcuts-title">{group.title}</h3>
            <dl class="shortcuts-list">
              {group.keys.map(([keys, what]) => (
                <div key={keys} class="shortcuts-row">
                  <dt>
                    <kbd>{keys}</kbd>
                  </dt>
                  <dd>{what}</dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </div>
      <div class="dialog-actions">
        <button type="button" class="btn btn-primary" data-action="close-shortcuts" onClick={onClose} autoFocus>
          Close
        </button>
      </div>
    </Dialog>
  );
}

/** Whether "?" there means a typed character, not the shortcut. */
export function typingIn(target: EventTarget | null): boolean {
  return (
    target instanceof Element &&
    target.closest("input, textarea, select, [contenteditable='true'], [contenteditable=''], [role='textbox'], dialog") !== null
  );
}
