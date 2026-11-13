// The icons of the shell and the search panel: simple strokes on a 24 unit
// grid, drawn here (class `icon` of base.css gives size and stroke).

import type { ComponentChildren } from "preact";

function Icon({ children }: { children: ComponentChildren }) {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

export function PlusIcon() {
  return (
    <Icon>
      <path d="M12 5v14M5 12h14" />
    </Icon>
  );
}

export function SearchIcon() {
  return (
    <Icon>
      <circle cx="11" cy="11" r="6.5" />
      <path d="M16 16l4.5 4.5" />
    </Icon>
  );
}

export function RefreshIcon() {
  return (
    <Icon>
      <path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" />
      <path d="M19.5 4.5v4h-4" />
    </Icon>
  );
}

export function MoreIcon() {
  return (
    <Icon>
      <circle cx="12" cy="5.5" r="0.9" />
      <circle cx="12" cy="12" r="0.9" />
      <circle cx="12" cy="18.5" r="0.9" />
    </Icon>
  );
}

export function SignOutIcon() {
  return (
    <Icon>
      <path d="M14 4.5H6.5v15H14" />
      <path d="M10.5 12h9.5M16.5 8.5L20 12l-3.5 3.5" />
    </Icon>
  );
}

export function ArchiveIcon() {
  return (
    <Icon>
      <path d="M4 5h16v4H4zM5.5 9v10h13V9M10 13h4" />
    </Icon>
  );
}

export function InboxIcon() {
  return (
    <Icon>
      <path d="M4 13.5l2.5-8h11l2.5 8v5.5H4z" />
      <path d="M4 13.5h4.5l1 2.5h5l1-2.5H20" />
    </Icon>
  );
}

export function CheckIcon() {
  return (
    <Icon>
      <path d="M5 12.5l4.5 4.5L19 7.5" />
    </Icon>
  );
}

export function MuteIcon() {
  return (
    <Icon>
      <path d="M6 9.5v5h3l4.5 4v-13L9 9.5zM17 9.5l4 5M21 9.5l-4 5" />
    </Icon>
  );
}

/** The mark of the product: three waves. */
export function SettingsIcon() {
  return (
    <Icon>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1" />
    </Icon>
  );
}

export function WaveMark() {
  return (
    <svg class="wave-mark" viewBox="0 0 32 32" aria-hidden="true" focusable="false">
      <path d="M4 11c4-5 8-5 12 0s8 5 12 0" />
      <path d="M4 17c4-5 8-5 12 0s8 5 12 0" />
      <path d="M4 23c4-5 8-5 12 0s8 5 12 0" />
    </svg>
  );
}
