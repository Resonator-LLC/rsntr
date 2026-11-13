// The icons of the wave panel: simple line drawings (see `.icon` in base.css).

import type { ComponentChildren } from "preact";

function Icon({ children }: { children: ComponentChildren }) {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      {children}
    </svg>
  );
}

export const BackIcon = () => (
  <Icon>
    <path d="M15 5l-7 7 7 7" />
  </Icon>
);

export const MoreIcon = () => (
  <Icon>
    <circle cx="5" cy="12" r="1.2" />
    <circle cx="12" cy="12" r="1.2" />
    <circle cx="19" cy="12" r="1.2" />
  </Icon>
);

export const ReplyIcon = () => (
  <Icon>
    <path d="M10 8L5 13l5 5" />
    <path d="M5 13h9a5 5 0 0 0 5-5V6" />
  </Icon>
);

export const EditIcon = () => (
  <Icon>
    <path d="M4 20l1-4L16 5l3 3L8 19z" />
    <path d="M14 7l3 3" />
  </Icon>
);

export const DoneIcon = () => (
  <Icon>
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  </Icon>
);

export const DeleteIcon = () => (
  <Icon>
    <path d="M5 7h14M10 7V5h4v2M7 7l1 12h8l1-12" />
  </Icon>
);

export const LinkIcon = () => (
  <Icon>
    <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" />
    <path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" />
  </Icon>
);

export const PlusIcon = () => (
  <Icon>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);

export const CloseIcon = () => (
  <Icon>
    <path d="M6 6l12 12M18 6L6 18" />
  </Icon>
);

export const ChevronIcon = () => (
  <Icon>
    <path d="M9 6l6 6-6 6" />
  </Icon>
);

export const AttachIcon = () => (
  <Icon>
    <path d="M19 11l-7.5 7.5a4.2 4.2 0 0 1-6-6L13 5a2.8 2.8 0 0 1 4 4l-7.5 7.5a1.4 1.4 0 0 1-2-2L14 8" />
  </Icon>
);

export const InlineReplyIcon = () => (
  <Icon>
    <path d="M4 5h16v10H10l-4 4v-4H4z" />
    <path d="M12 8v4M10 10h4" />
  </Icon>
);

export const ArchiveIcon = () => (
  <Icon>
    <path d="M4 5h16v4H4zM6 9v10h12V9M10 13h4" />
  </Icon>
);

export const InboxIcon = () => (
  <Icon>
    <path d="M4 13l3-8h10l3 8v6H4z" />
    <path d="M4 13h5a3 3 0 0 0 6 0h5" />
  </Icon>
);

export const UpIcon = () => (
  <Icon>
    <path d="M6 15l6-6 6 6" />
  </Icon>
);

export const DownIcon = () => (
  <Icon>
    <path d="M6 9l6 6 6-6" />
  </Icon>
);

export const UnreadIcon = () => (
  <Icon>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 3v3M12 18v3" />
  </Icon>
);

export const ClockIcon = () => (
  <Icon>
    <circle cx="12" cy="12" r="8" />
    <path d="M12 8v4l3 2" />
  </Icon>
);

export const CheckAllIcon = () => (
  <Icon>
    <path d="M3 13l4 4 8-9M12 16l1 1 8-9" />
  </Icon>
);

export const CopyIcon = () => (
  <Icon>
    <path d="M9 9h10v11H9z" />
    <path d="M15 9V4H5v11h4" />
  </Icon>
);
