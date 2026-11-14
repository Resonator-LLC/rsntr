// @vitest-environment happy-dom
// The settings page of a device node: its store against a fake node, and
// the page rendered.

import { render } from "preact";
import { act } from "preact/test-utils";
import { afterEach, describe, expect, test } from "vitest";

import type { Contact, JoinStage, NewPairing, Pairing, PersonOverview } from "../../net/person";
import { PersonApiError } from "../../net/person";
import { isSettingsFragment } from "../app/appStore";
import { PrefsStore } from "../app/prefs";
import { isRemoteWave } from "../wave/ContinueWave";
import { SettingsPage } from "./SettingsPage";
import {
  JOIN_POLL_MS,
  PAIRING_POLL_MS,
  PHRASE_SHOWN_MS,
  type SettingsApi,
  SettingsStore,
  type SettingsTimers,
  joinErrorText,
  lastSeenText,
  leaseText,
  remainingText,
} from "./settingsStore";

const PHRASE = Array.from({ length: 24 }, (_, i) => `word${i}`).join(" ");

function overview(admin = true, devices = 2): PersonOverview {
  const all = [
    { key: "dd", name: "phone", admin, revoked: false, self: true, online: false, lastSeen: null, holder: true, anchor: false },
    { key: "ee", name: "laptop", admin: false, revoked: false, self: false, online: false, lastSeen: 0, holder: false, anchor: false },
  ];
  return {
    domain: "pp",
    device: "dd",
    deviceName: "phone",
    admin,
    anchor: false,
    devices: all.slice(0, devices),
    lease: { holder: "dd", term: 1, expiresAt: 10_000_000, heldHere: true, expired: false, coHost: null },
    coHosts: [],
  };
}

class FakeNode implements SettingsApi {
  calls: string[] = [];
  view: PersonOverview | null = overview();
  contactList: Contact[] = [{ address: "zoe@a", name: "Zoe", added: true }];
  pending: Pairing[] = [];
  failNext: PersonApiError | null = null;
  /** The code new invitations get (null: none). */
  code: string | null = "482913-ocean-tiger";
  /** What `joinProgress` answers. */
  stage: JoinStage | null = null;
  /** While set, `join` waits for it. */
  joinGate: Promise<void> | null = null;

  private record(call: string): void {
    this.calls.push(call);
    const fail = this.failNext;
    if (fail !== null) {
      this.failNext = null;
      throw fail;
    }
  }

  async overview() {
    return this.view;
  }
  async contacts() {
    return [...this.contactList];
  }
  async pairings() {
    return [...this.pending];
  }
  async revoke(device: string) {
    this.record(`revoke ${device}`);
    if (this.view !== null) {
      this.view = { ...this.view, devices: this.view.devices.map((d) => (d.key === device ? { ...d, revoked: true } : d)) };
    }
  }
  async setAnchor(anchor: boolean) {
    this.record(`anchor ${anchor}`);
  }
  async revealPhrase() {
    this.record("phrase");
    return PHRASE;
  }
  async recover(phrase: string, revokeOld: boolean) {
    this.record(`recover ${phrase.split(" ").length} ${revokeOld}`);
    return "pp";
  }
  async coHost(domain: string, until: number) {
    this.record(`cohost ${domain} ${until}`);
  }
  async invite(admin: boolean): Promise<NewPairing> {
    this.record(`invite ${admin}`);
    const pairing = { id: "i1", admin, expiresAt: 600_000, state: "open", joinerName: null };
    this.pending.push(pairing);
    return { ...pairing, code: this.code, link: "resonator:pair/abc", qrSvg: '<svg class="qr"></svg>' };
  }
  async confirmPairing(id: string) {
    this.record(`confirm ${id}`);
  }
  async cancelPairing(id: string) {
    this.record(`cancel ${id}`);
    this.pending = this.pending.filter((p) => p.id !== id);
  }
  async join(link: string, name: string) {
    this.record(`join ${link} ${name}`);
    if (this.joinGate !== null) {
      await this.joinGate;
    }
    return { person: "pp", admin: false };
  }
  async joinProgress() {
    return this.stage;
  }
  async addContact(text: string) {
    this.record(`add ${text}`);
    const address = text.replace("resonator:contact/", "");
    this.contactList.push({ address, name: address, added: true });
    return address;
  }
  async removeContact(address: string) {
    this.record(`remove ${address}`);
    this.contactList = this.contactList.filter((c) => c.address !== address);
  }
}

