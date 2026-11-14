// Keys typed before an editor exists. "New wave" waits for the server
// before its editor appears; what is typed meanwhile is held here and
// typed into the editor once it takes focus, so nothing is lost.

/** How long held keys wait for an editor before they are dropped. */
const LIMIT_MS = 15_000;

/**
 * Starts holding typed text until an editable document takes focus.
 * Returns a function that stops holding and drops the text (on failure).
 */
export function holdTyping(doc: Document = document): () => void {
  const held: string[] = [];
  let done = false;

  const onKey = (e: KeyboardEvent): void => {
    if (e.ctrlKey || e.metaKey || e.altKey || e.isComposing) {
      return;
    }
    const target = e.target as Element | null;
    if (target !== null && target.closest("input, textarea, [contenteditable=true]") !== null) {
      return;
    }
    if (e.key.length === 1) {
      held.push(e.key);
    } else if (e.key === "Enter" && !e.shiftKey) {
      held.push("\n");
    } else if (e.key === "Backspace" && held.length > 0) {
      held.pop();
    } else {
      return;
    }
    e.preventDefault();
    e.stopPropagation();
  };

  const onFocus = (e: FocusEvent): void => {
    const target = e.target as HTMLElement | null;
    if (target === null || target.getAttribute("contenteditable") !== "true") {
      return;
    }
    stop();
    // After the editor has placed its caret, as the input events through
    // which the editor receives typing.
    queueMicrotask(() => {
      if (doc.activeElement !== target) {
        return;
      }
      for (const text of held.join("").split(/(\n)/)) {
        if (text === "") {
          continue;
        }
        const init: InputEventInit =
          text === "\n" ? { inputType: "insertParagraph" } : { inputType: "insertText", data: text };
        target.dispatchEvent(new InputEvent("beforeinput", { ...init, bubbles: true, cancelable: true }));
      }
    });
  };

  const timer = setTimeout(() => stop(), LIMIT_MS);
  function stop(): void {
    if (done) {
      return;
    }
    done = true;
    clearTimeout(timer);
    doc.removeEventListener("keydown", onKey, true);
    doc.removeEventListener("focusin", onFocus, true);
  }

  doc.addEventListener("keydown", onKey, true);
  doc.addEventListener("focusin", onFocus, true);
  return () => {
    held.length = 0;
    stop();
  };
}
