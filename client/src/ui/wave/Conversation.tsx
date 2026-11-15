// The threads and blips of a conversation.
//
// The structure follows the view tree of the Apache Wave web client
// (`org.waveprotocol.wave.client.wavepanel.view`: a root thread of blips
// with a reply box; each blip a meta bar and its document, in which the
// inline reply threads sit at their anchors, and below it the reply
// threads without an anchor; every reply thread collapsible, with a count
// of its blips, and a continuation indicator at its end).
//
// Accessibility: threads are lists and blips are list items holding an
// `article` (an article nested in another is a reply to it, as ARIA
// defines). A tree was not chosen because tree items make screen readers
// treat their content as a flat label, and blips hold rich, editable text
// with links and further threads. The article of the focused blip is the
// one tab stop of the conversation (roving tabindex).

import { memo } from "preact/compat";
import { createPortal } from "preact/compat";
import { useContext, useEffect, useLayoutEffect, useRef } from "preact/hooks";

import { Avatar } from "../profiles/Avatar";
import { firstName } from "../profiles/profileStore";
import type { AnchorHost, AppHost, BlipView } from "../editor/api";
import { AppView } from "../waveapps/AppView";
import { type WaveUi, WaveUiContext } from "./context";
import { formatFullDate, formatPastDate } from "./dates";
import { ChevronIcon, EditIcon, MoreIcon, PlusIcon, ReplyIcon } from "./icons";
import type { BlipVM, ConversationStore } from "./store";
import { ROOT_THREAD } from "./traversal";

/** `UndercurrentShallowBlipRenderer.MAX_CONTRIBUTORS`. */
const MAX_CONTRIBUTORS = 3;

function useUi(): WaveUi {
  const ui = useContext(WaveUiContext);
  if (ui === null) {
    throw new Error("wave panel component used outside a wave");
  }
  return ui;
}

/** Scrolls a blip into view; repeated while the layout around it settles. */
function scrollIntoView(element: HTMLElement): void {
  let frames = 0;
  const step = (): void => {
    if (!element.isConnected) {
      return;
    }
    element.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "auto" });
    if (++frames < 3 && typeof requestAnimationFrame === "function") {
      requestAnimationFrame(step);
    }
  };
  step();
}

interface BlipHeader {
  readonly avatar: string | null;
  readonly names: string;
  readonly time: number;
}

/** Who and when, as `UndercurrentShallowBlipRenderer` shows it. */
function blipHeader(ui: WaveUi, blipId: string): BlipHeader {
  const meta = ui.store.blipMeta(blipId);
  if (meta === null) {
    return { avatar: null, names: "anon", time: 0 };
  }
  const contributors = meta.contributors.length > 0 ? meta.contributors : [meta.author];
  // One author: the whole name they chose; several: first names.
  const names =
    contributors.length === 1
      ? ui.profiles.get(contributors[0]!).name
      : contributors
          .slice(0, MAX_CONTRIBUTORS)
          .map((address) => firstName(ui.profiles.get(address).name))
          .join(", ");
  return { avatar: contributors[0] ?? null, names: names === "" ? "anon" : names, time: meta.lastModifiedTime };
}

