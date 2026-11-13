// Person mode on a device node: the person and its devices, pairing,
// recovery, co-hosts and contacts (`/person...`, `/contacts`), and
// "continue as new wave" (`/waves/host-status`, `/waves/continue`).
// See `crates/resonator-server/src/http/person_handlers.rs`.
//
// Nothing here is cached: the recovery phrase in particular is fetched
// for one showing and dropped by whoever asked.

import type { HttpTransport, RequestOptions } from "./http";
import { NetworkError } from "./errors";

/** The start of a pairing link (`resonator:pair/<...>`). */
export const PAIR_LINK_PREFIX = "resonator:pair/";

/** A refusal of a person request: the HTTP status and the server's message. */
export class PersonApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "PersonApiError";
  }
}

/** A device of the person. */
export interface PersonDevice {
  /** Its key, 52 characters. */
  readonly key: string;
  readonly name: string;
  /** It holds the person key. */
  readonly admin: boolean;
  readonly revoked: boolean;
  /** The device showing the page. */
  readonly self: boolean;
  /** Connected to this device now. */
  readonly online: boolean;
  /** When it was last seen here (milliseconds), if since this device started. */
  readonly lastSeen: number | null;
  /** It holds the host lease. */
  readonly holder: boolean;
  readonly anchor: boolean;
}

/** The host lease of the person. */
export interface PersonLease {
  readonly holder: string;
  readonly term: number;
  readonly expiresAt: number;
  readonly heldHere: boolean;
  readonly expired: boolean;
  /** Held by a co-host of this domain; null if by a device of the person. */
  readonly coHost: string | null;
}

/** A server the person lets host its waves while no device is around. */
export interface PersonCoHost {
  readonly domain: string;
  readonly until: number;
  readonly active: boolean;
}

/** What `GET /person` tells. */
export interface PersonOverview {
  /** The person domain. */
  readonly domain: string;
  /** This device's key and name. */
  readonly device: string;
  readonly deviceName: string;
  /** This device holds the person key. */
  readonly admin: boolean;
  /** This device prefers to hold the lease (always on). */
  readonly anchor: boolean;
  readonly devices: readonly PersonDevice[];
  readonly lease: PersonLease | null;
  readonly coHosts: readonly PersonCoHost[];
}

/** A pairing invitation. */
export interface Pairing {
  readonly id: string;
  /** The new device gets the person key (needs confirmation here). */
  readonly admin: boolean;
  readonly expiresAt: number;
  /** `open`, `requested` (waits for confirmation) or `confirmed`. */
  readonly state: string;
  /** The name the new device gave, once it asked. */
  readonly joinerName: string | null;
}

/**
 * A new invitation: its short code (`482913-ocean-tiger`; null when the
 * device cannot publish codes), its link and the QR code of the link (SVG
 * markup). Never log the code or the link.
 */
export interface NewPairing extends Pairing {
  readonly code: string | null;
  readonly link: string;
  readonly qrSvg: string;
}

/** How far joining by a code or link has come. */
export type JoinStage = "finding" | "verifying" | "waiting" | "done";

const JOIN_STAGES: readonly string[] = ["finding", "verifying", "waiting", "done"];

/** A contact of the device's account. */
export interface Contact {
  readonly address: string;
  readonly name: string;
  /** Added by hand (it can be removed), not met on a wave. */
  readonly added: boolean;
}