class FakeTimers implements SettingsTimers {
  private next = 1;
  readonly tasks = new Map<number, { ms: number; run: () => void }>();
  setTimeout(callback: () => void, ms: number): unknown {
    const handle = this.next++;
    this.tasks.set(handle, { ms, run: callback });
    return handle;
  }
  clearTimeout(handle: unknown): void {
    this.tasks.delete(handle as number);
  }
  /** Runs the tasks due after `ms`. */
  fire(ms: number): void {
    for (const [handle, task] of [...this.tasks]) {
      if (task.ms <= ms) {
        this.tasks.delete(handle);
        task.run();
      }
    }
  }
}

const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

describe("SettingsStore", () => {
  test("loads the person, contacts and pairings", async () => {
    const node = new FakeNode();
    const store = new SettingsStore(node);
    expect(store.overview.value).toBeUndefined();
    await store.load();
    expect(store.overview.value?.domain).toBe("pp");
    expect(store.admin).toBe(true);
    expect(store.alone).toBe(false);
    expect(store.contacts.value.map((c) => c.address)).toEqual(["zoe@a"]);
  });

  test("the phrase is fetched only when confirmed, and dropped again", async () => {
    const node = new FakeNode();
    const timers = new FakeTimers();
    const store = new SettingsStore(node, { timers });
    await store.load();
    store.askPhrase();
    expect(node.calls).toEqual([]);
    expect(store.phraseAsked.value).toBe(true);
    await store.revealPhrase();
    expect(node.calls).toEqual(["phrase"]);
    expect(store.phrase.value).toBe(PHRASE);
    expect(store.phraseAsked.value).toBe(false);
    // Hidden after a while.
    timers.fire(PHRASE_SHOWN_MS);
    expect(store.phrase.value).toBeNull();
    await store.revealPhrase();
    store.dispose();
    expect(store.phrase.value).toBeNull();
    expect(timers.tasks.size).toBe(0);
  });

  test("an invitation is watched until the device paired", async () => {
    const node = new FakeNode();
    const timers = new FakeTimers();
    const store = new SettingsStore(node, { timers, clock: () => 0 });
    await store.load();
    await store.invite(false);
    expect(store.invitation.value?.link).toBe("resonator:pair/abc");
    expect(store.pairings.value.map((p) => p.id)).toEqual(["i1"]);
    // Asked for confirmation (an admin pairing): shown.
    node.pending = [{ ...node.pending[0]!, state: "requested", joinerName: "tablet" }];
    timers.fire(PAIRING_POLL_MS);
    await flush();
    expect(store.pairings.value[0]?.state).toBe("requested");
    await store.confirmPairing("i1");
    expect(node.calls).toContain("confirm i1");
    // Used: gone from the list; the invitation closes.
    node.pending = [];
    timers.fire(PAIRING_POLL_MS);
    await flush();
    await flush();
    expect(store.invitation.value).toBeNull();
    expect(store.message.value?.text).toBe("The new device was paired.");
    expect(timers.tasks.size).toBe(0);
  });

  test("an expired invitation closes", async () => {
    const node = new FakeNode();
    const timers = new FakeTimers();
    let now = 0;
    const store = new SettingsStore(node, { timers, clock: () => now });
    await store.load();
    await store.invite(true);
    now = 600_001;
    timers.fire(PAIRING_POLL_MS);
    expect(store.invitation.value).toBeNull();
    expect(store.message.value?.text).toBe("The invitation expired.");
  });

  test("joining and recovering restart the node; bad input is refused here", async () => {
    const node = new FakeNode();
    const store = new SettingsStore(node);
    await store.load();
    expect(await store.join("https://example.com", "tablet")).toBe(false);
    expect(store.message.value?.kind).toBe("error");
    expect(await store.recover("too few words", true)).toBe(false);
    expect(node.calls).toEqual([]);
    expect(await store.join(" resonator:pair/abc", " tablet ")).toBe(true);
    expect(node.calls).toEqual(["join  resonator:pair/abc tablet"]);
    expect(store.restart.value).toEqual({ reason: "paired", person: "pp" });
    const again = new SettingsStore(node);
    expect(await again.recover(`  ${PHRASE.replaceAll(" ", "\n ")} `, false)).toBe(true);
    expect(node.calls.at(-1)).toBe("recover 24 false");
    expect(again.restart.value?.reason).toBe("recovered");
  });

  test("joining by a code: read loosely, refused here when malformed, progress followed", async () => {
    const node = new FakeNode();
    const timers = new FakeTimers();
    const store = new SettingsStore(node, { timers });
    await store.load();
    expect(await store.join("48291-ocean-tiger", "")).toBe(false);
    expect(store.message.value?.text).toContain("6 digits and 2 words");
    expect(await store.join("482913 ocean tigr", "")).toBe(false);
    expect(store.message.value?.text).toBe('"tigr" is not one of the code words.');
    expect(node.calls).toEqual([]);

    let open = (): void => {};
    node.joinGate = new Promise((resolve) => (open = resolve));
    const joining = store.join("  482913 OCEAN tige ", "tablet");
    expect(store.joinStage.value).toBe("finding");
    expect(node.calls).toEqual(["join 482913-ocean-tiger tablet"]);
    node.stage = "waiting";
    timers.fire(JOIN_POLL_MS);
    await flush();
    expect(store.joinStage.value).toBe("waiting");
    open();
    expect(await joining).toBe(true);
    expect(store.joinStage.value).toBe("done");
    expect(store.restart.value?.reason).toBe("paired");
    expect(timers.tasks.size).toBe(0);
  });

  test("a code not found and a wrong code are told plainly", async () => {
    const node = new FakeNode();
    const store = new SettingsStore(node);
    await store.load();
    node.failNext = new PersonApiError(404, "code not found or expired: check the digits, or make a new code");
    expect(await store.join("482913-ocean-tiger", "")).toBe(false);
    expect(store.message.value?.text).toMatch(/^Code not found or expired\./);
    expect(store.joinStage.value).toBeNull();
    node.failNext = new PersonApiError(409, "wrong code (2 tries left)");
    expect(await store.join("482913-ocean-tiger", "")).toBe(false);
    expect(store.message.value?.text).toBe("Wrong code (2 tries left)");
    expect(joinErrorText(new PersonApiError(409, "wrong code; the code is used up now, make a new one"))).toBe(
      "Wrong code; the code is used up now, make a new one.",
    );
    expect(remainingText(600_000, 60_000)).toBe("9:00");
    expect(remainingText(1_000, 2_000)).toBe("0:00");
    expect(remainingText(61_500, 0)).toBe("1:02");
  });

  test("a refusal becomes the message; busy while an action runs", async () => {
    const node = new FakeNode();
    const store = new SettingsStore(node);
    await store.load();
    node.failNext = new PersonApiError(403, "Only an admin device can revoke.");
    const revoking = store.revoke("ee");
    expect(store.busy.value).toBe("revoke");
    // A second action meanwhile is ignored.
    await store.setAnchor(true);
    await revoking;
    expect(store.busy.value).toBeNull();
    expect(store.message.value).toEqual({ kind: "error", text: "Only an admin device can revoke." });
    expect(node.calls).toEqual(["revoke ee"]);
    await store.revoke("ee");
    expect(store.overview.value?.devices.find((d) => d.key === "ee")?.revoked).toBe(true);
  });

  test("contacts and co-hosts", async () => {
    const node = new FakeNode();
    const store = new SettingsStore(node, { clock: () => 1_000 });
    await store.load();
    expect(await store.addContact("  ")).toBe(false);
    expect(await store.addContact("resonator:contact/yuri@b")).toBe(true);
    expect(store.contacts.value.map((c) => c.address)).toEqual(["zoe@a", "yuri@b"]);
    await store.removeContact("zoe@a");
    expect(store.contacts.value.map((c) => c.address)).toEqual(["yuri@b"]);
    await store.grantCoHost(" friend.example ");
    await store.withdrawCoHost("friend.example");
    expect(node.calls.slice(-2)).toEqual([`cohost friend.example ${1_000 + 30 * 86_400_000}`, "cohost friend.example 0"]);
  });
});

