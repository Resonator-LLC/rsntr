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
import type { SearchPanelProps } from "../contracts";
import { Avatar } from "../profiles/Avatar";
import type { ProfileStore } from "../profiles/profileStore";
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
}: {
  digest: Digest;
  profiles: ProfileStore;
  selected: boolean;
  folder: string | null;
  onSelect: (waveId: string) => void;
  onAction: (waveId: string, action: "archive" | "inbox" | "mute") => void;
}) {
  const unread = digest.unreadCount;
  const people = [digest.author, ...digest.participants].filter((p) => p !== "").slice(0, 4);
  const time = Number(digest.lastModified);
  const title = digest.title.trim() === "" ? "(untitled)" : digest.title;
  const href = `#${digest.waveId}`;
  return (
    <li class={`digest ${selected ? "is-selected" : ""} ${unread > 0 ? "is-unread" : ""}`} data-wave-id={digest.waveId}>
      <a
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
      <span class="digest-actions">
        {folder === "in:inbox" ? (
          <button type="button" class="icon-btn" aria-label={`Archive “${title}”`} title="Archive" data-action="archive" onClick={() => onAction(digest.waveId, "archive")}>
            <ArchiveIcon />
          </button>
        ) : (
          <button type="button" class="icon-btn" aria-label={`Move “${title}” to the inbox`} title="Move to inbox" data-action="inbox" onClick={() => onAction(digest.waveId, "inbox")}>
            <InboxIcon />
          </button>
        )}
        {folder !== "in:mute" ? (
          <button type="button" class="icon-btn" aria-label={`Mute “${title}”`} title="Mute" data-action="mute" onClick={() => onAction(digest.waveId, "mute")}>
            <MuteIcon />
          </button>
        ) : null}
      </span>
    </li>
  );
}

export function SearchPanel({ client, profiles, selectedWaveId, onSelect, onNewWave, refreshToken }: SearchPanelProps) {
  const live = useMemo(() => new OpenWaveDigests(client), [client]);
  const store = useMemo(
    () => new SearchStore({ search: (q, i, n) => client.search(q, i, n), live }),
    [client, live],
  );
  const [draft, setDraft] = useState(store.query.peek());
  const [busy, setBusy] = useState<string | null>(null);
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
  const act = async (waveId: string, action: "archive" | "inbox" | "mute"): Promise<void> => {
    setBusy(waveId);
    try {
      await withWave(client, waveId, (wave) =>
        wave.writeSupplement((s) => {
          if (action === "archive") {
            s.archive();
          } else if (action === "inbox") {
            s.follow();
            s.inbox();
          } else {
            s.unfollow();
          }
        }),
      );
    } catch (error) {
      console.error("search: the action failed", error);
    } finally {
      setBusy(null);
      store.refreshSoon();
    }
  };

  // Up/Down move between digests (the original's list keys).
  const onListKey = (event: KeyboardEvent): void => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
      return;
    }
    const links = Array.from(listRef.current?.querySelectorAll<HTMLElement>(".digest-link") ?? []);
    const index = links.indexOf(document.activeElement as HTMLElement);
    const next = links[Math.max(0, Math.min(links.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)))];
    if (next !== undefined) {
      event.preventDefault();
      next.focus();
    }
  };

  return (
    <section class="search" aria-label="Search">
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
    </section>
  );
}