/** What `GET /waves/host-status` tells of a wave. */
export interface HostStatus {
  /** The domain that hosts it. */
  readonly host: string;
  /** Hosted by this device. */
  readonly local: boolean;
  /** Its host is gone: it may be continued as a new wave. */
  readonly frozen: boolean;
  readonly reason: string;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const str = (value: unknown): string => (typeof value === "string" ? value : "");
const num = (value: unknown): number => (typeof value === "number" && Number.isFinite(value) ? value : 0);
const bool = (value: unknown): boolean => value === true;

function parseDevice(value: unknown): PersonDevice | null {
  if (!isObject(value) || typeof value["key"] !== "string") {
    return null;
  }
  return {
    key: value["key"],
    name: str(value["name"]),
    admin: bool(value["admin"]),
    revoked: bool(value["revoked"]),
    self: bool(value["this"]),
    online: bool(value["online"]),
    lastSeen: typeof value["last_seen"] === "number" ? value["last_seen"] : null,
    holder: bool(value["holder"]),
    anchor: bool(value["anchor"]),
  };
}

/** The body of `GET /person`; null if it is not one. */
export function parseOverview(body: unknown): PersonOverview | null {
  if (!isObject(body) || typeof body["domain"] !== "string") {
    return null;
  }
  const lease = body["lease"];
  const devices = Array.isArray(body["devices"]) ? body["devices"] : [];
  const coHosts = Array.isArray(body["co_hosts"]) ? body["co_hosts"] : [];
  return {
    domain: body["domain"],
    device: str(body["device"]),
    deviceName: str(body["device_name"]),
    admin: bool(body["admin"]),
    anchor: bool(body["anchor"]),
    devices: devices.map(parseDevice).filter((d): d is PersonDevice => d !== null),
    lease: isObject(lease)
      ? {
          holder: str(lease["holder"]),
          term: num(lease["term"]),
          expiresAt: num(lease["expires_at"]),
          heldHere: bool(lease["held_here"]),
          expired: bool(lease["expired"]),
          coHost: typeof lease["co_host"] === "string" ? lease["co_host"] : null,
        }
      : null,
    coHosts: coHosts.filter(isObject).map((grant) => ({
      domain: str(grant["domain"]),
      until: num(grant["until"]),
      active: bool(grant["active"]),
    })),
  };
}

/** A pairing of `/person/pair` answers; null if it is not one. */
export function parsePairing(value: unknown): Pairing | null {
  if (!isObject(value) || typeof value["id"] !== "string") {
    return null;
  }
  return {
    id: value["id"],
    admin: bool(value["admin"]),
    expiresAt: num(value["expires_at"]),
    state: str(value["state"]),
    joinerName: typeof value["joiner_name"] === "string" ? value["joiner_name"] : null,
  };
}

/** Whether `text` is a pairing link. */
export function isPairLink(text: string): boolean {
  return text.trim().startsWith(PAIR_LINK_PREFIX) && text.trim().length > PAIR_LINK_PREFIX.length;
}

/** The person endpoints over a transport. */
export class PersonApi {
  constructor(private readonly transport: HttpTransport) {}

  private async call(path: string, options: RequestOptions = {}): Promise<Record<string, unknown>> {
    const response = await this.transport.request(path, options);
    let body: unknown;
    try {
      body = JSON.parse(response.text) as unknown;
    } catch {
      body = undefined;
    }
    if (response.status < 200 || response.status >= 300) {
      const message = isObject(body) && typeof body["message"] === "string" ? body["message"] : "";
      throw new PersonApiError(response.status, message !== "" ? message : `HTTP ${response.status}`);
    }
    return isObject(body) ? body : {};
  }

  /** `GET /person`; null outside person mode (404). */
  async overview(): Promise<PersonOverview | null> {
    try {
      return parseOverview(await this.call("/person"));
    } catch (error) {
      if (error instanceof PersonApiError && error.status === 404) {
        return null;
      }
      throw error;
    }
  }

  async revoke(device: string): Promise<void> {
    await this.call("/person/revoke", { method: "POST", json: { device } });
  }

  async setAnchor(anchor: boolean): Promise<void> {
    await this.call("/person/anchor", { method: "POST", json: { anchor } });
  }

  /**
   * The recovery phrase (admin devices), after the user confirmed. The
   * caller shows it once and forgets it.
   */
  async revealPhrase(): Promise<string> {
    return str((await this.call("/person/phrase", { method: "POST", json: { confirm: true } }))["phrase"]);
  }

  /** Makes this fresh device the person of `phrase`; the node restarts. */
  async recover(phrase: string, revokeOld: boolean, peers: readonly string[] = []): Promise<string> {
    const body = await this.call("/person/recover", {
      method: "POST",
      json: { phrase, revoke_old: revokeOld, peers },
    });
    return str(body["person"]);
  }

