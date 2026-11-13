// The state of the settings page of a device node (person mode): this
// person and device, the devices, pairing, recovery, the anchor, co-hosts
// and contacts. Every action goes to the node and reloads what it changes.
//
// The recovery phrase is held only while it is shown: `hidePhrase`, a
// timeout and `dispose` drop it, and nothing stores it anywhere else.

import { type Signal, batch, signal } from "@preact/signals";

import { looksLikePairLink, parsePairCode } from "../../net/pairCode";
import {
  type Contact,
  type JoinStage,
  type NewPairing,
  type Pairing,
  type PersonApi,
  PersonApiError,
  type PersonOverview,
  isPairLink,
  personErrorText,
} from "../../net/person";

/** What the store needs of the node. */
export type SettingsApi = Pick<
  PersonApi,
  | "overview"
  | "revoke"
  | "setAnchor"
  | "revealPhrase"
  | "recover"
  | "coHost"
  | "invite"
  | "pairings"
  | "confirmPairing"
  | "cancelPairing"
  | "join"
  | "joinProgress"
  | "contacts"
  | "addContact"
  | "removeContact"
>;

export interface SettingsTimers {
  setTimeout(callback: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const DEFAULT_TIMERS: SettingsTimers = {
  setTimeout: (callback, ms) => setTimeout(callback, ms),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** How long the recovery phrase stays on screen. */
export const PHRASE_SHOWN_MS = 60_000;
/** How often pairings are looked at while an invitation is open. */
export const PAIRING_POLL_MS = 2_000;
/** How often the progress of a join is looked at. */
export const JOIN_POLL_MS = 700;
/** How long a co-host grant made here lasts. */
export const CO_HOST_DAYS = 30;

export type SettingsMessage = { readonly kind: "error" | "info"; readonly text: string };

/** Why the node is restarting: the page has to be opened again from the app. */
export type Restart = { readonly reason: "paired" | "recovered"; readonly person: string };

export class SettingsStore {
  /** undefined while loading; null outside person mode. */
  readonly overview: Signal<PersonOverview | null | undefined> = signal<PersonOverview | null | undefined>(undefined);
  readonly contacts: Signal<readonly Contact[]> = signal<readonly Contact[]>([]);
  readonly pairings: Signal<readonly Pairing[]> = signal<readonly Pairing[]>([]);
  /** The invitation made last, with its link and QR code. */
  readonly invitation: Signal<NewPairing | null> = signal<NewPairing | null>(null);
  /** The user is asked whether to show the phrase. */
  readonly phraseAsked: Signal<boolean> = signal(false);
  /** The phrase, while shown. */
  readonly phrase: Signal<string | null> = signal<string | null>(null);
  /** The action under way, if any (its name). */
  readonly busy: Signal<string | null> = signal<string | null>(null);
  readonly message: Signal<SettingsMessage | null> = signal<SettingsMessage | null>(null);
  readonly restart: Signal<Restart | null> = signal<Restart | null>(null);
  /** How far the join under way has come (null when none runs). */
  readonly joinStage: Signal<JoinStage | null> = signal<JoinStage | null>(null);

  private readonly api: SettingsApi;
  private readonly timers: SettingsTimers;
  private readonly clock: () => number;
  private phraseTimer: unknown = null;
  private pollTimer: unknown = null;
  private joinTimer: unknown = null;
  private disposed = false;

  constructor(api: SettingsApi, options: { timers?: SettingsTimers; clock?: () => number } = {}) {
    this.api = api;
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.clock = options.clock ?? Date.now;
  }

  dispose(): void {
    this.disposed = true;
    this.hidePhrase();
    this.stopPolling();
    this.stopJoinPolling();
  }

  /** Whether the device holds the person key (it may invite, revoke, show the phrase). */
  get admin(): boolean {
    return this.overview.value?.admin === true;
  }

  /** Whether this device is the person's only one: it may be a fresh device to pair or recover. */
  get alone(): boolean {
    const overview = this.overview.value;
    return overview !== null && overview !== undefined && overview.devices.filter((d) => !d.revoked).length <= 1;
  }

  async load(): Promise<void> {
    try {
      const [overview, contacts] = await Promise.all([this.api.overview(), this.api.contacts()]);
      if (this.disposed) {
        return;
      }
      let pairings: readonly Pairing[] = [];
      if (overview?.admin === true) {
        pairings = await this.api.pairings().catch(() => []);
      }
      batch(() => {
        this.overview.value = overview;
        this.contacts.value = contacts;
        this.pairings.value = pairings;
      });
    } catch (error) {
      if (!this.disposed) {
        batch(() => {
          if (this.overview.value === undefined) {
            this.overview.value = null;
          }
          this.message.value = { kind: "error", text: personErrorText(error) };
        });
      }
    }
  }

  dismissMessage = (): void => {
    this.message.value = null;
  };

  /** Runs one action: busy while it runs, its failure as the message. */
  private async run<T>(
    name: string,
    action: () => Promise<T>,
    done?: string,
    errorText: (error: unknown) => string = personErrorText,
  ): Promise<T | undefined> {
    if (this.busy.value !== null) {
      return undefined;
    }
    batch(() => {
      this.busy.value = name;
      this.message.value = null;
    });
    try {
      const result = await action();
      if (done !== undefined && !this.disposed) {
        this.message.value = { kind: "info", text: done };
      }
      return result;
    } catch (error) {
      if (!this.disposed) {
        this.message.value = { kind: "error", text: errorText(error) };
      }
      return undefined;
    } finally {
      if (!this.disposed) {
        this.busy.value = null;
      }
    }
  }

  // ---- devices ----

  revoke = async (key: string): Promise<void> => {
    await this.run("revoke", async () => {
      await this.api.revoke(key);
      await this.load();
    }, "The device was revoked. It can no longer speak for you.");
  };

  setAnchor = async (anchor: boolean): Promise<void> => {
    await this.run("anchor", async () => {
      await this.api.setAnchor(anchor);
      await this.load();
    });
  };

  // ---- pairing ----

  invite = async (admin: boolean): Promise<void> => {
    const invitation = await this.run("invite", () => this.api.invite(admin));
    if (invitation !== undefined && !this.disposed) {
      this.invitation.value = invitation;
      await this.refreshPairings();
      this.startPolling();
    }
  };

  closeInvitation = (): void => {
    this.invitation.value = null;
    this.stopPolling();
  };

  confirmPairing = async (id: string): Promise<void> => {
    await this.run("confirm", async () => {
      await this.api.confirmPairing(id);
      await this.refreshPairings();
    }, "Confirmed. The new device gets the person key.");
  };

  cancelPairing = async (id: string): Promise<void> => {
    await this.run("cancel", async () => {
      await this.api.cancelPairing(id);
      if (this.invitation.value?.id === id) {
        this.closeInvitation();
      }
      await this.refreshPairings();
    });
  };

  private async refreshPairings(): Promise<void> {
    const pairings = await this.api.pairings().catch(() => null);
    if (pairings === null || this.disposed) {
      return;
    }
    this.pairings.value = pairings;
    const open = this.invitation.value;
    // The invitation was used: the device list has a new device.
    if (open !== null && !pairings.some((p) => p.id === open.id)) {
      this.closeInvitation();
      await this.load();
      this.message.value = { kind: "info", text: "The new device was paired." };
    }
  }

  private startPolling(): void {
    this.stopPolling();
    const tick = (): void => {
      this.pollTimer = null;
      if (this.disposed || this.invitation.value === null) {
        return;
      }
      if (this.invitation.value.expiresAt !== 0 && this.invitation.value.expiresAt < this.clock()) {
        this.closeInvitation();
        this.message.value = { kind: "info", text: "The invitation expired." };
        void this.refreshPairings();
        return;
      }
      void this.refreshPairings().finally(() => {
        if (!this.disposed && this.invitation.value !== null) {
          this.pollTimer = this.timers.setTimeout(tick, PAIRING_POLL_MS);
        }
      });
    };
    this.pollTimer = this.timers.setTimeout(tick, PAIRING_POLL_MS);
  }

  private stopPolling(): void {
    if (this.pollTimer !== null) {
      this.timers.clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  /** Pairs this device by a code or a link from another device of the person. */
  join = async (codeOrLink: string, name: string): Promise<boolean> => {
    let text = codeOrLink;
    if (looksLikePairLink(codeOrLink)) {
      if (!isPairLink(codeOrLink)) {
        this.message.value = { kind: "error", text: "Paste the whole link: it starts with resonator:pair/." };
        return false;
      }
    } else {
      const reading = parsePairCode(codeOrLink);
      if (!reading.ok) {
        this.message.value = { kind: "error", text: reading.error };
        return false;
      }
      text = reading.text;
    }
    if (this.busy.value !== null) {
      return false;
    }
    this.joinStage.value = looksLikePairLink(text) ? "verifying" : "finding";
    this.startJoinPolling();
    const joined = await this.run("join", () => this.api.join(text, name.trim()), undefined, joinErrorText);
    this.stopJoinPolling();
    if (this.disposed) {
      return false;
    }
    if (joined === undefined) {
      this.joinStage.value = null;
      return false;
    }
    batch(() => {
      this.joinStage.value = "done";
      this.restart.value = { reason: "paired", person: joined.person };
    });
    return true;
  };

  private startJoinPolling(): void {
    this.stopJoinPolling();
    const tick = (): void => {
      this.joinTimer = null;
      if (this.disposed || this.busy.value !== "join") {
        return;
      }
      void this.api
        .joinProgress()
        .catch(() => null)
        .then((stage) => {
          if (this.disposed || this.busy.value !== "join") {
            return;
          }
          if (stage !== null) {
            this.joinStage.value = stage;
          }
          this.joinTimer = this.timers.setTimeout(tick, JOIN_POLL_MS);
        });
    };
    this.joinTimer = this.timers.setTimeout(tick, JOIN_POLL_MS);
  }

  private stopJoinPolling(): void {
    if (this.joinTimer !== null) {
      this.timers.clearTimeout(this.joinTimer);
      this.joinTimer = null;
    }
  }

  // ---- recovery phrase ----

  askPhrase = (): void => {
    this.phraseAsked.value = true;
  };

  /** After the user confirmed: fetches the phrase and shows it for a while. */
  revealPhrase = async (): Promise<void> => {
    const phrase = await this.run("phrase", () => this.api.revealPhrase());
    batch(() => {
      this.phraseAsked.value = false;
      if (phrase !== undefined && !this.disposed) {
        this.phrase.value = phrase;
      }
    });
    if (this.phrase.value !== null) {
      this.phraseTimer = this.timers.setTimeout(() => this.hidePhrase(), PHRASE_SHOWN_MS);
    }
  };

  hidePhrase = (): void => {
    batch(() => {
      this.phrase.value = null;
      this.phraseAsked.value = false;
    });
    if (this.phraseTimer !== null) {
      this.timers.clearTimeout(this.phraseTimer);
      this.phraseTimer = null;
    }
  };

  /** Makes this (fresh) device the person of a recovery phrase. */
  recover = async (phrase: string, revokeOld: boolean, peers: readonly string[] = []): Promise<boolean> => {
    const words = phrase.trim().split(/\s+/).filter((w) => w !== "");
    if (words.length < 12) {
      this.message.value = { kind: "error", text: "Enter every word of the recovery phrase." };
      return false;
    }
    const person = await this.run("recover", () => this.api.recover(words.join(" "), revokeOld, peers));
    if (person === undefined || this.disposed) {
      return false;
    }
    this.restart.value = { reason: "recovered", person };
    return true;
  };

  // ---- co-hosts ----

  grantCoHost = async (domain: string): Promise<void> => {
    const name = domain.trim();
    if (name === "") {
      return;
    }
    const until = this.clock() + CO_HOST_DAYS * 24 * 3_600_000;
    await this.run("cohost", async () => {
      await this.api.coHost(name, until);
      await this.load();
    }, `${name} may host your waves while none of your devices is around.`);
  };

  withdrawCoHost = async (domain: string): Promise<void> => {
    await this.run("cohost", async () => {
      await this.api.coHost(domain, 0);
      await this.load();
    });
  };

  // ---- contacts ----

  addContact = async (addressOrLink: string): Promise<boolean> => {
    const text = addressOrLink.trim();
    if (text === "") {
      return false;
    }
    const added = await this.run("contact", async () => {
      const address = await this.api.addContact(text);
      this.contacts.value = await this.api.contacts();
      return address;
    });
    return added !== undefined;
  };

  removeContact = async (address: string): Promise<void> => {
    await this.run("contact", async () => {
      await this.api.removeContact(address);
      this.contacts.value = await this.api.contacts();
    });
  };
}

/** A failure of joining by a code or link, for people. */
export function joinErrorText(error: unknown): string {
  if (error instanceof PersonApiError) {
    if (error.status === 404) {
      return "Code not found or expired. Check the digits, or make a new code on your other device.";
    }
    const text = error.message.trim();
    if (text === "") {
      return personErrorText(error);
    }
    const sentence = text.charAt(0).toUpperCase() + text.slice(1);
    return /[.!?)]$/.test(sentence) ? sentence : `${sentence}.`;
  }
  return personErrorText(error);
}

/** "m:ss" left until `expiresAt`. */
export function remainingText(expiresAt: number, now: number): string {
  const seconds = Math.max(0, Math.ceil((expiresAt - now) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** "3 minutes ago" and the like, for a device's last sighting. */
export function lastSeenText(lastSeen: number | null, now: number, online: boolean): string {
  if (online) {
    return "online";
  }
  if (lastSeen === null) {
    return "not seen since this device started";
  }
  const seconds = Math.max(0, Math.round((now - lastSeen) / 1000));
  if (seconds < 60) {
    return "seen just now";
  }
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) {
    return `seen ${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  }
  const hours = Math.round(minutes / 60);
  if (hours < 48) {
    return `seen ${hours} hour${hours === 1 ? "" : "s"} ago`;
  }
  const days = Math.round(hours / 24);
  return `seen ${days} days ago`;
}

/** A device's name, or a short form of its key. */
export function deviceLabel(device: { readonly name: string; readonly key: string }): string {
  return device.name !== "" ? device.name : `device ${device.key.slice(0, 8)}`;
}

/** What the lease state says to people. */
export function leaseText(overview: PersonOverview, now: number): string {
  const lease = overview.lease;
  if (lease === null) {
    return "No device hosts your waves yet.";
  }
  if (lease.coHost !== null) {
    return `${lease.coHost} hosts your waves while your devices are away.`;
  }
  const holder = overview.devices.find((d) => d.key === lease.holder);
  const name = lease.heldHere ? "This device" : holder !== undefined ? deviceLabel(holder) : "Another device";
  if (lease.expired || lease.expiresAt < now) {
    return `${name} hosted your waves; its turn ran out and the next device online takes over.`;
  }
  return `${name} hosts your waves.`;
}
