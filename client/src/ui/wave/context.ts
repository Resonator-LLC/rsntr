// What the components of the wave panel share.

import type { Announcer } from "./announce";
import type { Signal } from "@preact/signals";
import { createContext } from "preact";

import type { DirectoryEntry, ProbeResult } from "../../net/http";
import type { ProfileStore } from "../profiles/profileStore";
import type { WaveAppsUi } from "../waveapps/AppView";
import type { WaveAttachments } from "./attachments";
import type { LazyActivator } from "./lazyViews";
import type { ConversationStore } from "./store";
import type { BlipViewFactory } from "./viewFactory";

export type DialogState =
  | { readonly kind: "delete"; readonly blipId: string }
  | { readonly kind: "link"; readonly blipId: string }
  | { readonly kind: "addParticipant" }
  | { readonly kind: "removeSelf"; readonly address: string }
  | { readonly kind: "attachment"; readonly file: File }
  | { readonly kind: "app"; readonly file: File }
  | { readonly kind: "appLog"; readonly gadgetId: string; readonly name: string };

export interface AnchoredState {
  readonly anchor: HTMLElement;
}

export interface WaveUi {
  readonly store: ConversationStore;
  /** Announces messages others add (the live region of the wave). */
  readonly announcer: Announcer;
  readonly profiles: ProfileStore;
  readonly createView: BlipViewFactory;
  readonly lazy: LazyActivator;
  /** Null if the session has no attachment service (tests). */
  readonly attachments: WaveAttachments | null;
  /** Null if the session cannot run wave apps (tests); gadgets with apps then show as chips. */
  readonly apps: WaveAppsUi | null;
  readonly newAttachmentId: () => string;
  /** The address of the signed-in user, if known. */
  readonly viewer: string | null;
  readonly localDomain: string | null;
  /** Looks up accounts of this server to suggest; null if unavailable. */
  readonly directory: ((query: string) => Promise<readonly DirectoryEntry[]>) | null;
  /** With federation: asks whether an address of another domain can be added; null without. */
  readonly probe: ((address: string) => Promise<ProbeResult>) | null;
  /** The blip whose menu is open. */
  readonly menu: Signal<(AnchoredState & { readonly blipId: string }) | null>;
  /** The participant whose card is open. */
  readonly participantCard: Signal<(AnchoredState & { readonly address: string }) | null>;
  readonly dialog: Signal<DialogState | null>;
  /** The `href` of a `link/wave` annotation value. */
  readonly waveLinkHref: (value: string) => string;
  /** The absolute URL of a blip of this wave. */
  readonly linkForBlip: (blipId: string) => string;
  /** The keys that end an edit session; true if handled. */
  readonly onEditorKeyDown: (event: KeyboardEvent) => boolean;
  /** The `@mention` list's keys, asked first while it is open; true if handled. */
  readonly mentionKeys: { current: ((event: KeyboardEvent) => boolean) | null };
}

export const WaveUiContext = createContext<WaveUi | null>(null);
