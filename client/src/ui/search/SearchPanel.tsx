// The search panel (`SearchPanelWidget`, `DigestDomImpl`, `SearchPresenter`
// of Apache Wave): the query box, folder shortcuts, "New wave", the digest
// list with avatars, title, snippet, time and unread count, "show more",
// and per-wave Archive / Inbox / Mute actions.
//
// Freshness: the store's 15 s poll (the original's), plus a long poll of
// `/search/changes` that refreshes soon after the server's search index
// changed; if that endpoint fails, the poll alone remains.

import { useEffect, useMemo, useRef, useState } from "preact/hooks";

import type { OpenWave, WaveClient } from "../../client";
import type { Digest } from "../../wire/search";
import { ArchiveIcon, InboxIcon, MuteIcon, PlusIcon, SearchIcon } from "../app/icons";
import { CheckAllIcon } from "../wave/icons";
import type { SearchPanelProps } from "../contracts";
import { Avatar } from "../profiles/Avatar";
import type { ProfileStore } from "../profiles/profileStore";
import { MoreIcon } from "../wave/icons";
import { Popup } from "../wave/Popup";
import { formatFullDateTime, formatPastDate } from "./dates";
import { OpenWaveDigests } from "./liveDigest";
import { DEFAULT_SEARCH, SearchStore, hasMore, resultRange } from "./searchStore";

const FOLDERS: readonly (readonly [string, string])[] = [
  ["Inbox", "in:inbox"],
  ["Archive", "in:archive"],
  ["Muted", "in:mute"],
];

/** Long-polls the search change signal; calls `changed` after each change. */
function watchChanges(changed: () => void): () => void {
  let stopped = false;
  let controller: AbortController | null = null;
  let since: string | null = null;
  let failures = 0;
  const loop = async (): Promise<void> => {
    while (!stopped) {
      controller = new AbortController();
      try {
        const query = since === null ? "" : `?since=${encodeURIComponent(since)}&timeout=30`;
        const response = await fetch(`/search/changes${query}`, {
          credentials: "same-origin",
          signal: controller.signal,
        });
        if (!response.ok) {
          throw new Error(`status ${response.status}`);
        }
        const body = (await response.json()) as { generation?: unknown; changed?: unknown };
        const generation = typeof body.generation === "string" ? body.generation : since;
        if (since !== null && generation !== since) {
          changed();
        }
        if (generation === since && body.changed !== false) {
          throw new Error("no generation");
        }
        since = generation;
        failures = 0;
      } catch {
        if (stopped) {
          return;
        }
        failures++;
        // Back off; the store's own poll keeps the list fresh meanwhile.
        await new Promise((resolve) => setTimeout(resolve, Math.min(60_000, 2000 * 2 ** failures)));
      }
    }
  };
  void loop();
  return () => {
    stopped = true;
    controller?.abort();
  };
}

/** Changes the user's folder state of a wave, opening it for a moment if needed. */
async function withWave(client: WaveClient, waveId: string, change: (wave: OpenWave) => void): Promise<void> {
  const open = client.openWaves().find((w) => w.waveId.serialise() === waveId);
  if (open !== undefined) {
    change(open);
    return;
  }
  const wave = await client.openWave(waveId);
  try {
    change(wave);
    await wave.whenSaved();
  } finally {
    wave.close();
  }
}

function folderOf(query: string): string | null {
  const q = query.trim();
  return FOLDERS.find(([, value]) => value === q)?.[1] ?? null;
}

