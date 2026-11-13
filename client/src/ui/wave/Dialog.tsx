// A modal dialog on the `<dialog>` element (`.dialog` in base.css).

import type { ComponentChildren } from "preact";
import { useEffect, useRef } from "preact/hooks";

export interface DialogProps {
  readonly title: string;
  readonly onClose: () => void;
  readonly children: ComponentChildren;
  readonly class?: string;
}

export function Dialog({ title, onClose, children, class: extra }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    const el = ref.current;
    if (el === null) {
      return;
    }
    const restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (typeof el.showModal === "function" && !el.open) {
      try {
        el.showModal();
      } catch {
        el.setAttribute("open", "");
      }
    } else if (!el.open) {
      el.setAttribute("open", "");
    }
    const onClosed = (): void => closeRef.current();
    el.addEventListener("close", onClosed);
    return () => {
      el.removeEventListener("close", onClosed);
      if (el.open && typeof el.close === "function") {
        el.close();
      }
      if (restore !== null && restore.isConnected) {
        restore.focus({ preventScroll: true });
      }
    };
  }, []);

  return (
    <dialog ref={ref} class={`dialog ${extra ?? ""}`} aria-label={title}>
      <h2 class="dialog-title">{title}</h2>
      {children}
    </dialog>
  );
}