export const BlipItem = memo(function BlipItem({ id }: { id: string }) {
  const ui = useUi();
  const store = ui.store;
  const vm = store.blips.get(id);
  const mainRef = useRef<HTMLElement>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<BlipView | null>(null);

  const activate = (): void => {
    const host = hostRef.current;
    if (vm === undefined || host === null || viewRef.current !== null) {
      return;
    }
    const anchors: AnchorHost = {
      attach(threadId, container) {
        const next = new Map(vm.anchors.peek());
        next.set(threadId, container);
        vm.anchors.value = next;
      },
      detach(threadId, container) {
        if (vm.anchors.peek().get(threadId) === container) {
          const next = new Map(vm.anchors.peek());
          next.delete(threadId);
          vm.anchors.value = next;
        }
      },
    };
    const apps: AppHost = {
      attach(gadget, container) {
        const next = new Map(vm.apps.peek());
        next.set(gadget.id, { gadget, container });
        vm.apps.value = next;
      },
      detach(gadgetId, container) {
        if (vm.apps.peek().get(gadgetId)?.container === container) {
          const next = new Map(vm.apps.peek());
          next.delete(gadgetId);
          vm.apps.value = next;
        }
      },
    };
    const view = ui.createView({
      host,
      doc: store.documentFor(id),
      anchors,
      ...(ui.apps !== null ? { apps } : {}),
      ...(ui.attachments !== null ? { attachments: ui.attachments } : {}),
      waveLinkHref: ui.waveLinkHref,
      onKeyDown: ui.onEditorKeyDown,
      ariaLabel: "Message text",
    });
    viewRef.current = view;
    store.views.set(id, view);
    view.onLocalEdit(() => store.onLocalEdit(id));
    vm.viewReady.value = true;
    if (vm.editing.peek()) {
      view.setEditable(true);
      view.focus();
      store.editingView.value = view;
    }
  };

  // The document is rendered when the blip nears the viewport, in idle
  // time, or at once when it is needed.
  useEffect(() => {
    const main = mainRef.current;
    if (vm === undefined || main === null) {
      return;
    }
    const unregister = ui.lazy.register(main, activate);
    return () => {
      unregister();
      const view = viewRef.current;
      if (view !== null) {
        viewRef.current = null;
        if (store.views.get(id) === view) {
          store.views.delete(id);
        }
        if (store.editingView.peek() === view) {
          store.editingView.value = null;
        }
        view.destroy();
        vm.viewReady.value = false;
        vm.anchors.value = new Map();
        vm.apps.value = new Map();
      }
    };
  }, [vm]);

  // Layout effects: the editor of a new reply takes focus in the same task
  // as the click or key that made it, so no typed character is lost.
  const needView = vm?.needView.value ?? false;
  useLayoutEffect(() => {
    if (needView) {
      activate();
    }
  }, [needView]);

  const focusRequest = vm?.focusRequest.value ?? null;
  useEffect(() => {
    const main = mainRef.current;
    if (vm === undefined || focusRequest === null || main === null) {
      return;
    }
    vm.focusRequest.value = null;
    if (focusRequest.dom && !vm.editing.peek()) {
      const active = document.activeElement;
      // Focus that is already on something of this blip (a link, its menu) stays.
      if (active === null || active.closest("[data-blip-id]") !== main) {
        main.focus({ preventScroll: true });
      }
    }
    if (focusRequest.scroll) {
      scrollIntoView(main);
    }
  }, [focusRequest]);

  const editing = vm?.editing.value ?? false;
  useLayoutEffect(() => {
    const view = viewRef.current;
    if (view === null) {
      return;
    }
    if (editing) {
      view.setEditable(true);
      view.focus();
      store.editingView.value = view;
    } else if (view.editable) {
      view.setEditable(false);
    }
  }, [editing]);

  if (vm === undefined) {
    return null;
  }
  // Subscriptions of this blip alone.
  vm.meta.value;
  const focused = vm.focused.value;
  const unread = vm.unread.value;
  const replies = vm.replies.value;
  const anchors = vm.anchors.value;
  const appContainers = vm.apps.value;
  const viewReady = vm.viewReady.value;
  // Without a focus frame, the root blip is the tab stop of the conversation.
  const tabStop = focused || (store.isRootBlip(id) && store.focusedId.value === null);

  const header = blipHeader(ui, id);
  const now = Date.now();
  const time = header.time === 0 ? now : header.time;
  const timeText = formatPastDate(time, now);
  // Author, time, unread, and how many replies, for screen readers.
  const replyCount = replies.reduce((n, r) => n + (store.threads.get(r.threadId)?.total.value ?? 0), 0);
  const repliesText = replyCount === 0 ? "" : `, ${replyCount} ${replyCount === 1 ? "reply" : "replies"}`;
  const label = `${header.names}, ${formatFullDate(time)}${unread ? ", unread" : ""}${repliesText}`;

  const below: string[] = [];
  const portals = [];
  for (const reply of replies) {
    const container = anchors.get(reply.threadId);
    if (container !== undefined) {
      portals.push(createPortal(<ThreadItem key={reply.threadId} id={reply.threadId} placement="inline" />, container));
    } else if (!reply.anchored || viewReady) {
      below.push(reply.threadId);
    }
  }

  if (ui.apps !== null) {
    for (const { gadget, container } of appContainers.values()) {
      portals.push(createPortal(<AppView key={`${gadget.id} ${gadget.app}`} gadget={gadget} apps={ui.apps} />, container));
    }
  }

  const classes = ["blip-main"];
  if (focused) {
    classes.push("blip-focused");
  }
  if (editing) {
    classes.push("blip-editing");
  }
  if (unread) {
    classes.push("blip-unread");
  }

  const openMenu = (event: MouseEvent): void => {
    const anchor = event.currentTarget as HTMLElement;
    ui.menu.value = ui.menu.peek()?.blipId === id ? null : { blipId: id, anchor };
  };

  return (
    <div class="blip" role="listitem">
      <article
        ref={mainRef}
        class={classes.join(" ")}
        data-blip-id={id}
        tabIndex={tabStop ? 0 : -1}
        aria-label={label}
        aria-current={focused ? "true" : undefined}
      >
        <header class="blip-meta">
          {header.avatar !== null ? (
            <Avatar profiles={ui.profiles} address={header.avatar} decorative class="blip-avatar" />
          ) : (
            <span class="avatar blip-avatar" />
          )}
          <span class="blip-names">{header.names}</span>
          {unread ? (
            <span class="blip-unread-mark">
              <span class="blip-unread-dot" aria-hidden="true" />
              Unread
            </span>
          ) : null}
          <time class="blip-time" dateTime={new Date(time).toISOString()} title={formatFullDate(time)}>
            {timeText}
          </time>
          {editing ? (
            <button type="button" class="btn btn-primary blip-done" onClick={() => store.stopEditing()}>
              Done
            </button>
          ) : null}
          <button
            type="button"
            class="icon-btn blip-menu-btn"
            aria-label="Message actions"
            aria-haspopup="menu"
            tabIndex={focused ? 0 : -1}
            onClick={openMenu}
          >
            <MoreIcon />
          </button>
        </header>
        <div ref={hostRef} class="blip-content" />
        {portals}
        {editing ? null : (
          // Always visible, so they are found; in the tab order of the
          // focused message only, so Tab does not stop at every message.
          <div class="blip-actions">
            <button type="button" class="blip-action" data-action="reply" tabIndex={focused ? 0 : -1} onClick={() => store.reply(id)}>
              <ReplyIcon />
              Reply
            </button>
            <button type="button" class="blip-action" data-action="edit" tabIndex={focused ? 0 : -1} onClick={() => store.startEditing(id)}>
              <EditIcon />
              Edit
            </button>
          </div>
        )}
      </article>
      {below.length > 0 ? (
        <div class="blip-replies">
          {below.map((threadId) => (
            <ThreadItem key={threadId} id={threadId} placement="below" />
          ))}
        </div>
      ) : null}
    </div>
  );
});