function DigestItem({
  digest,
  profiles,
  selected,
  folder,
  onSelect,
  onAction,
  onMenu,
  menuOpen,
}: {
  digest: Digest;
  profiles: ProfileStore;
  selected: boolean;
  folder: string | null;
  onSelect: (waveId: string) => void;
  onAction: (waveId: string, action: WaveAction) => void;
  onMenu: (waveId: string, anchor: HTMLElement) => void;
  menuOpen: boolean;
}) {
  const unread = digest.unreadCount;
  const people = [digest.author, ...digest.participants].filter((p) => p !== "").slice(0, 4);
  const time = Number(digest.lastModified);
  const title = digest.title.trim() === "" ? "(untitled)" : digest.title;
  const href = `#${digest.waveId}`;
  // Swipe left in the inbox: archive (a shortcut; the menu has it too).
  const swipe = useRef<{ x: number; y: number; dx: number; sideways: boolean | null } | null>(null);
  const linkRef = useRef<HTMLAnchorElement>(null);
  const canSwipe = folder === "in:inbox";
  const slide = (dx: number): void => {
    const link = linkRef.current;
    if (link !== null) {
      link.style.transform = dx === 0 ? "" : `translateX(${dx}px)`;
    }
  };
  const onTouchStart = (event: TouchEvent): void => {
    const t = event.touches[0];
    swipe.current = canSwipe && t !== undefined && event.touches.length === 1 ? { x: t.clientX, y: t.clientY, dx: 0, sideways: null } : null;
  };
  const onTouchMove = (event: TouchEvent): void => {
    const s = swipe.current;
    const t = event.touches[0];
    if (s === null || t === undefined) {
      return;
    }
    const dx = t.clientX - s.x;
    const dy = t.clientY - s.y;
    if (s.sideways === null && Math.abs(dx) + Math.abs(dy) > 10) {
      s.sideways = Math.abs(dx) > Math.abs(dy) * 1.5;
    }
    if (s.sideways === true) {
      s.dx = Math.min(0, dx);
      slide(s.dx);
    }
  };
  const onTouchEnd = (): void => {
    const s = swipe.current;
    swipe.current = null;
    slide(0);
    if (s?.sideways === true && s.dx < -SWIPE_PX) {
      onAction(digest.waveId, "archive");
    }
  };
  return (
    <li
      class={`digest ${selected ? "is-selected" : ""} ${unread > 0 ? "is-unread" : ""}`}
      data-wave-id={digest.waveId}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onTouchCancel={onTouchEnd}
    >
      <a
        ref={linkRef}
        class="digest-link"
        href={href}
        aria-current={selected ? "true" : undefined}
        onClick={(event) => {
          if (event.button === 0 && !event.ctrlKey && !event.metaKey && !event.shiftKey) {
            event.preventDefault();
            onSelect(digest.waveId);
          }
        }}
      >
        <span class="digest-avatars" aria-hidden="true">
          {people.map((p) => (
            <Avatar key={p} profiles={profiles} address={p} size="sm" decorative />
          ))}
        </span>
        <span class="digest-main">
          <span class="digest-title">{title}</span>
          <span class="digest-snippet">{digest.snippet}</span>
        </span>
        <span class="digest-info">
          <time class="digest-time" dateTime={new Date(time).toISOString()} title={formatFullDateTime(time)}>
            {formatPastDate(time)}
          </time>
          {unread > 0 ? (
            <span class="badge-unread digest-count" title={`${unread} unread of ${digest.blipCount}`}>
              {unread}
              <span class="visually-hidden"> unread</span>
            </span>
          ) : (
            <span class="digest-count digest-count-read" title={`${digest.blipCount} messages`}>
              {digest.blipCount}
              <span class="visually-hidden"> messages</span>
            </span>
          )}
        </span>
      </a>
      <button
        type="button"
        class="icon-btn digest-more"
        aria-label={`Actions for “${title}”`}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        data-action="digest-menu"
        onClick={(event) => onMenu(digest.waveId, event.currentTarget)}
      >
        <MoreIcon />
      </button>
    </li>
  );
}

type WaveAction = "archive" | "inbox" | "mute" | "read";

/** How far a row is swiped left to archive it. */
const SWIPE_PX = 96;
/** How long the note after an action (with Undo) stays. */
const TOAST_MS = 8000;

const DONE_TEXT: Record<WaveAction, string> = {
  archive: "Archived",
  inbox: "Moved to the inbox",
  mute: "Muted",
  read: "Marked as read",
};

