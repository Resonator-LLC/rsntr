// An open wave: header, participants, toolbar, the conversation and the
// tags, with the keyboard and mouse handling of the focus frame.
//
// Key bindings follow the Apache Wave web client
// (`FocusFrameController`: up/down move the frame, space/shift-space go to
// the next/previous unread blip; `EditController`: enter and ctrl-enter
// reply, shift-enter continues the thread, ctrl-e edits, shift-delete
// deletes; `EditSession`: shift-enter and escape end an edit session).

import { Announcer } from "./announce";
import type { DirectoryEntry, ProbeResult } from "../../net/http";
import { signal } from "@preact/signals";
import { useContext, useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { ComponentChildren } from "preact";

import type { OpenWave } from "../../client";
import { InvalidIdError, ModernIdSerialiser, WaveRef, type WaveletId } from "../../model/id";
import type { StreamHandle, StreamHandlers } from "../../net";
import type { AttachmentService } from "../../net/http";
import type { AppEventRequest, AppEventResponse, AppOpenRequest, AppUpdate } from "../../wire";
import type { WaveAppsUi } from "../waveapps/AppView";
import type { Route } from "../contracts";
import type { ProfileStore } from "../profiles/profileStore";
import { AttachmentIdGenerator, WaveAttachments } from "./attachments";
import { BlipMenu, Dialogs, ParticipantCard, ParticipantsBar, TagsBar, Toolbar } from "./chrome";
import { ThreadItem } from "./Conversation";
import { type WaveUi, WaveUiContext } from "./context";
import { BackIcon } from "./icons";
import { LazyActivator } from "./lazyViews";
import { MentionPopup } from "./MentionPopup";
import { ConversationStore } from "./store";
import { ROOT_THREAD } from "./traversal";
import { type BlipViewFactory, defaultBlipViewFactory } from "./viewFactory";

/** How long changes are gathered before the shell is told (`onWaveChanged`). */
export const CHANGE_DEBOUNCE_MS = 500;

export interface WaveViewProps {
  readonly wave: OpenWave;
  readonly profiles: ProfileStore;
  /** The conversation to show, as a serialised wavelet id; default: the root conversation. */
  readonly waveletId: string | null;
  /** The blip to focus (a deep link). */
  readonly blipId: string | null;
  /** Start editing the root blip (a wave created a moment ago). */
  readonly startEditing: boolean;
  readonly viewer: string | null;
  readonly localDomain: string | null;
  /** Looks up accounts of this server to suggest as participants. */
  readonly directory?: (query: string) => Promise<readonly DirectoryEntry[]>;
  /**
   * Set if the server federates: asks whether an address of another domain
   * can be added. Attachment ids then name the domain.
   */
  readonly probe?: (address: string) => Promise<ProbeResult>;
  readonly attachmentService: AttachmentService | null;
  /** Opens wave apps over the session's socket; without it apps show as chips. */
  readonly apps?: {
    open(request: AppOpenRequest, handlers: StreamHandlers<AppUpdate>): StreamHandle;
    event(request: AppEventRequest): Promise<AppEventResponse>;
  } | null;
  /** The seed of the session's id generator, for attachment ids. */
  readonly idSeed: string | null;
  readonly onClose: () => void;
  readonly onWaveChanged: () => void;
  readonly onNavigate: (route: Route) => void;
  readonly linkFor: (route: Route) => string;
  /** Default: the editor. */
  readonly createView?: BlipViewFactory;
  /** Render every blip document at once (tests). */
  readonly eagerViews?: boolean;
  /** Shown under the toolbar (such as "continue as new wave"). */
  readonly notice?: ComponentChildren;
}

/** The key of `m/gadgets` that allows a gadget's app: the app id allowed. */
export const APP_GRANT_KEY = "allowed";

function waveAppsUi(
  socket: NonNullable<WaveViewProps["apps"]>,
  waveId: string,
  wave: OpenWave,
  attachments: WaveAttachments | null,
): WaveAppsUi {
  return {
    waveId,
    open: (request, handlers) => socket.open(request, handlers),
    event: (request) => socket.event(request),
    allow: (gadgetId, appId) => {
      wave.writeSupplement((s) => s.setGadgetState(gadgetId, APP_GRANT_KEY, appId));
    },
    attachmentUrl: (id) => attachments?.attachmentUrl(id) ?? null,
    showLog: null,
  };
}

function parseWaveletId(serialised: string | null): WaveletId | null {
  if (serialised === null) {
    return null;
  }
  try {
    return ModernIdSerialiser.INSTANCE.deserialiseWaveletId(serialised);
  } catch (error) {
    if (error instanceof InvalidIdError) {
      return null;
    }
    throw error;
  }
}

/** The route a `link/wave` value names (a wave id or a waveref), or null. */
function routeOfWaveLink(value: string): Route | null {
  try {
    const ref = WaveRef.decodeFromPath(value);
    const serialiser = ModernIdSerialiser.INSTANCE;
    return {
      waveId: serialiser.serialiseWaveId(ref.waveId),
      waveletId: ref.waveletId !== null ? serialiser.serialiseWaveletId(ref.waveletId) : null,
      blipId: ref.documentId,
    };
  } catch {
    return null;
  }
}

/** Whether a key event belongs to something the user types into or operates. */
/** A touch screen is the main pointer: typing is on an on-screen keyboard. */
function touchFirst(): boolean {
  return typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;
}

/** The live region of messages others add (its own component: an announcement re-renders only it). */
function Announcements() {
  const ui = useContext(WaveUiContext);
  if (ui === null) {
    return null;
  }
  return (
    <div class="visually-hidden" role="status" aria-live="polite" data-testid="wave-announcer">
      {ui.announcer.text.value}
    </div>
  );
}

function isTyping(target: Element): boolean {
  return (
    target.closest(
      "input, textarea, select, [contenteditable='true'], [contenteditable=''], [role='textbox'], [role='menu'], dialog",
    ) !== null
  );
}

export function WaveView(props: WaveViewProps) {
  const { wave } = props;
  const [generation, setGeneration] = useState(0);
  const propsRef = useRef(props);
  propsRef.current = props;
  const changeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hrefRoutes = useRef(new Map<string, Route>());

  // The conversation is rebuilt when its wavelet is replaced or appears.
  useEffect(() => {
    const bump = (): void => setGeneration((g) => g + 1);
    const offs = [
      wave.events.on("waveletReset", bump),
      wave.conversations.events.on("conversationAdded", bump),
      wave.conversations.events.on("conversationRemoved", bump),
    ];
    return () => offs.forEach((off) => off());
  }, [wave]);

  const ui = useMemo((): WaveUi | null => {
    const waveletId = parseWaveletId(props.waveletId);
    const target = waveletId !== null && wave.view.wavelet(waveletId) !== null ? { waveletId } : {};
    const observable = wave.observe(target);
    if (observable === null) {
      return null;
    }
    const notify = (): void => {
      if (changeTimer.current === null) {
        changeTimer.current = setTimeout(() => {
          changeTimer.current = null;
          propsRef.current.onWaveChanged();
        }, CHANGE_DEBOUNCE_MS);
      }
    };
    // Messages others add, for screen readers; described once they are in the structure.
    let storeRef: ConversationStore | null = null;
    const announcer = new Announcer({
      viewer: props.viewer,
      name: (address) => props.profiles.get(address).name,
      describe: (blipId) => {
        const meta = storeRef?.blipMeta(blipId);
        if (storeRef === null || meta === null || meta === undefined) {
          return null;
        }
        const threadId = storeRef.threadOf(blipId);
        const parent = threadId === null ? null : storeRef.parentBlipOf(threadId);
        return { author: meta.author, parentAuthor: parent === null ? null : (storeRef.blipMeta(parent)?.author ?? null) };
      },
    });
    const store = new ConversationStore(wave, observable, {
      onChanged: notify,
      onRemoteBlipAdded: (blipId) => announcer.add(blipId),
    });
    storeRef = store;
    const serialiser = ModernIdSerialiser.INSTANCE;
    const waveIdString = serialiser.serialiseWaveId(wave.waveId);
    const waveletIdString = serialiser.serialiseWaveletId(store.waveletId);
    const mentionKeys: WaveUi["mentionKeys"] = { current: null };
    const attachments =
      props.attachmentService !== null
        ? new WaveAttachments(
            props.attachmentService,
            wave.waveId,
            props.probe !== undefined && props.localDomain !== null ? { localDomain: props.localDomain } : null,
          )
        : null;
    const idGenerator =
      props.idSeed !== null && props.localDomain !== null
        ? new AttachmentIdGenerator(props.localDomain, props.idSeed, true)
        : new AttachmentIdGenerator("local", Math.random().toString(36).slice(2, 12));
    return {
      store,
      announcer,
      profiles: props.profiles,
      createView: props.createView ?? defaultBlipViewFactory,
      lazy: new LazyActivator({ root: null, eager: props.eagerViews === true }),
      attachments,
      apps:
        props.apps !== undefined && props.apps !== null
          ? waveAppsUi(props.apps, waveIdString, wave, attachments)
          : null,
      newAttachmentId: () => idGenerator.next(),
      viewer: props.viewer,
      localDomain: props.localDomain,
      directory: props.directory ?? null,
      probe: props.probe ?? null,
      menu: signal(null),
      participantCard: signal(null),
      dialog: signal(null),
      waveLinkHref: (value) => {
        const route = routeOfWaveLink(value);
        if (route === null) {
          return `#${value}`;
        }
        const href = propsRef.current.linkFor(route);
        hrefRoutes.current.set(href, route);
        return href;
      },
      linkForBlip: (blipId) =>
        propsRef.current.linkFor({ waveId: waveIdString, waveletId: waveletIdString, blipId }),
      mentionKeys,
      onEditorKeyDown: (event) => {
        if (mentionKeys.current?.(event) === true) {
          return true;
        }
        // Shift+Enter ends the edit only where a hardware keyboard is the
        // rule: on-screen keyboards shift a new line's first key (iOS sends
        // the Enter after an Enter as Shift+Enter), so there Enter only
        // ever makes lines and Done ends the edit.
        const shiftEnterEnds = event.key === "Enter" && event.shiftKey && !touchFirst();
        if (shiftEnterEnds || event.key === "Escape") {
          if (event.isComposing) {
            return false;
          }
          event.preventDefault();
          event.stopPropagation();
          store.stopEditing();
          return true;
        }
        return false;
      },
    };
  }, [wave, generation, props.waveletId]);

  useEffect(() => {
    if (ui === null) {
      return;
    }
    return () => {
      ui.store.dispose();
      ui.announcer.dispose();
      ui.lazy.dispose();
    };
  }, [ui]);

  useEffect(
    () => () => {
      if (changeTimer.current !== null) {
        clearTimeout(changeTimer.current);
        changeTimer.current = null;
        // What was pending is still worth telling.
        propsRef.current.onWaveChanged();
      }
    },
    [],
  );

  const store = ui?.store ?? null;
  const hasConversation = store?.hasConversation.value ?? false;

  // "New wave": edit the root blip (`StagesProvider.initNewWave`). Once per wave.
  const startedFor = useRef<OpenWave | null>(null);
  useEffect(() => {
    if (store === null || !props.startEditing || startedFor.current === wave) {
      return;
    }
    const root = store.rootBlipId();
    if (root !== null) {
      startedFor.current = wave;
      store.startEditing(root);
    }
  }, [store, props.startEditing, hasConversation]);

  // A deep link focuses its blip (`StagesProvider.handleExistingWave`).
  useEffect(() => {
    if (store === null || props.blipId === null) {
      return;
    }
    if (store.blips.has(props.blipId) && store.editingId.peek() === null) {
      store.focus(props.blipId);
    }
  }, [store, props.blipId, hasConversation]);

  const header = (title: string) => (
    <header class="wave-header">
      <button type="button" class="icon-btn wave-back" aria-label="Back to the list of waves" onClick={props.onClose}>
        <BackIcon />
      </button>
      <h2 class={`wave-title${title === "" ? " wave-title-empty" : ""}`}>{title === "" ? "Untitled wave" : title}</h2>
    </header>
  );

  if (ui === null || store === null || !hasConversation) {
    return (
      <section class="wave-panel" aria-label="Wave">
        {header("")}
        <div class="wave-state" role="status">
          <p class="wave-state-title">Wave not found</p>
          <p>This wave does not exist, or you do not have access to it.</p>
        </div>
      </section>
    );
  }

  const onKeyDown = (event: KeyboardEvent): void => {
    if (event.defaultPrevented || event.isComposing || event.altKey || !(event.target instanceof Element)) {
      return;
    }
    const target = event.target;
    if (isTyping(target)) {
      return;
    }
    const operable = target.closest("button, a[href], summary") !== null;
    const focused = store.focusedId.peek();
    const command = event.ctrlKey || event.metaKey;
    let handled = true;
    switch (event.key) {
      case "ArrowUp":
        if (command || event.shiftKey) {
          return;
        }
        store.moveUp();
        break;
      case "ArrowDown":
        if (command || event.shiftKey) {
          return;
        }
        store.moveDown();
        break;
      case " ":
        if (command || operable) {
          return;
        }
        if (event.shiftKey) {
          store.focusPrevious();
        } else {
          store.nextUnread();
        }
        break;
      case "Enter":
        if (operable || focused === null || event.metaKey) {
          return;
        }
        if (event.shiftKey) {
          const threadId = store.threadOf(focused);
          if (threadId !== null) {
            store.continueThread(threadId);
          }
        } else {
          store.reply(focused);
        }
        break;
      case "e":
      case "E":
        if (!event.ctrlKey || event.metaKey || event.shiftKey || focused === null) {
          return;
        }
        store.startEditing(focused);
        break;
      case "Delete":
        if (!event.shiftKey || command || focused === null) {
          return;
        }
        ui.dialog.value = { kind: "delete", blipId: focused };
        break;
      default:
        handled = false;
    }
    if (handled) {
      event.preventDefault();
    }
  };

  const blipOf = (target: EventTarget | null): string | null =>
    target instanceof Element ? (target.closest("[data-blip-id]")?.getAttribute("data-blip-id") ?? null) : null;

  // `FocusFrameController.onMouseDown`: a press on a blip focuses it, without scrolling.
  const onMouseDown = (event: MouseEvent): void => {
    if (event.button !== 0) {
      return;
    }
    const blipId = blipOf(event.target);
    if (blipId !== null) {
      store.focus(blipId, { scroll: false, dom: false });
    }
  };

  // The frame follows the keyboard focus (tabbing to a link of another blip).
  const onFocusIn = (event: FocusEvent): void => {
    const blipId = blipOf(event.target);
    if (blipId !== null && blipId !== store.focusedId.peek()) {
      store.focus(blipId, { scroll: false, dom: false });
    }
  };

  // Links to waves navigate within the app.
  const onClick = (event: MouseEvent): void => {
    if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) {
      return;
    }
    const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
    const route = link === null ? undefined : hrefRoutes.current.get(link.getAttribute("href") ?? "");
    if (route !== undefined) {
      event.preventDefault();
      props.onNavigate(route);
    }
  };

  const error = store.error.value;

  return (
    <WaveUiContext.Provider value={ui}>
      <section class="wave-panel" aria-label="Wave">
        {header(store.title.value)}
        <ParticipantsBar />
        <Toolbar />
        {props.notice ?? null}
        {error !== null ? (
          <div class="notice notice-error wave-error" role="alert">
            <span>{error}</span>
            <button
              type="button"
              class="btn btn-ghost"
              onClick={() => {
                store.error.value = null;
              }}
            >
              Dismiss
            </button>
          </div>
        ) : null}
        <div
          class="wave-scroll"
          ref={(el) => {
            if (el !== null) {
              ui.lazy.setRoot(el);
            }
          }}
          onKeyDown={onKeyDown}
          onMouseDown={onMouseDown}
          onFocusIn={onFocusIn}
          onClick={onClick}
        >
          <ThreadItem id={ROOT_THREAD} placement="root" />
        </div>
        <TagsBar />
        <BlipMenu />
        <ParticipantCard />
        <Dialogs />
        <MentionPopup />
        <Announcements />
      </section>
    </WaveUiContext.Provider>
  );
}