  /** Grants `domain` co-hosting until `until` (milliseconds; 0 withdraws). */
  async coHost(domain: string, until: number): Promise<void> {
    await this.call("/person/cohost", { method: "POST", json: { domain, until } });
  }

  async invite(admin: boolean): Promise<NewPairing> {
    const body = await this.call("/person/pair", { method: "POST", json: { admin } });
    const pairing = parsePairing(body);
    if (pairing === null || typeof body["link"] !== "string") {
      throw new PersonApiError(500, "The invitation could not be read.");
    }
    const code = typeof body["code"] === "string" && body["code"] !== "" ? body["code"] : null;
    return { ...pairing, code, link: body["link"], qrSvg: str(body["qr"]) };
  }

  async pairings(): Promise<Pairing[]> {
    const list = (await this.call("/person/pair"))["invitations"];
    return Array.isArray(list) ? list.map(parsePairing).filter((p): p is Pairing => p !== null) : [];
  }

  async confirmPairing(id: string): Promise<void> {
    await this.call(`/person/pair/${encodeURIComponent(id)}/confirm`, { method: "POST" });
  }

  async cancelPairing(id: string): Promise<void> {
    await this.call(`/person/pair/${encodeURIComponent(id)}`, { method: "DELETE" });
  }

  /**
   * Pairs this device by a pairing link or code; the node restarts as a
   * device of that person.
   */
  async join(codeOrLink: string, name: string): Promise<{ person: string; admin: boolean }> {
    const text = codeOrLink.trim();
    const json = isPairLink(text) ? { link: text, name } : { code: text, name };
    const body = await this.call("/person/pair/join", { method: "POST", json });
    return { person: str(body["person"]), admin: bool(body["admin"]) };
  }

  /** How far the join under way has come; null if none runs. */
  async joinProgress(): Promise<JoinStage | null> {
    const stage = (await this.call("/person/pair/join"))["stage"];
    return typeof stage === "string" && JOIN_STAGES.includes(stage) ? (stage as JoinStage) : null;
  }

  /** `GET /contacts`: every contact, added ones marked. */
  async contacts(): Promise<Contact[]> {
    let body: Record<string, unknown>;
    try {
      body = await this.call("/contacts?query=&limit=500");
    } catch (error) {
      if (error instanceof PersonApiError && error.status === 404) {
        return [];
      }
      throw error;
    }
    const accounts = Array.isArray(body["accounts"]) ? body["accounts"] : [];
    return accounts.filter(isObject).flatMap((account) =>
      typeof account["address"] === "string"
        ? [
            {
              address: account["address"],
              name: typeof account["name"] === "string" ? account["name"] : account["address"],
              added: bool(account["added"]),
            },
          ]
        : [],
    );
  }

  /** Adds a contact by address or `resonator:contact/` link; the address added. */
  async addContact(addressOrLink: string): Promise<string> {
    const text = addressOrLink.trim();
    const body = text.startsWith("resonator:contact/") ? { link: text } : { address: text };
    return str((await this.call("/contacts", { method: "POST", json: body }))["address"]);
  }

  async removeContact(address: string): Promise<void> {
    await this.call(`/contacts/${encodeURIComponent(address)}`, { method: "DELETE" });
  }

  async hostStatus(waveId: string): Promise<HostStatus> {
    const body = await this.call(`/waves/host-status?wave=${encodeURIComponent(waveId)}`);
    return {
      host: str(body["host"]),
      local: bool(body["local"]),
      frozen: bool(body["frozen"]),
      reason: str(body["reason"]),
    };
  }

  /** Continues a wave whose host is gone as a new wave; its id. */
  async continueWave(waveId: string): Promise<string> {
    const wave = str((await this.call("/waves/continue", { method: "POST", json: { wave: waveId } }))["wave"]);
    if (wave === "") {
      throw new PersonApiError(500, "The new wave could not be read.");
    }
    return wave;
  }
}

/** A message for people from a failure of the person API. */
export function personErrorText(error: unknown): string {
  if (error instanceof PersonApiError) {
    return error.message;
  }
  if (error instanceof NetworkError) {
    return "The device could not be reached.";
  }
  return "Something went wrong.";
}