export function SearchPanel({ client, profiles, selectedWaveId, onSelect, onNewWave, refreshToken }: SearchPanelProps) {
  const live = useMemo(() => new OpenWaveDigests(client), [client]);
  const store = useMemo(
    () => new SearchStore({ search: (q, i, n) => client.search(q, i, n), live }),
    [client, live],
  );
  const [draft, setDraft] = useState(store.query.peek());
  const [busy, setBusy] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ waveId: string; anchor: HTMLElement } | null>(null);
  const [toast, setToast] = useState<{ text: string; undo: (() => void) | null } | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (toastTimer.current !== null) {
      clearTimeout(toastTimer.current);
    }
  }, []);
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => {
    store.start();
    const onVisibility = (): void => store.setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", onVisibility);
    const stopWatching = watchChanges(() => store.refreshSoon());
    const offConnection = client.events.on("connection", ({ state }) => {
      if (state === "connected") {
        store.refresh();
      }
    });
    return () => {
      store.stop();
      live.dispose();
      stopWatching();
      offConnection();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [store, live, client]);

  const firstToken = useRef(refreshToken);
  useEffect(() => {
    if (refreshToken !== firstToken.current) {
      store.refreshSoon();
    }
  }, [refreshToken, store]);

  const query = store.query.value;
  const folder = folderOf(query);
  const result = store.result.value;
  const status = store.status.value;
  const digests = store.digests.value;

  const submit = (event: Event): void => {
    event.preventDefault();
    const q = draft.trim() === "" ? DEFAULT_SEARCH : draft.trim();
    setDraft(q);
    store.setQuery(q);
  };
  const goFolder = (q: string): void => {
    setDraft(q);
    store.setQuery(q);
  };
  const showToast = (text: string, undo: (() => void) | null): void => {
    if (toastTimer.current !== null) {
      clearTimeout(toastTimer.current);
    }
    setToast({ text, undo });
    toastTimer.current = setTimeout(() => setToast(null), TOAST_MS);
  };
  const act = async (waveId: string, action: WaveAction, undoable = true): Promise<void> => {
    setBusy(waveId);
    try {
      await withWave(client, waveId, (wave) => {
        if (action === "read") {
          wave.markRead();
          return;
        }
        wave.writeSupplement((s) => {
          if (action === "archive") {
            s.archive();
          } else if (action === "inbox") {
            s.follow();
            s.inbox();
          } else {
            s.unfollow();
          }
        });
      });
      const undo = undoable && (action === "archive" || action === "mute") && folder === "in:inbox"
        ? () => {
            setToast(null);
            void act(waveId, "inbox", false);
          }
        : null;
      showToast(DONE_TEXT[action], undo);
    } catch (error) {
      console.error("search: the action failed", error);
      showToast("That did not work. Try again.", null);
    } finally {
      setBusy(null);
      store.refreshSoon();
    }
  };

  // Up/Down move between digests (the original's list keys), Home/End to
  // the first and last.
  const onListKey = (event: KeyboardEvent): void => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key) || event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    const links = Array.from(listRef.current?.querySelectorAll<HTMLElement>(".digest-link") ?? []);
    const row = (document.activeElement as HTMLElement | null)?.closest(".digest");
    const index = links.findIndex((l) => l.closest(".digest") === row);
    const to =
      event.key === "Home" ? 0 : event.key === "End" ? links.length - 1 : index + (event.key === "ArrowDown" ? 1 : -1);
    const next = links[Math.max(0, Math.min(links.length - 1, to))];
    if (next !== undefined) {
      event.preventDefault();
      next.focus();
    }
  };

  return (
    <section class="search" aria-labelledby="search-heading">
      <h2 class="visually-hidden" id="search-heading">Waves</h2>
      <div class="search-head">
        <form class="search-form" role="search" onSubmit={submit}>
          <label class="search-box">
            <SearchIcon />
            <span class="visually-hidden">Search waves</span>
            <input
              class="search-input"
              type="search"
              value={draft}
              placeholder="Search waves"
              enterKeyHint="search"
              autoComplete="off"
              spellcheck={false}
              onInput={(e) => setDraft(e.currentTarget.value)}
            />
          </label>
        </form>
        <button type="button" class="btn btn-primary search-new" data-action="new-wave" onClick={onNewWave}>
          <PlusIcon />
          <span>New wave</span>
        </button>
      </div>
      <nav class="search-folders" aria-label="Folders">
        {FOLDERS.map(([name, value]) => (
          <button
            type="button"
            key={value}
            class="search-folder"
            aria-pressed={query.trim() === value}
            onClick={() => goFolder(value)}
          >
            {name}
          </button>
        ))}
        <span class="search-range" role="status" aria-live="polite">
          {resultRange(result, status)}
        </span>
      </nav>
      {store.failed.value ? (
        <div class="notice notice-error search-failed" role="alert">
          <span>The search failed.</span>
          <button type="button" class="btn btn-ghost" onClick={() => store.refresh()}>
            Retry
          </button>
        </div>
      ) : null}
      {/* biome-ignore lint: arrow keys between the links of the list */}
      <ul class="search-list" ref={listRef} onKeyDown={onListKey} aria-busy={status === "searching"}>
        {digests.map((digest) => (
          <DigestItem
            key={digest.waveId}
            digest={digest}
            profiles={profiles}
            selected={digest.waveId === selectedWaveId}
            folder={folder}
            onSelect={onSelect}
            onAction={(id, action) => {
              if (busy === null) {
                void act(id, action);
              }
            }}
            menuOpen={menu?.waveId === digest.waveId}
            onMenu={(waveId, anchor) => setMenu(menu?.waveId === waveId ? null : { waveId, anchor })}
          />
        ))}
      </ul>
      {status === "ready" && digests.length === 0 ? (
        <p class="search-empty">
          {folder === "in:inbox" ? "Your inbox is empty. Start a new wave!" : "No waves match this search."}
        </p>
      ) : null}
      {hasMore(result) && digests.length > 0 ? (
        <button
          type="button"
          class="btn btn-ghost search-more"
          disabled={store.loadingMore.value}
          onClick={() => store.showMore()}
        >
          {store.loadingMore.value ? "Loading..." : "Show more"}
        </button>
      ) : null}
      {menu !== null ? <DigestMenu
        digest={digests.find((d) => d.waveId === menu.waveId) ?? null}
        anchor={menu.anchor}
        folder={folder}
        onClose={() => setMenu(null)}
        onAction={(action) => {
          const waveId = menu.waveId;
          setMenu(null);
          menu.anchor.focus();
          if (busy === null) {
            void act(waveId, action);
          }
        }}
      /> : null}
      <div class="search-toast-region" role="status">
        {toast !== null ? (
          <div class="toast">
            <span>{toast.text}</span>
            {toast.undo !== null ? (
              <button type="button" class="btn btn-ghost toast-action" data-action="undo" onClick={toast.undo}>
                Undo
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
    </section>
  );
}

/** The actions of one wave in the list: a menu at its "..." button. */
function DigestMenu({
  digest,
  anchor,
  folder,
  onClose,
  onAction,
}: {
  digest: Digest | null;
  anchor: HTMLElement;
  folder: string | null;
  onClose: () => void;
  onAction: (action: WaveAction) => void;
}) {
  if (digest === null) {
    return null;
  }
  return (
    <Popup anchor={anchor} onClose={onClose} role="menu" label="Wave actions">
      {folder === "in:inbox" ? (
        <button type="button" class="menu-item" role="menuitem" data-action="archive" onClick={() => onAction("archive")}>
          <ArchiveIcon />
          Archive
        </button>
      ) : (
        <button type="button" class="menu-item" role="menuitem" data-action="inbox" onClick={() => onAction("inbox")}>
          <InboxIcon />
          Move to inbox
        </button>
      )}
      {folder !== "in:mute" ? (
        <button type="button" class="menu-item" role="menuitem" data-action="mute" onClick={() => onAction("mute")}>
          <MuteIcon />
          Mute
        </button>
      ) : null}
      {digest.unreadCount > 0 ? (
        <button type="button" class="menu-item" role="menuitem" data-action="read" onClick={() => onAction("read")}>
          <CheckAllIcon />
          Mark as read
        </button>
      ) : null}
    </Popup>
  );
}