describe("texts", () => {
  test("last seen", () => {
    expect(lastSeenText(null, 0, true)).toBe("online");
    expect(lastSeenText(null, 0, false)).toBe("not seen since this device started");
    expect(lastSeenText(0, 30_000, false)).toBe("seen just now");
    expect(lastSeenText(0, 60_000, false)).toBe("seen 1 minute ago");
    expect(lastSeenText(0, 3 * 3_600_000, false)).toBe("seen 3 hours ago");
    expect(lastSeenText(0, 5 * 86_400_000, false)).toBe("seen 5 days ago");
  });

  test("the lease", () => {
    const view = overview();
    expect(leaseText(view, 0)).toBe("This device hosts your waves.");
    expect(leaseText({ ...view, lease: { ...view.lease!, heldHere: false, holder: "ee" } }, 0)).toBe("laptop hosts your waves.");
    expect(leaseText({ ...view, lease: { ...view.lease!, coHost: "friend.example" } }, 0)).toContain("friend.example");
    expect(leaseText({ ...view, lease: null }, 0)).toBe("No device hosts your waves yet.");
    expect(leaseText(view, 20_000_000)).toContain("ran out");
  });

  test("the settings fragment and remote waves", () => {
    expect(isSettingsFragment("#settings")).toBe(true);
    expect(isSettingsFragment("settings")).toBe(true);
    expect(isSettingsFragment("#example.com/w+abc")).toBe(false);
    expect(isRemoteWave("pp", "qq")).toBe(true);
    expect(isRemoteWave("qq", "qq")).toBe(false);
    expect(isRemoteWave("pp", null)).toBe(false);
  });
});

