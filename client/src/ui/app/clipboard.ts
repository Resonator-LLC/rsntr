/**
 * Puts `text` on the clipboard. Where `navigator.clipboard` is missing
 * (plain http from another machine), selects the contents of `selectable`
 * and copies the selection instead. Resolves to whether it worked.
 */
export async function copyText(text: string, selectable: HTMLElement | null): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Not a secure context: copy a selection instead.
  }
  if (selectable === null) {
    return false;
  }
  const selection = window.getSelection();
  const range = document.createRange();
  range.selectNodeContents(selectable);
  selection?.removeAllRanges();
  selection?.addRange(range);
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  }
}
