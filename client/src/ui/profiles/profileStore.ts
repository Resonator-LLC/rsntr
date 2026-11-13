// Display names and avatars of participants, fetched from `/profile` in
// batches and cached for the session. Stands where `ProfileManagerImpl` and
// `RemoteProfileManagerImpl` of the Apache Wave web client stand.
//
// Reading a profile inside a Preact component (or a signals `effect`)
// subscribes it: the component re-renders when the fetched profile arrives.

import { type Signal, signal } from "@preact/signals";

export interface Profile {
  readonly address: string;
  /** The full name; until fetched, derived from the address. */
  readonly name: string;
  /** Same-origin URL of the avatar image. */
  readonly imageUrl: string;
}

/** The part of the HTTP API the store needs (`WaveClient` has it). */
export interface ProfileSource {
  fetchProfiles(addresses: readonly string[]): Promise<{
    readonly profiles: readonly { address: string; name: string; imageUrl: string }[];
  }>;
  avatarUrl(address: string, width: number, height: number): string;
}

/** The pixel size avatars are requested at (shown at up to 40 CSS px, 2x). */
const AVATAR_PX = 80;

/** `ProfileImpl`'s default name: the local part with a capital first letter. */
export function defaultName(address: string): string {
  const at = address.indexOf("@");
  const local = at > 0 ? address.slice(0, at) : address;
  if (local === "") {
    // The shared-domain participant `@example.com`.
    return address;
  }
  return local.charAt(0).toUpperCase() + local.slice(1);
}

/** `ProfileImpl.getFirstName`. */
export function firstName(name: string): string {
  const space = name.indexOf(" ");
  return space > 0 ? name.slice(0, space) : name;
}

export class ProfileStore {
  private readonly profiles = new Map<string, Signal<Profile>>();
  private pending = new Set<string>();
  private scheduled = false;

  constructor(
    private readonly source: ProfileSource,
    /** Resolves a server path (`imageUrl`) to a URL the page can load. */
    private readonly resolve: (path: string) => string = (path) => path,
  ) {}

  /** The profile of an address; a default at first, the fetched one later. */
  get(address: string): Profile {
    let entry = this.profiles.get(address);
    if (entry === undefined) {
      entry = signal<Profile>({
        address,
        name: defaultName(address),
        imageUrl: this.source.avatarUrl(address, AVATAR_PX, AVATAR_PX),
      });
      this.profiles.set(address, entry);
      this.pending.add(address);
      this.schedule();
    }
    return entry.value;
  }

  /** Forgets everything (on sign-out). */
  clear(): void {
    this.profiles.clear();
    this.pending.clear();
  }

  private schedule(): void {
    if (this.scheduled) {
      return;
    }
    this.scheduled = true;
    queueMicrotask(() => {
      this.scheduled = false;
      const addresses = [...this.pending];
      this.pending = new Set();
      if (addresses.length > 0) {
        void this.fetch(addresses);
      }
    });
  }

  private async fetch(addresses: string[]): Promise<void> {
    // Batches keep the query string short.
    for (let i = 0; i < addresses.length; i += 40) {
      try {
        const response = await this.source.fetchProfiles(addresses.slice(i, i + 40));
        for (const fetched of response.profiles) {
          const entry = this.profiles.get(fetched.address);
          if (entry !== undefined) {
            entry.value = {
              address: fetched.address,
              name: fetched.name !== "" ? fetched.name : defaultName(fetched.address),
              imageUrl: fetched.imageUrl !== "" ? this.resolve(fetched.imageUrl) : entry.value.imageUrl,
            };
          }
        }
      } catch {
        // The defaults stay; a later sign-in starts afresh.
      }
    }
  }
}