describe("SettingsPage", () => {
  let root: HTMLElement | null = null;
  afterEach(() => {
    if (root !== null) {
      render(null, root);
      root.remove();
      root = null;
    }
  });

  async function mount(node: FakeNode, store?: SettingsStore) {
    root = document.createElement("div");
    document.body.append(root);
    let closed = 0;
    await act(async () => {
      render(
        <SettingsPage api={node} onClose={() => closed++} now={() => 60_000} {...(store !== undefined ? { store } : {})} />,
        root!,
      );
    });
    await act(flush);
    return { el: root, closed: () => closed };
  }

  const click = async (el: Element | null | undefined) => {
    expect(el).toBeTruthy();
    await act(async () => {
      (el as HTMLElement).click();
    });
    await act(flush);
  };

  test("an account on a server: the connection with Connect now and the address, nothing loaded", async () => {
    const node = new FakeNode();
    root = document.createElement("div");
    document.body.append(root);
    let connects = 0;
    const page = (status: "online" | "offline") => (
      <SettingsPage
        api={node}
        onClose={() => {}}
        device={false}
        address="ann@example.com"
        connection={{ status, text: status === "online" ? "Online" : "Offline", connectNow: () => connects++ }}
      />
    );
    await act(async () => render(page("offline"), root!));
    await act(flush);
    expect(root.querySelector("[data-testid=connection]")?.textContent).toBe("Offline");
    await click(root.querySelector("[data-action=connect-now]"));
    expect(connects).toBe(1);
    expect(root.querySelector("[data-testid=my-address]")?.textContent).toBe("ann@example.com");
    expect(root.querySelector("[data-section=devices]")).toBeNull();
    expect(node.calls).toEqual([]);
    await act(async () => render(page("online"), root!));
    expect(root.querySelector("[data-testid=connection]")?.textContent).toBe("Online");
    expect(root.querySelector("[data-action=connect-now]")).toBeNull();
  });

  test("You: the name and photo others see, saved and told to the rest of the app", async () => {
    const calls: string[] = [];
    let current = { address: "ann@example.com", name: "Ann", customName: false, imageUrl: "/ini/ann", hasAvatar: false };
    const api = {
      myProfile: async () => current,
      setMyName: async (name: string) => {
        calls.push(`name ${name}`);
        if (name.includes("\u202e")) {
          throw new Error("A name cannot hold control or text-direction characters.");
        }
        current = { ...current, name: name.trim() === "" ? "Ann" : name.trim(), customName: name.trim() !== "" };
        return current;
      },
      setMyAvatar: async (image: Blob) => {
        calls.push(`avatar ${image.size}`);
        current = { ...current, imageUrl: "/avatar/ann?v=1", hasAvatar: true };
        return current;
      },
      removeMyAvatar: async () => {
        calls.push("remove");
        current = { ...current, imageUrl: "/ini/ann", hasAvatar: false };
        return current;
      },
    };
    const told: string[] = [];
    root = document.createElement("div");
    document.body.append(root);
    await act(async () =>
      render(
        <SettingsPage
          api={new FakeNode()}
          onClose={() => {}}
          device={false}
          address="ann@example.com"
          profile={api}
          onProfileChanged={(p) => told.push(`${p.name} ${p.imageUrl}`)}
        />,
        root!,
      ),
    );
    await act(flush);
    const editor = root.querySelector("[data-testid=profile-editor]")!;
    const input = editor.querySelector<HTMLInputElement>("#settings-name")!;
    expect(input.value).toBe("");
    expect(input.placeholder).toBe("Ann");
    expect(editor.querySelector<HTMLButtonElement>("[data-action=save-name]")!.disabled).toBe(true);
    expect(editor.querySelector("[data-action=remove-photo]")).toBeNull();

    const type = async (text: string) => {
      await act(async () => {
        input.value = text;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      });
    };
    await type("Ann Lee");
    await act(async () => {
      editor.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    await act(flush);
    expect(calls).toEqual(["name Ann Lee"]);
    expect(told).toEqual(["Ann Lee /ini/ann"]);
    expect(editor.querySelector("[data-testid=profile-status]")!.textContent).toBe("Name saved.");

    await type("\u202eevil");
    await act(async () => {
      editor.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    await act(flush);
    expect(editor.querySelector("[data-testid=profile-status]")!.textContent).toContain("text-direction");
    expect(told).toHaveLength(1);

    const file = editor.querySelector<HTMLInputElement>("[data-testid=photo-input]")!;
    Object.defineProperty(file, "files", { value: [new File([new Uint8Array(10)], "me.png", { type: "image/png" })], configurable: true });
    await act(async () => {
      file.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(flush);
    expect(calls.at(-1)).toBe("avatar 10");
    expect(editor.querySelector<HTMLImageElement>("img.avatar-xl")!.getAttribute("src")).toBe("/avatar/ann?v=1");
    await click(editor.querySelector("[data-action=remove-photo]"));
    expect(calls.at(-1)).toBe("remove");
    expect(editor.querySelector("[data-action=remove-photo]")).toBeNull();
  });

  test("Accessibility: the choices change the preferences and the attributes of <html>", async () => {
    const prefs = new PrefsStore(null, null);
    let shortcuts = 0;
    const html = document.createElement("html");
    const stop = prefs.apply(html);
    root = document.createElement("div");
    document.body.append(root);
    await act(async () => render(<SettingsPage api={new FakeNode()} onClose={() => {}} device={false} prefs={prefs} onShortcuts={() => shortcuts++} />, root!));
    const section = root.querySelector("[data-section=accessibility]")!;
    expect(section).not.toBeNull();
    const size = section.querySelector<HTMLSelectElement>("select[data-pref=textSize]")!;
    expect(section.querySelector(`label[for=${size.id}]`)?.textContent).toBe("Text size");
    await act(async () => {
      size.value = "150";
      size.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(html.getAttribute("data-text-size")).toBe("150");
    const contrast = section.querySelector<HTMLSelectElement>("select[data-pref=contrast]")!;
    await act(async () => {
      contrast.value = "more";
      contrast.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(html.getAttribute("data-contrast")).toBe("more");
    await click(section.querySelector("input[data-pref=underlineLinks]"));
    expect(html.hasAttribute("data-underline-links")).toBe(true);
    expect(prefs.prefs.value).toMatchObject({ textSize: "150", contrast: "more", underlineLinks: true });
    const announce = section.querySelector<HTMLSelectElement>("select[data-pref=announce]")!;
    await act(async () => {
      announce.value = "replies";
      announce.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(html.getAttribute("data-announce")).toBe("replies");
    await click(section.querySelector("[data-action=shortcuts]"));
    expect(shortcuts).toBe(1);
    stop();
  });

  test("an admin device: devices, pairing with a QR code and link, the phrase behind a confirmation", async () => {
    const node = new FakeNode();
    const { el } = await mount(node);
    expect(el.querySelector(".settings-person")?.textContent).toBe("pp");
    const devices = [...el.querySelectorAll(".settings-device")];
    expect(devices.map((d) => d.getAttribute("data-key"))).toEqual(["dd", "ee"]);
    expect(devices[0]?.textContent).toContain("this device");
    expect(devices[0]?.textContent).toContain("hosts your waves");
    expect(devices[1]?.textContent).toContain("seen 1 minute ago");
    expect(el.querySelector(".settings-lease")?.textContent).toBe("This device hosts your waves.");
    // Not alone: no join or recover.
    expect(el.querySelector("[data-section=join]")).toBeNull();

    await click(el.querySelector("[data-action=pair]"));
    const code = el.querySelector("[data-testid=pair-code]");
    expect([...(code?.querySelectorAll(".settings-code-group") ?? [])].map((g) => g.textContent)).toEqual([
      "482913",
      "ocean",
      "tiger",
    ]);
    expect(code?.getAttribute("aria-label")).toBe("Pairing code 482913 ocean tiger");
    expect(el.querySelector("[data-testid=pair-remaining]")?.textContent).toBe("Expires in 9:00");
    expect(el.querySelector("[data-action=copy-code]")).not.toBeNull();
    expect((el.querySelector(".settings-pair-link") as HTMLInputElement).value).toBe("resonator:pair/abc");
    expect(el.querySelector(".settings-qr svg")).not.toBeNull();

    await click(el.querySelector("[data-action=ask-phrase]"));
    expect(node.calls).not.toContain("phrase");
    expect(el.querySelector("[role=alertdialog]")?.textContent).toContain("Anyone who sees");
    await click(el.querySelector("[data-action=reveal-phrase]"));
    expect(el.querySelectorAll(".settings-phrase li")).toHaveLength(24);
    await click(el.querySelector("[data-action=hide-phrase]"));
    expect(el.querySelector(".settings-phrase")).toBeNull();
    expect(el.textContent).not.toContain("word0");

    // Revoking asks first.
    await click(el.querySelector("[data-key=ee] [data-action=revoke]"));
    expect(node.calls).not.toContain("revoke ee");
    await click(el.querySelector("[data-key=ee] [data-action=confirm-revoke]"));
    expect(node.calls).toContain("revoke ee");
    expect(el.querySelector("[data-key=ee]")?.classList.contains("is-revoked")).toBe(true);
  });

  test("without a code the invitation offers the QR code and link only", async () => {
    const node = new FakeNode();
    node.code = null;
    const { el } = await mount(node);
    await click(el.querySelector("[data-action=pair]"));
    expect(el.querySelector("[data-testid=pair-code]")).toBeNull();
    expect(el.querySelector("[data-testid=pair-no-code]")?.textContent).toContain("Expires in 9:00");
    expect(el.querySelector(".settings-qr svg")).not.toBeNull();
  });

  test("a code is typed with word suggestions, checked as typed, and joined with progress", async () => {
    const node = new FakeNode();
    node.view = overview(true, 1);
    const { el } = await mount(node);
    expect(el.querySelector("[data-section=join] h3, #settings-join")?.textContent).toBe("Join with a code");
    const input = el.querySelector("input[name=pair-code]") as HTMLInputElement;
    expect(input.getAttribute("autocapitalize")).toBe("off");
    expect(input.getAttribute("autocorrect")).toBe("off");
    expect(input.getAttribute("inputmode")).toBe("text");
    const type = async (text: string) => {
      await act(async () => {
        input.value = text;
        input.dispatchEvent(new Event("input", { bubbles: true }));
      });
    };
    await type("482913-oce");
    const suggestions = [...el.querySelectorAll("[data-suggestion]")].map((b) => b.textContent);
    expect(suggestions).toEqual(["ocean"]);
    await click(el.querySelector("[data-suggestion=ocean]"));
    expect(input.value).toBe("482913-ocean-");
    await type("482913-ocean-tigr ");
    expect(el.querySelector(".settings-code-hint")?.textContent).toBe('"tigr" is not one of the code words.');
    expect(input.getAttribute("aria-invalid")).toBe("true");
    await type("482913-ocean-tiger");
    expect(el.querySelector(".settings-code-hint")?.textContent).toBe("");

    let open = (): void => {};
    node.joinGate = new Promise((resolve) => (open = resolve));
    await click(el.querySelector("[data-action=join]"));
    expect(node.calls).toEqual(["join 482913-ocean-tiger "]);
    const progress = el.querySelector(".settings-join-progress");
    expect(progress?.getAttribute("data-stage")).toBe("finding");
    expect(progress?.querySelector("[aria-current=step]")?.textContent).toBe("Finding your device");
    await act(async () => {
      open();
      await flush();
    });
    await act(flush);
    expect(el.querySelector(".settings-restart")?.textContent).toContain("now one of yours");
  });

  test("a device that is not an admin cannot pair, revoke or see the phrase", async () => {
    const node = new FakeNode();
    node.view = overview(false);
    const { el } = await mount(node);
    expect(el.querySelector("[data-section=pair]")).toBeNull();
    expect(el.querySelector("[data-section=phrase]")).toBeNull();
    expect(el.querySelector("[data-action=revoke]")).toBeNull();
  });

  test("a fresh device offers joining and recovering", async () => {
    const node = new FakeNode();
    node.view = overview(true, 1);
    const { el } = await mount(node);
    expect(el.querySelector("[data-section=join]")).not.toBeNull();
    expect(el.querySelector("[data-section=recover]")).not.toBeNull();
    const input = el.querySelector("input[name=pair-link]") as HTMLInputElement;
    await act(async () => {
      input.value = "resonator:pair/abc";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(el.querySelector("[data-action=join]"));
    expect(node.calls).toEqual(["join resonator:pair/abc "]);
    expect(el.querySelector(".settings-restart")?.textContent).toContain("now one of yours");
  });

  test("contacts are added and removed; back closes", async () => {
    const node = new FakeNode();
    const { el, closed } = await mount(node);
    const input = el.querySelector("input[name=contact]") as HTMLInputElement;
    await act(async () => {
      input.value = "yuri@b";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(el.querySelector("[data-action=add-contact]"));
    expect([...el.querySelectorAll(".settings-contact")].map((c) => c.getAttribute("data-address"))).toEqual([
      "zoe@a",
      "yuri@b",
    ]);
    expect(input.value).toBe("");
    await click(el.querySelector('[data-address="zoe@a"] [data-action=remove-contact]'));
    expect([...el.querySelectorAll(".settings-contact")].map((c) => c.getAttribute("data-address"))).toEqual(["yuri@b"]);
    await click(el.querySelector(".settings-back"));
    expect(closed()).toBe(1);
  });

});
