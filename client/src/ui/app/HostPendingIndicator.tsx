// "Waiting for host": changes the server keeps because the host of their
// wave (another server, or another device of the person) cannot be
// reached now. They stay unsaved, without an error, and are sent when the
// host is back.

import type { Signal } from "@preact/signals";

export const AWAITING_HOST_TEXT = "Waiting for host";

const STYLE = {
  display: "inline-flex",
  alignItems: "center",
  gap: "0.35em",
  fontSize: "0.85em",
  opacity: 0.8,
  whiteSpace: "nowrap",
} as const;

export function HostPendingIndicator({ count }: { count: Signal<number> }) {
  const waiting = count.value;
  if (waiting <= 0) {
    return null;
  }
  return (
    <span
      class="host-pending"
      role="status"
      style={STYLE}
      data-testid="host-pending"
      title={`${waiting} ${waiting === 1 ? "change waits" : "changes wait"} until the wave's host can be reached; nothing is lost`}
    >
      <span class="topbar-net-dot" aria-hidden="true" />
      {AWAITING_HOST_TEXT}
    </span>
  );
}
