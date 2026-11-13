// A popup anchored to an element: menus and the participant card. It is
// shown in the top layer where the browser has the popover API, so that
// nothing clips it, and positioned by hand next to its anchor. Escape, a
// press outside, scrolling or resizing close it; the keyboard focus goes
// into it when it opens and back to the anchor when it closes.

import type { ComponentChildren } from "preact";
import { useEffect, useLayoutEffect, useRef } from "preact/hooks";

export interface PopupProps {
  readonly anchor: HTMLElement;
  readonly onClose: () => void;
  readonly role: "menu" | "dialog";
  readonly label: string;
  readonly class?: string;
  readonly children: ComponentChildren;
}

const GAP = 4;
const EDGE = 8;

function focusables(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>("button:not(:disabled), a[href], input, [tabindex='0']")];
}

export function Popup({ anchor, onClose, role, label, class: extra, children }: PopupProps) {
  const ref = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useLayoutEffect(() => {
    const el = ref.current;
    if (el === null) {
      return;
    }
    const popover = el as HTMLDivElement & { showPopover?: () => void; hidePopover?: () => void };
    if (typeof popover.showPopover === "function") {
      try {
        popover.showPopover();
      } catch {
        // Shown in place.
      }
    }
    const rect = anchor.getBoundingClientRect();
    const width = el.offsetWidth;
    const height = el.offsetHeight;
    const viewportWidth = document.documentElement.clientWidth;
    const viewportHeight = document.documentElement.clientHeight;
    let left = rect.right - width;
    if (left < EDGE) {
      left = Math.min(rect.left, viewportWidth - width - EDGE);
    }
    left = Math.max(EDGE, left);
    let top = rect.bottom + GAP;
    if (top + height > viewportHeight - EDGE && rect.top - GAP - height >= EDGE) {
      top = rect.top - GAP - height;
    }
    el.style.setProperty("left", `${Math.round(left)}px`);
    el.style.setProperty("top", `${Math.round(Math.max(EDGE, top))}px`);
    (focusables(el)[0] ?? el).focus({ preventScroll: true });
    return () => {
      if (typeof popover.hidePopover === "function") {
        try {
          popover.hidePopover();
        } catch {
          // Not shown as a popover.
        }
      }
    };
  }, [anchor]);

  useEffect(() => {
    const el = ref.current;
    const close = (): void => closeRef.current();
    const onPointerDown = (event: Event): void => {
      const target = event.target as Node | null;
      if (el !== null && target !== null && !el.contains(target) && !anchor.contains(target)) {
        close();
      }
    };
    const onScroll = (event: Event): void => {
      if (el !== null && event.target instanceof Node && el.contains(event.target)) {
        return;
      }
      close();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", close);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", close);
    };
  }, [anchor]);

  const onKeyDown = (event: KeyboardEvent): void => {
    const el = ref.current;
    if (el === null) {
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose();
      anchor.focus({ preventScroll: true });
      return;
    }
    if (event.key === "Tab") {
      // A popup is left by closing it.
      event.preventDefault();
      onClose();
      anchor.focus({ preventScroll: true });
      return;
    }
    if (role !== "menu") {
      return;
    }
    const items = focusables(el);
    const index = items.indexOf(document.activeElement as HTMLElement);
    let next = -1;
    if (event.key === "ArrowDown") {
      next = (index + 1) % items.length;
    } else if (event.key === "ArrowUp") {
      next = (index - 1 + items.length) % items.length;
    } else if (event.key === "Home") {
      next = 0;
    } else if (event.key === "End") {
      next = items.length - 1;
    }
    if (next >= 0) {
      event.preventDefault();
      event.stopPropagation();
      items[next]?.focus();
    }
  };

  return (
    <div
      ref={ref}
      class={`wave-popup ${role === "menu" ? "menu" : "wave-card"} ${extra ?? ""}`}
      role={role}
      aria-label={label}
      tabIndex={-1}
      popover="manual"
      onKeyDown={onKeyDown}
    >
      {children}
    </div>
  );
}
