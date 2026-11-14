// The state of the shell: session, connection, saved state and route, as
// signals, with the actions that change them. The components only render it.
//
// Stands where `WebClient`, `Session`, `SavedStateIndicator` and the
// history handling of the Apache Wave web client stand (Apache License 2.0).

import type { PrefsStore } from "./prefs";
import { type Signal, batch, signal } from "@preact/signals";

import type { WaveClient } from "../../client";
import type { ConnectionState, SessionInfo } from "../../net";
import type { RpcFatalError } from "../../net/errors";
import type { Unsubscribe } from "../../util/events";
import type { Route } from "../contracts";
import type { ProfileStore } from "../profiles/profileStore";
import { MESSAGES, authErrorMessage, normaliseAddress } from "./authMessages";
import type { UpdateCheck } from "./updateCheck";
import { EMPTY_ROUTE, formatFragment, parseFragment, sameRoute, urlForRoute, waveRoute } from "./router";
import { holdTyping } from "./typeAhead";

/** The part of the browser the shell's routing uses. */
export interface BrowserLocation {
  /** The fragment of the page's URL, with or without `#`. */
  fragment(): string;
  /** Makes a new history entry with this fragment (empty: none). */
  push(fragment: string): void;
  /** Calls back when the fragment changed by navigation (back, forward, a link, an edit). */
  onChange(listener: () => void): Unsubscribe;
  /** The URL of the page. */
  href(): string;
}

export interface AppTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

const PROFILE_REFRESH_MS = 15 * 60 * 1000;

const DEFAULT_TIMERS: AppTimers = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/**
 * `loading`: the session is being read. `unreachable`: that failed.
 * `signedOut`: the sign-in (or registration) screen. `signedIn`: the panels.
 */
export type Phase = "loading" | "unreachable" | "signedOut" | "signedIn";

/** What the top bar says about the connection (`WebClientMessages`). */
export type NetStatus = "online" | "offline" | "connecting";

export const NET_STATUS_TEXT: Record<NetStatus, string> = {
  online: "Online",
  offline: "Offline",
  connecting: "Connecting...",
};

/** `SavedStateMessages`. */
export const SAVED_TEXT = "Saved";
export const UNSAVED_TEXT = "Unsaved...";

/** `WebClient.setupConnectionIndicator` over the states of the channel. */
export function netStatus(state: ConnectionState): NetStatus {
  switch (state) {
    case "connected":
      return "online";
    case "connecting":
    case "authenticating":
    case "waiting":
      return "connecting";
    case "idle":
    case "offline":
    case "closed":
      return "offline";
  }
}

/** A message above the sign-in form. */
export interface AuthNotice {
  readonly kind: "info" | "error";
  readonly text: string;
}

/** `SavedStateIndicator.UPDATE_DELAY_MS`: "Saved" is shown with this delay. */
export const SAVED_DELAY_MS = 300;

/** The fragment of the settings page (device mode): `#settings`. */
export const SETTINGS_FRAGMENT = "settings";