/** The toggle of a reply thread: its blip count and how many are unread (`CollapsibleBuilder`). */
function ThreadToggle({ store, id }: { store: ConversationStore; id: string }) {
  const vm = store.threads.get(id);
  if (vm === undefined) {
    return null;
  }
  const collapsed = vm.collapsed.value;
  const total = vm.total.value;
  const unread = vm.unread.value;
  const what = `${total} ${total === 1 ? "reply" : "replies"}${unread > 0 ? `, ${unread} unread` : ""}`;
  return (
    <button
      type="button"
      class={`thread-toggle${unread > 0 ? " thread-toggle-unread" : ""}`}
      aria-expanded={!collapsed}
      aria-label={`${collapsed ? "Expand" : "Collapse"} thread: ${what}`}
      title={collapsed ? "Expand thread" : "Collapse thread"}
      data-thread-toggle={id}
      onClick={() => store.toggleCollapsed(id)}
    >
      <ChevronIcon />
      <span class="thread-count">{total}</span>
      {unread > 0 ? <span class="thread-count-unread">({unread})</span> : null}
    </button>
  );
}

/**
 * The reply box of the root thread ("Click here to reply") or the
 * continuation indicator of a reply thread. Hidden while the last blip of
 * the thread is being edited (`ReplyIndicatorController`).
 */
function ReplyIndicator({ store, id }: { store: ConversationStore; id: string }) {
  const vm = store.threads.get(id);
  if (vm === undefined) {
    return null;
  }
  const blips = vm.blipIds.value;
  const editing = store.editingId.value;
  if (editing !== null && blips[blips.length - 1] === editing) {
    return null;
  }
  // A list item of its own, as a list holds only list items (WCAG 1.3.1).
  if (id === ROOT_THREAD) {
    return (
      <div class="thread-end" role="listitem">
        <button type="button" class="reply-box" onClick={() => store.continueThread(id)}>
          Click here to reply
        </button>
      </div>
    );
  }
  return (
    <div class="thread-end" role="listitem">
      <button
        type="button"
        class="continuation"
        aria-label="Continue this thread"
        title="Continue this thread"
        onClick={() => store.continueThread(id)}
      >
        <PlusIcon />
        <span class="continuation-bar" />
      </button>
    </div>
  );
}

export interface ThreadItemProps {
  readonly id: string;
  /** Where the thread is shown: the conversation itself, at an anchor in a blip, or below a blip. */
  readonly placement: "root" | "inline" | "below";
}

export const ThreadItem = memo(function ThreadItem({ id, placement }: ThreadItemProps) {
  const ui = useUi();
  const store = ui.store;
  const vm = store.threads.get(id);
  if (vm === undefined) {
    return null;
  }
  const blipIds = vm.blipIds.value;
  const blips = blipIds.map((blipId) => <BlipItem key={blipId} id={blipId} />);
  if (placement === "root") {
    return (
      <div class="thread thread-root" role="list" aria-label="Messages">
        {blips}
        <ReplyIndicator store={store} id={id} />
      </div>
    );
  }
  if (blipIds.length === 0) {
    return null;
  }
  const collapsed = vm.collapsed.value;
  return (
    <div
      class={`thread thread-reply thread-${placement}${collapsed ? " thread-collapsed" : ""}`}
      data-thread-id={id}
    >
      <ThreadToggle store={store} id={id} />
      {collapsed ? null : (
        <div class="thread-body" role="list" aria-label="Replies">
          {blips}
          <ReplyIndicator store={store} id={id} />
        </div>
      )}
    </div>
  );
});

export type { BlipVM };
