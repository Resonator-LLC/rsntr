// `@mentions` in the blip being edited: typing `@` opens a list of people
// at the caret, the wave's participants first, then the server's
// directory (and, with federation, a full address of another server).
// Arrows, Enter and Tab pick, Escape closes; a tap picks on touch screens.
// The keyboard focus stays in the editor throughout: the list is driven
// through `WaveUi.mentionKeys`, which the editor's key handler asks first.
//
// Picking replaces `@query` by `@<name>` marked `mention/user` = address
// and, as Google Wave did, adds the person to the wave if they are not in
// it yet (before the name is inserted, so an AI agent sees the mention as
// what it was added for).

import { useContext, useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";

import type { DirectoryEntry } from "../../net/http";
import type { BlipView, MentionQuery } from "../editor/api";
import { Avatar } from "../profiles/Avatar";
import { type ProbeState, describeProbe, probeStateName, remoteCandidate, remoteDomain } from "./chrome";
import { type WaveUi, WaveUiContext } from "./context";

/** How long typing must pause before the directory is asked. */
const DIRECTORY_DELAY_MS = 120;
const MAX_SUGGESTIONS = 8;
const GAP = 4;
const EDGE = 8;
const LIST_ID = "wave-mention-list";

export interface MentionCandidate {
  readonly address: string;
  readonly name: string;
  readonly kind: "human" | "agent" | "robot";
  readonly participant: boolean;
  readonly remote?: boolean;
}

/** Whether a name or address matches what was typed after the `@`. */
export function matchesMention(query: string, name: string, address: string): boolean {
  const q = query.trim().toLowerCase();
  if (q === "") {
    return true;
  }
  const n = name.toLowerCase();
  const a = address.toLowerCase();
  return n.startsWith(q) || a.startsWith(q) || n.split(/\s+/).some((word) => word.startsWith(q));
}

/** What a mention shows: the name if known, else the local part of the address. */
export function mentionLabel(name: string, address: string): string {
  const local = address.split("@")[0] ?? address;
  const n = name.trim();
  return n === "" || n === address ? local : n;
}

/**
 * The suggestions for a query: matching participants (not the viewer),
 * then directory entries not in the wave, then a remote address.
 */
export function mentionCandidates(
  query: string,
  participants: readonly { readonly address: string; readonly name: string }[],
  found: readonly DirectoryEntry[],
  viewer: string | null,
  remote: string | null,
): MentionCandidate[] {
  const out: MentionCandidate[] = [];
  const seen = new Set<string>();
  for (const p of participants) {
    if (p.address === viewer || seen.has(p.address) || !matchesMention(query, p.name, p.address)) {
      continue;
    }
    seen.add(p.address);
    out.push({ address: p.address, name: p.name, kind: "human", participant: true });
  }
  for (const entry of found) {
    if (entry.address === viewer || seen.has(entry.address)) {
      continue;
    }
    seen.add(entry.address);
    out.push({ address: entry.address, name: entry.name, kind: entry.kind, participant: false });
  }
  const list = out.slice(0, MAX_SUGGESTIONS);
  if (remote !== null && !seen.has(remote)) {
    list.push({ address: remote, name: remote, kind: "human", participant: false, remote: true });
  }
  return list;
}

/** The popup for the blip being edited, if any. */
export function MentionPopup() {
  const ui = useContext(WaveUiContext);
  const view = ui?.store.editingView.value ?? null;
  if (ui === null || view === null) {
    return null;
  }
  return <MentionSuggestions key={ui.store.editingId.value ?? ""} ui={ui} view={view} />;
}

function caretRect(view: BlipView): DOMRect | null {
  const selection = document.getSelection();
  if (selection !== null && selection.rangeCount > 0) {
    const range = selection.getRangeAt(0);
    if (view.element.contains(range.endContainer)) {
      const rects = range.getClientRects();
      const rect = rects.length > 0 ? rects[rects.length - 1]! : range.getBoundingClientRect();
      if (rect.height > 0 || rect.width > 0) {
        return rect;
      }
      // An empty line: its element.
      const node = range.endContainer;
      const el = node instanceof Element ? node : node.parentElement;
      if (el !== null) {
        return el.getBoundingClientRect();
      }
    }
  }
  return null;
}

function MentionSuggestions({ ui, view }: { ui: WaveUi; view: BlipView }) {
  const [mention, setMention] = useState<MentionQuery | null>(() => {
    const state = view.state();
    return state.focused ? (state.mention ?? null) : null;
  });
  const [dismissed, setDismissed] = useState<number | null>(null);
  const [active, setActive] = useState(0);
  const [found, setFound] = useState<{ readonly query: string; readonly entries: readonly DirectoryEntry[] } | null>(
    null,
  );
  const [probed, setProbed] = useState<{ readonly address: string; readonly result: ProbeState } | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(
    () =>
      view.onStateChange((state) => {
        setMention(state.focused ? (state.mention ?? null) : null);
      }),
    [view],
  );

  const query = mention?.query ?? null;
  // Forget a dismissal once the caret leaves that mention.
  useEffect(() => {
    if (dismissed !== null && mention?.start !== dismissed) {
      setDismissed(null);
    }
  }, [mention?.start]);
  useEffect(() => setActive(0), [query]);

  // The directory, for what is typed (a name with a space is matched locally only).
  useEffect(() => {
    const directory = ui.directory;
    if (directory === null || query === null || /\s/.test(query)) {
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      directory(query).then(
        (entries) => current && setFound({ query, entries }),
        () => current && setFound({ query, entries: [] }),
      );
    }, DIRECTORY_DELAY_MS);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [ui.directory, query]);

  const participantAddresses = ui.store.participants.value;
  const remote = query === null ? null : remoteCandidate(ui, query.trim(), participantAddresses);
  useEffect(() => {
    const probe = ui.probe;
    if (probe === null || remote === null) {
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      setProbed({ address: remote, result: "checking" });
      probe(remote).then(
        (result) => current && setProbed({ address: remote, result }),
        () => current && setProbed({ address: remote, result: "failed" }),
      );
    }, DIRECTORY_DELAY_MS);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [ui.probe, remote]);
  const remoteState: ProbeState = probed !== null && probed.address === remote ? probed.result : "checking";

  const participants = participantAddresses.map((address) => ({ address, name: ui.profiles.get(address).name }));
  const directoryEntries =
    query !== null && found !== null && !/\s/.test(query)
      ? found.entries.filter((e) => found.query === query || matchesMention(query, e.name, e.address))
      : [];
  const candidates =
    query === null ? [] : mentionCandidates(query, participants, directoryEntries, ui.viewer, remote);
  const loading = query !== null && ui.directory !== null && !/\s/.test(query) && found?.query !== query;
  // A space typed where nothing matches ends it.
  const endedBySpace = query !== null && /\s$/.test(query) && candidates.length === 0;
  const open = mention !== null && dismissed !== mention.start && !endedBySpace;
  const index = candidates.length === 0 ? 0 : Math.min(active, candidates.length - 1);

  useEffect(() => {
    if (mention !== null && endedBySpace && dismissed !== mention.start) {
      setDismissed(mention.start);
    }
  }, [endedBySpace, mention?.start]);

  const pick = (candidate: MentionCandidate): void => {
    const current = view.state().mention ?? mention;
    if (current === null || current === undefined) {
      return;
    }
    if (candidate.remote === true && probeStateName(remoteState) === "not-allowed") {
      return;
    }
    if (!ui.store.participants.peek().includes(candidate.address)) {
      ui.store.addParticipants([candidate.address]);
    }
    const name = candidate.participant ? ui.profiles.get(candidate.address).name : candidate.name;
    view.exec({
      type: "insertMention",
      start: current.start,
      end: current.end,
      text: `@${mentionLabel(name, candidate.address)}`,
      address: candidate.address,
    });
    setDismissed(current.start);
  };

  // The keys, before the editor sees them.
  const keysRef = useRef<(event: KeyboardEvent) => boolean>(() => false);
  keysRef.current = (event: KeyboardEvent): boolean => {
    if (!open || event.isComposing || event.altKey || event.ctrlKey || event.metaKey) {
      return false;
    }
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        if (candidates.length === 0) {
          return false;
        }
        const step = event.key === "ArrowDown" ? 1 : -1;
        setActive((index + step + candidates.length) % candidates.length);
        break;
      }
      case "Enter":
      case "Tab": {
        const chosen = candidates[index];
        if (chosen === undefined || event.shiftKey) {
          return false;
        }
        pick(chosen);
        break;
      }
      case "Escape":
        setDismissed(mention.start);
        break;
      default:
        return false;
    }
    event.preventDefault();
    event.stopPropagation();
    return true;
  };
  useEffect(() => {
    const handler = (event: KeyboardEvent): boolean => keysRef.current(event);
    ui.mentionKeys.current = handler;
    return () => {
      if (ui.mentionKeys.current === handler) {
        ui.mentionKeys.current = null;
      }
    };
  }, [ui]);

  // Accessibility: the editor owns the list while it is open.
  const activeId = open && candidates.length > 0 ? `${LIST_ID}-${index}` : null;
  useEffect(() => {
    const el = view.element;
    if (open) {
      el.setAttribute("aria-controls", LIST_ID);
      el.setAttribute("aria-expanded", "true");
      el.setAttribute("aria-autocomplete", "list");
      if (activeId !== null) {
        el.setAttribute("aria-activedescendant", activeId);
      } else {
        el.removeAttribute("aria-activedescendant");
      }
    } else {
      for (const name of ["aria-controls", "aria-expanded", "aria-autocomplete", "aria-activedescendant"]) {
        el.removeAttribute(name);
      }
    }
  }, [view, open, activeId]);
  useEffect(
    () => () => {
      for (const name of ["aria-controls", "aria-expanded", "aria-autocomplete", "aria-activedescendant"]) {
        view.element.removeAttribute(name);
      }
    },
    [view],
  );

  // At the caret: below it, or above it if the keyboard or the window's
  // edge leaves no room below; within the visible part of the page.
  const [placed, setPlaced] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (el === null || !open) {
      return;
    }
    const popover = el as HTMLDivElement & { showPopover?: () => void; hidePopover?: () => void };
    if (typeof popover.showPopover === "function" && !el.matches(":popover-open")) {
      try {
        popover.showPopover();
      } catch {
        // Shown in place.
      }
    }
    const rect = caretRect(view) ?? view.element.getBoundingClientRect();
    const vv = window.visualViewport;
    const top0 = vv?.offsetTop ?? 0;
    const left0 = vv?.offsetLeft ?? 0;
    const width = vv?.width ?? document.documentElement.clientWidth;
    const height = vv?.height ?? document.documentElement.clientHeight;
    const w = Math.min(el.offsetWidth, width - 2 * EDGE);
    let left = Math.max(left0 + EDGE, Math.min(rect.left - 12, left0 + width - w - EDGE));
    left = Math.round(left);
    const below = top0 + height - EDGE - (rect.bottom + GAP);
    const above = rect.top - GAP - (top0 + EDGE);
    const h = el.offsetHeight;
    let top: number;
    if (h <= below || below >= above) {
      top = rect.bottom + GAP;
      el.style.setProperty("max-height", `${Math.max(96, Math.floor(below))}px`);
    } else {
      el.style.setProperty("max-height", `${Math.max(96, Math.floor(above))}px`);
      top = rect.top - GAP - Math.min(h, el.offsetHeight);
    }
    el.style.setProperty("left", `${left}px`);
    el.style.setProperty("top", `${Math.round(top)}px`);
  });
  useEffect(() => {
    if (!open) {
      return;
    }
    const again = (): void => setPlaced((n) => n + 1);
    const vv = window.visualViewport;
    vv?.addEventListener("resize", again);
    vv?.addEventListener("scroll", again);
    window.addEventListener("resize", again);
    document.addEventListener("scroll", again, true);
    return () => {
      vv?.removeEventListener("resize", again);
      vv?.removeEventListener("scroll", again);
      window.removeEventListener("resize", again);
      document.removeEventListener("scroll", again, true);
    };
  }, [open]);
  void placed;

  if (!open) {
    return null;
  }
  // Presses keep the focus (and the caret) in the editor.
  const keep = (event: Event): void => event.preventDefault();
  return (
    <div
      ref={ref}
      class="mention-popup"
      popover="manual"
      onPointerDown={keep}
      onMouseDown={keep}
    >
      {candidates.length === 0 ? (
        <div class="mention-empty" role="status">
          {loading ? "Looking…" : query === "" ? "Type a name" : "Nobody found"}
        </div>
      ) : (
        <ul id={LIST_ID} class="mention-list" role="listbox" aria-label="People to mention">
          {candidates.map((c, i) => {
            const name = c.participant ? ui.profiles.get(c.address).name : c.name;
            const remoteDomainName = c.remote === true ? remoteDomain(ui.localDomain, c.address) : null;
            return (
              <li
                key={c.address}
                id={`${LIST_ID}-${i}`}
                class={i === index ? "mention-option mention-option-active" : "mention-option"}
                role="option"
                aria-selected={i === index}
                data-address={c.address}
                onClick={() => pick(c)}
                onPointerEnter={(event) => {
                  if (event.pointerType === "mouse") {
                    setActive(i);
                  }
                }}
              >
                <Avatar profiles={ui.profiles} address={c.address} size="sm" decorative />
                <span class="mention-text">
                  <span class="mention-name">
                    {name}
                    {c.kind !== "human" ? <span class="suggestion-kind">{c.kind === "agent" ? "AI" : "robot"}</span> : null}
                  </span>
                  <span class="mention-address">
                    {remoteDomainName !== null ? describeProbe(remoteState, remoteDomainName) : c.address}
                  </span>
                </span>
                {c.participant ? null : (
                  <span class="mention-adds" aria-hidden="true">
                    {c.remote === true ? "other server" : "adds"}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