/** Whether a fragment (with or without `#`) names the settings page. */
export function isSettingsFragment(fragment: string): boolean {
  return fragment.replace(/^#/, "") === SETTINGS_FRAGMENT;
}

/** A second forced session end within this time is not retried. */
const SESSION_RETRY_WINDOW_MS = 30_000;

export interface AppStoreOptions {
  client: WaveClient;
  profiles: ProfileStore;
  location: BrowserLocation;
  timers?: AppTimers;
  clock?: () => number;
  /** Asks the server for a newer client build; see `updateCheck.ts`. */
  update?: UpdateCheck;
  /** Display and accessibility preferences of this device. */
  prefs?: PrefsStore;
}

export class AppStore {
  readonly client: WaveClient;
  readonly profiles: ProfileStore;
  readonly prefs: PrefsStore | null;
  readonly update: UpdateCheck | null;

  readonly phase: Signal<Phase> = signal<Phase>("loading");
  readonly session: Signal<SessionInfo | null> = signal<SessionInfo | null>(null);
  /** Which of the two screens is shown while signed out. */
  readonly authScreen: Signal<"signIn" | "register"> = signal<"signIn" | "register">("signIn");
  readonly authNotice: Signal<AuthNotice | null> = signal<AuthNotice | null>(null);
  readonly connection: Signal<ConnectionState> = signal<ConnectionState>("idle");
  /** Whether the indicator shows "Unsaved..." (the original's delay applied). */
  readonly unsavedShown: Signal<boolean> = signal(false);
  /**
   * How many changes the server keeps for a host it cannot reach now
   * (another server, or another device of the person): "waiting for host".
   */
  readonly awaitingHost: Signal<number> = signal(0);
  readonly route: Signal<Route> = signal<Route>(EMPTY_ROUTE);
  /** The settings page is shown instead of the panels (device mode). */
  readonly settingsOpen: Signal<boolean> = signal(false);
  /** The keyboard shortcuts dialog ("?"). */
  readonly shortcutsOpen: Signal<boolean> = signal(false);
  /** The wave "New wave" made, while it is the one shown. */
  readonly newWaveId: Signal<string | null> = signal<string | null>(null);
  readonly creatingWave: Signal<boolean> = signal(false);
  /** Passed to the search panel; see `SearchPanelProps.refreshToken`. */
  readonly refreshToken: Signal<number> = signal(0);
  /** Bumped when the panels must start afresh (a new session). */
  readonly epoch: Signal<number> = signal(0);
  /** A problem to tell the user about, above the panels. */
  readonly problem: Signal<string | null> = signal<string | null>(null);

  private readonly location: BrowserLocation;
  private readonly timers: AppTimers;
  private readonly clock: () => number;
  private readonly subscriptions: Unsubscribe[] = [];
  private savedTimer: unknown = null;
  private profilesTimer: unknown = null;
  private lastForcedEnd = Number.NEGATIVE_INFINITY;
  private disposed = false;
  /** The connection was up once: a later "connected" is a reconnect. */
  private wasConnected = false;

  constructor(options: AppStoreOptions) {
    this.client = options.client;
    this.profiles = options.profiles;
    this.prefs = options.prefs ?? null;
    this.update = options.update ?? null;
    this.location = options.location;
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.clock = options.clock ?? Date.now;
    this.route.value = parseFragment(this.location.fragment());
    this.refreshProfilesLater();
    this.settingsOpen.value = isSettingsFragment(this.location.fragment());
    this.subscriptions.push(
      this.location.onChange(() => this.onLocationChanged()),
      this.client.events.on("connection", ({ state }) => {
        this.connection.value = state;
        if (state === "connected") {
          if (this.wasConnected) {
            this.update?.onReconnected();
            // Back from sleep or the background: names and avatars may
            // have changed meanwhile.
            this.profiles.refreshAll();
          }
          this.wasConnected = true;
        }
      }),
      this.client.events.on("unsaved", (state) => this.onUnsaved(state.unacknowledged !== 0)),
      this.client.events.on("awaitingHost", (count) => {
        this.awaitingHost.value = count;
      }),
      this.client.events.on("signedOut", ({ error }) => this.onSignedOut(error)),
    );
  }

  dispose(): void {
    this.disposed = true;
    this.subscriptions.forEach((off) => off());
    this.subscriptions.length = 0;
    if (this.savedTimer !== null) {
      this.timers.clearTimeout(this.savedTimer);
    }
    if (this.profilesTimer !== null) {
      this.timers.clearTimeout(this.profilesTimer);
    }
  }

  /** Fetches the names and avatars shown again every 15 minutes. */
  private refreshProfilesLater(): void {
    this.profilesTimer = this.timers.setTimeout(() => {
      if (!this.disposed) {
        this.profiles.refreshAll();
        this.refreshProfilesLater();
      }
    }, PROFILE_REFRESH_MS);
  }

  // ---- session ----

  /** Reads the session; with one, the connection starts. */
  async start(): Promise<void> {
    this.phase.value = "loading";
    try {
      const session = await this.client.start();
      if (this.disposed) {
        return;
      }
      this.enter(session);
    } catch {
      if (!this.disposed) {
        this.phase.value = "unreachable";
      }
    }
  }

  private enter(session: SessionInfo): void {
    batch(() => {
      this.session.value = session;
      this.connection.value = this.client.connectionState;
      if (session.address !== null) {
        this.authNotice.value = null;
        this.authScreen.value = "signIn";
        // The fragment may have been there before the sign-in: a deep link.
        this.route.value = parseFragment(this.location.fragment());
        this.settingsOpen.value = isSettingsFragment(this.location.fragment());
        this.epoch.value++;
        this.phase.value = "signedIn";
      } else {
        if (!session.registrationEnabled) {
          this.authScreen.value = "signIn";
        }
        this.phase.value = "signedOut";
      }
    });
  }

  /** Resolves with null, or with the message of the failure. */
  async signIn(address: string, password: string): Promise<string | null> {
    try {
      const session = await this.client.signIn(normaliseAddress(address), password);
      this.enter(session);
      return null;
    } catch (error) {
      return authErrorMessage(error);
    }
  }

  /**
   * Creates the account and signs in with it. (The original's page says
   * "Registration complete." and links to the sign-in page; if the sign-in
   * fails here, that is what happens.) Resolves with null, or with the
   * message of the failure.
   */
  async register(address: string, password: string): Promise<string | null> {
    const name = normaliseAddress(address);
    try {
      await this.client.register(name, password);
    } catch (error) {
      return authErrorMessage(error);
    }
    try {
      this.enter(await this.client.signIn(name, password));
    } catch {
      batch(() => {
        this.authScreen.value = "signIn";
        this.authNotice.value = {
          kind: "info",
          text: `${MESSAGES.registrationComplete} ${MESSAGES.pleaseSignIn}`,
        };
      });
    }
    return null;
  }

  showAuthScreen(screen: "signIn" | "register"): void {
    batch(() => {
      this.authNotice.value = null;
      this.authScreen.value = screen;
    });
  }

  async signOut(): Promise<void> {
    try {
      // Emits `signedOut` with no error.
      await this.client.signOut();
    } catch {
      // The session is forgotten here whatever the server said.
    }
    if (this.phase.value === "signedIn") {
      this.leave(null);
    }
    await this.readSession();
  }

  private onSignedOut(error: RpcFatalError | null): void {
    if (this.disposed) {
      return;
    }
    if (error === null) {
      this.leave(null);
      return;
    }
    // The server ended the session, or refused the connection's credential.
    const text = error.kind === "signedOut" ? MESSAGES.signedOutElsewhere : MESSAGES.sessionEnded;
    const now = this.clock();
    const retry = now - this.lastForcedEnd > SESSION_RETRY_WINDOW_MS;
    this.lastForcedEnd = now;
    this.leave({ kind: "error", text });
    void this.readSession(retry);
  }

  /** To the sign-in screen. */
  private leave(notice: AuthNotice | null): void {
    batch(() => {
      this.phase.value = "signedOut";
      this.authScreen.value = "signIn";
      this.authNotice.value = notice;
      this.newWaveId.value = null;
      this.creatingWave.value = false;
      this.problem.value = null;
      this.connection.value = "idle";
      this.awaitingHost.value = 0;
      this.setUnsavedShown(false);
      const session = this.session.value;
      if (session !== null) {
        this.session.value = { ...session, address: null, socketToken: null };
      }
    });
    this.profiles.clear();
  }

  /**
   * Reads the session again after it ended: the server may have changed
   * its mind about registration, and if the cookie is still good (only the
   * connection was refused) the user is signed in again, once.
   */
  private async readSession(mayReenter = false): Promise<void> {
    try {
      const session = await this.client.api.getSession();
      if (this.disposed || this.phase.value !== "signedOut") {
        return;
      }
      if (session.address !== null && mayReenter) {
        this.enter(await this.client.start());
        return;
      }
      this.session.value = { ...session, address: null, socketToken: null };
      if (!session.registrationEnabled) {
        this.authScreen.value = "signIn";
      }
    } catch {
      // The sign-in screen stays as it is.
    }
  }

  // ---- saved state ----

  private onUnsaved(unsaved: boolean): void {
    if (this.savedTimer !== null) {
      this.timers.clearTimeout(this.savedTimer);
      this.savedTimer = null;
    }
    if (unsaved) {
      this.unsavedShown.value = true;
    } else if (this.unsavedShown.value) {
      this.savedTimer = this.timers.setTimeout(() => {
        this.savedTimer = null;
        this.unsavedShown.value = false;
      }, SAVED_DELAY_MS);
    }
  }

  private setUnsavedShown(shown: boolean): void {
    if (this.savedTimer !== null) {
      this.timers.clearTimeout(this.savedTimer);
      this.savedTimer = null;
    }
    this.unsavedShown.value = shown;
  }

  /** Whether leaving the page now would lose work. */
  hasUnsavedWork(): boolean {
    return this.phase.value === "signedIn" && this.client.unsavedState.unacknowledged !== 0;
  }

  // ---- route ----

  private onLocationChanged(): void {
    this.settingsOpen.value = isSettingsFragment(this.location.fragment());
    const route = parseFragment(this.location.fragment());
    if (!sameRoute(route, this.route.value)) {
      this.setRoute(route);
    }
  }

  private setRoute(route: Route): void {
    batch(() => {
      if (route.waveId !== this.newWaveId.value) {
        this.newWaveId.value = null;
      }
      this.route.value = route;
    });
  }

  /** Goes to a route, with a new history entry. */
  navigate = (route: Route): void => {
    if (sameRoute(route, this.route.value) && !this.settingsOpen.value) {
      return;
    }
    let fragment: string;
    try {
      fragment = formatFragment(route);
    } catch {
      return;
    }
    this.location.push(fragment);
    this.settingsOpen.value = false;
    this.setRoute(route);
  };

  /** Shows the settings page (device mode), with a history entry. */
  openSettings = (): void => {
    if (this.settingsOpen.value) {
      return;
    }
    this.location.push(SETTINGS_FRAGMENT);
    this.settingsOpen.value = true;
  };

  /** Back from the settings page to the list of waves. */
  closeSettings = (): void => {
    if (!this.settingsOpen.value) {
      return;
    }
    this.location.push("");
    batch(() => {
      this.settingsOpen.value = false;
      this.setRoute(EMPTY_ROUTE);
    });
  };

  selectWave = (waveId: string): void => {
    this.navigate(waveRoute(waveId));
  };

  /** Leaves the wave: back to the list (the phone layout's back button). */
  closeWave = (): void => {
    this.navigate(EMPTY_ROUTE);
  };

  linkFor = (route: Route): string => {
    try {
      return urlForRoute(route, this.location.href());
    } catch {
      return this.location.href();
    }
  };

  // ---- waves ----

  /**
   * "New Wave": a wave with the creator as its only participant and an
   * empty root blip, shown at once and edited (`WebClient.openWave` with
   * `isNewWave`).
   */
  newWave = async (): Promise<void> => {
    if (this.creatingWave.value) {
      return;
    }
    this.creatingWave.value = true;
    this.problem.value = null;
    const dropTyping = holdTyping();
    try {
      const { wave } = await this.client.createWave();
      if (this.phase.value !== "signedIn") {
        dropTyping();
        return;
      }
      const waveId = wave.waveId.serialise();
      batch(() => {
        this.navigate(waveRoute(waveId));
        this.newWaveId.value = waveId;
      });
      // The original searches again half a second after creating a wave.
      this.waveChanged();
    } catch {
      dropTyping();
      if (this.phase.value === "signedIn") {
        this.problem.value =
          this.connection.value === "connected"
            ? "The wave could not be created. Please try again."
            : "The wave could not be created while offline. Try again when the connection is back.";
      }
    } finally {
      this.creatingWave.value = false;
    }
  };

  /** Something the search panel shows may have changed. */
  waveChanged = (): void => {
    this.refreshToken.value++;
  };

  dismissProblem = (): void => {
    this.problem.value = null;
  };

  reconnect = (): void => {
    this.client.reconnectNow();
  };

  /**
   * The page is shown again (the app or tab was in the background, the
   * phone asleep): a connection that is not up is tried again now rather
   * than after its backoff, and an attempt begun before the sleep (whose
   * socket the system may have dropped silently) is started afresh.
   */
  resumed = (): void => {
    const net = netStatus(this.connection.value);
    if (net === "online") {
      return;
    }
    if (net === "connecting") {
      this.client.dropConnection();
    }
    this.client.reconnectNow();
  };
}

/** The routing of a browser window. */
export function windowLocation(win: Window = window): BrowserLocation {
  return {
    fragment: () => win.location.hash,
    push: (fragment) => {
      if (fragment === "") {
        // Not `location.hash = ""`, which leaves a dangling `#`.
        win.history.pushState(null, "", win.location.pathname + win.location.search);
      } else {
        win.history.pushState(null, "", `#${fragment}`);
      }
    },
    onChange: (listener) => {
      win.addEventListener("hashchange", listener);
      win.addEventListener("popstate", listener);
      return () => {
        win.removeEventListener("hashchange", listener);
        win.removeEventListener("popstate", listener);
      };
    },
    href: () => win.location.href,
  };
}
