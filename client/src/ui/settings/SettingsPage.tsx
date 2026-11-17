// The settings, laid out as an app's settings screen: the connection; on
// any server the address and contacts; on a device node (person mode) also
// this person and device, the devices (revoke), hosting
// (lease, anchor), pairing (a short code, a QR code and link) and joining
// by a code or link, the
// recovery phrase (admin devices, behind a confirmation) and recovering
// from one, co-hosts, and contacts.

import { CHOICES, type Prefs, type PrefsStore } from "../app/prefs";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";

import "../styles/settings.css";
import { copyText } from "../app/clipboard";

import { codeGroups, codeHint, completeCodeWord, suggestCodeWords } from "../../net/pairCode";
import type { JoinStage, PersonDevice, PersonOverview } from "../../net/person";
import type { MyProfile } from "../../net/http";
import { BackIcon } from "../wave/icons";
import {
  type SettingsApi,
  SettingsStore,
  deviceLabel,
  lastSeenText,
  leaseText,
  remainingText,
} from "./settingsStore";

export interface SettingsPageProps {
  readonly api: SettingsApi;
  readonly onClose: () => void;
  /** The signed-in address (`name@<person domain>`), shown whole with Copy. */
  readonly address?: string;
  /** False: an account on a server, not a device node, so there are no
   *  device or contact settings to load (default true). */
  readonly device?: boolean;
  /** The connection to the server, with a way to try again now. */
  readonly connection?: {
    readonly status: "online" | "offline" | "connecting";
    readonly text: string;
    readonly connectNow: () => void;
  };
  /** Display and accessibility preferences; the section shows with them. */
  readonly prefs?: PrefsStore;
  /** Opens the list of keyboard shortcuts. */
  readonly onShortcuts?: () => void;
  /** The user's name and avatar; the editor shows with it. */
  readonly profile?: ProfileEditorApi;
  /** Told about a changed own profile (to show it everywhere at once). */
  readonly onProfileChanged?: (profile: MyProfile) => void;
  /** For tests. */
  readonly store?: SettingsStore;
  readonly now?: () => number;
}

function Section(props: { readonly title: string; readonly id: string; readonly children: preact.ComponentChildren }) {
  return (
    <section class="settings-section" aria-labelledby={`settings-${props.id}`} data-section={props.id}>
      <h3 class="settings-section-title" id={`settings-${props.id}`}>
        {props.title}
      </h3>
      <div class="settings-card">{props.children}</div>
    </section>
  );
}

function Device(props: {
  readonly device: PersonDevice;
  readonly store: SettingsStore;
  readonly now: number;
}) {
  const { device, store } = props;
  const [confirming, setConfirming] = useState(false);
  const busy = store.busy.value !== null;
  return (
    <li class={`settings-row settings-device${device.revoked ? " is-revoked" : ""}`} data-key={device.key}>
      <div class="settings-row-main">
        <span class="settings-device-name">{deviceLabel(device)}</span>
        <span class="settings-badges">
          {device.self ? <span class="settings-badge">this device</span> : null}
          {device.admin ? <span class="settings-badge">admin</span> : null}
          {device.holder ? <span class="settings-badge settings-badge-accent">hosts your waves</span> : null}
          {device.anchor ? <span class="settings-badge">always on</span> : null}
          {device.revoked ? <span class="settings-badge settings-badge-danger">revoked</span> : null}
        </span>
        {device.self || device.revoked ? null : (
          <span class="settings-row-sub">{lastSeenText(device.lastSeen, props.now, device.online)}</span>
        )}
      </div>
      {store.admin && !device.self && !device.revoked ? (
        confirming ? (
          <span class="settings-row-actions">
            <button
              type="button"
              class="btn btn-danger"
              data-action="confirm-revoke"
              disabled={busy}
              onClick={() => {
                setConfirming(false);
                void store.revoke(device.key);
              }}
            >
              Revoke
            </button>
            <button type="button" class="btn btn-ghost" onClick={() => setConfirming(false)}>
              Keep
            </button>
          </span>
        ) : (
          <button
            type="button"
            class="btn btn-ghost settings-danger-text"
            data-action="revoke"
            disabled={busy}
            onClick={() => setConfirming(true)}
          >
            Revoke…
          </button>
        )
      ) : null}
    </li>
  );
}

/** The code of an invitation, large: the number, then each word. */
function PairCode({ code }: { readonly code: string }) {
  const groups = codeGroups(code);
  return (
    <p class="settings-pair-code" data-testid="pair-code" aria-label={`Pairing code ${groups.join(" ")}`}>
      {groups.map((group, i) => (
        <span key={i} class="settings-code-part">
          {i > 0 ? (
            <span class="settings-code-sep" aria-hidden="true">
              -
            </span>
          ) : null}
          <span class={`settings-code-group${i === 0 ? " is-number" : ""}`}>{group}</span>
        </span>
      ))}
    </p>
  );
}

function Pairing({ store, now }: { readonly store: SettingsStore; readonly now: () => number }) {
  const invitation = store.invitation.value;
  const pairings = store.pairings.value;
  const busy = store.busy.value !== null;
  const [copied, setCopied] = useState<"code" | "link" | null>(null);
  const [clock, setClock] = useState(now);
  useEffect(() => {
    if (invitation === null) {
      return;
    }
    setClock(now());
    const timer = setInterval(() => setClock(now()), 1_000);
    return () => clearInterval(timer);
  }, [invitation]);
  useEffect(() => setCopied(null), [invitation]);
  const copy = (what: "code" | "link", text: string): void => {
    void navigator.clipboard?.writeText(text).then(
      () => setCopied(what),
      () => {},
    );
  };
  return (
    <>
      {invitation === null ? (
        <p class="settings-help">
          Make a code here, then on the new device open Settings, choose "Join with a code" and type it. It works once, for
          ten minutes.
        </p>
      ) : null}
      {invitation !== null ? (
        <div class="settings-invitation" data-admin={invitation.admin ? "true" : "false"}>
          {invitation.code !== null ? (
            <div class="settings-code-box">
              <p class="settings-help">On the new device, choose "Join with a code" and type:</p>
              <PairCode code={invitation.code} />
              <div class="settings-code-meta">
                <span class="settings-row-sub" data-testid="pair-remaining" role="timer">
                  Expires in {remainingText(invitation.expiresAt, clock)}
                </span>
                <button type="button" class="btn" data-action="copy-code" onClick={() => copy("code", invitation.code!)}>
                  {copied === "code" ? "Copied" : "Copy code"}
                </button>
              </div>
            </div>
          ) : (
            <p class="settings-help" data-testid="pair-no-code">
              No code this time (this device cannot reach the pairing rendezvous): scan the QR code or paste the link on the
              new device. Expires in {remainingText(invitation.expiresAt, clock)}.
            </p>
          )}
          <p class="settings-help settings-or">{invitation.code !== null ? "Or scan this QR code, or paste the link:" : ""}</p>
          {invitation.qrSvg !== "" ? (
            <div
              class="settings-qr"
              role="img"
              aria-label="QR code of the pairing link"
              // The server's own SVG, made from the link.
              dangerouslySetInnerHTML={{ __html: invitation.qrSvg }}
            />
          ) : null}
          <label class="field">
            <span>Pairing link</span>
            <input class="input settings-pair-link" readOnly value={invitation.link} onFocus={(e) => e.currentTarget.select()} />
          </label>
          <div class="settings-actions">
            <button type="button" class="btn" data-action="copy-link" onClick={() => copy("link", invitation.link)}>
              {copied === "link" ? "Copied" : "Copy link"}
            </button>
            <button type="button" class="btn btn-ghost" data-action="cancel-invitation" disabled={busy} onClick={() => void store.cancelPairing(invitation.id)}>
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <div class="settings-actions">
          <button type="button" class="btn btn-primary" data-action="pair" disabled={busy} onClick={() => void store.invite(false)}>
            Pair a device
          </button>
          <button
            type="button"
            class="btn"
            data-action="pair-admin"
            disabled={busy}
            title="The new device gets the person key too; you confirm it here"
            onClick={() => void store.invite(true)}
          >
            Pair an admin device
          </button>
        </div>
      )}
      {pairings.filter((p) => p.state === "requested").map((p) => (
        <div class="settings-request notice" key={p.id} data-pairing={p.id}>
          <span>
            {p.joinerName !== null && p.joinerName !== "" ? `"${p.joinerName}"` : "A device"} asks to become an admin device
            and get the person key.
          </span>
          <span class="settings-row-actions">
            <button type="button" class="btn btn-primary" data-action="confirm-pairing" disabled={busy} onClick={() => void store.confirmPairing(p.id)}>
              Confirm
            </button>
            <button type="button" class="btn btn-ghost" disabled={busy} onClick={() => void store.cancelPairing(p.id)}>
              Refuse
            </button>
          </span>
        </div>
      ))}
    </>
  );
}

const JOIN_STEPS: readonly { readonly stage: JoinStage; readonly text: string }[] = [
  { stage: "finding", text: "Finding your device" },
  { stage: "verifying", text: "Verifying the code" },
  { stage: "waiting", text: "Waiting for confirmation on your device" },
  { stage: "done", text: "Done" },
];

function JoinProgress({ stage }: { readonly stage: JoinStage }) {
  const at = JOIN_STEPS.findIndex((step) => step.stage === stage);
  return (
    <ol class="settings-join-progress" data-stage={stage} aria-live="polite">
      {JOIN_STEPS.map((step, i) => (
        <li key={step.stage} class={i < at ? "is-done" : i === at ? "is-current" : ""} aria-current={i === at ? "step" : undefined}>
          {step.text}
        </li>
      ))}
    </ol>
  );
}

function Join({ store }: { readonly store: SettingsStore }) {
  const [code, setCode] = useState("");
  const [link, setLink] = useState("");
  const [name, setName] = useState("");
  const busy = store.busy.value !== null;
  const joining = store.busy.value === "join";
  const stage = store.joinStage.value;
  const hint = codeHint(code);
  const suggestions = suggestCodeWords(code);
  const text = code.trim() !== "" ? code : link;
  return (
    <form
      class="settings-form"
      onSubmit={(event) => {
        event.preventDefault();
        void store.join(text, name);
      }}
    >
      <p class="settings-help">
        Make this device one of yours: on a device you already use, open Settings and choose "Pair a device", then type the
        code it shows. What is on this device now is set aside.
      </p>
      <label class="field">
        <span>Pairing code</span>
        <input
          class="input settings-code-input"
          name="pair-code"
          type="text"
          inputMode="text"
          enterKeyHint="go"
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellcheck={false}
          placeholder="482913-ocean-tiger"
          aria-describedby="settings-code-hint"
          aria-invalid={hint !== null ? "true" : undefined}
          disabled={joining}
          value={code}
          onInput={(e) => setCode(e.currentTarget.value)}
        />
      </label>
      {suggestions.length > 0 ? (
        <div class="settings-suggestions" role="group" aria-label="Code words">
          {suggestions.map((word) => (
            <button
              type="button"
              class="btn btn-ghost settings-suggestion"
              key={word}
              data-suggestion={word}
              onClick={() => setCode(completeCodeWord(code, word))}
            >
              {word}
            </button>
          ))}
        </div>
      ) : null}
      <p id="settings-code-hint" class={`settings-code-hint${hint !== null ? " is-error" : ""}`} aria-live="polite">
        {hint ?? ""}
      </p>
      <label class="field">
        <span>Or a pairing link</span>
        <input
          class="input"
          name="pair-link"
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellcheck={false}
          placeholder="resonator:pair/…"
          disabled={joining}
          value={link}
          onInput={(e) => setLink(e.currentTarget.value)}
        />
      </label>
      <label class="field">
        <span>Name of this device</span>
        <input class="input" name="device-name" placeholder="Phone" value={name} onInput={(e) => setName(e.currentTarget.value)} />
      </label>
      {joining && stage !== null ? <JoinProgress stage={stage} /> : null}
      <div class="settings-actions">
        <button type="submit" class="btn btn-primary" data-action="join" disabled={busy || text.trim() === ""}>
          {joining ? "Joining…" : "Join"}
        </button>
      </div>
    </form>
  );
}

function Phrase({ store }: { readonly store: SettingsStore }) {
  const phrase = store.phrase.value;
  const busy = store.busy.value !== null;
  if (phrase !== null) {
    return (
      <div class="settings-phrase-shown">
        <p class="settings-help">Write these words down, in order, and keep them somewhere safe. They are hidden again in a minute.</p>
        <ol class="settings-phrase" data-testid="phrase">
          {phrase.split(" ").map((word, i) => (
            <li key={i}>{word}</li>
          ))}
        </ol>
        <div class="settings-actions">
          <button type="button" class="btn" data-action="hide-phrase" onClick={store.hidePhrase}>
            Hide
          </button>
        </div>
      </div>
    );
  }
  if (store.phraseAsked.value) {
    return (
      <div class="settings-confirm notice" role="alertdialog" aria-label="Show the recovery phrase?">
        <p>
          Anyone who sees the recovery phrase can take over your person and all your waves. Make sure nobody is watching your
          screen.
        </p>
        <div class="settings-actions">
          <button type="button" class="btn btn-danger" data-action="reveal-phrase" disabled={busy} onClick={() => void store.revealPhrase()}>
            Show the phrase
          </button>
          <button type="button" class="btn btn-ghost" onClick={store.hidePhrase}>
            Cancel
          </button>
        </div>
      </div>
    );
  }
  return (
    <>
      <p class="settings-help">The recovery phrase brings your person back on a new device if you lose all of yours.</p>
      <div class="settings-actions">
        <button type="button" class="btn" data-action="ask-phrase" onClick={store.askPhrase}>
          Show recovery phrase…
        </button>
      </div>
    </>
  );
}

function Recover({ store }: { readonly store: SettingsStore }) {
  const [phrase, setPhrase] = useState("");
  const [revokeOld, setRevokeOld] = useState(true);
  const busy = store.busy.value !== null;
  return (
    <form
      class="settings-form"
      onSubmit={(event) => {
        event.preventDefault();
        void store.recover(phrase, revokeOld).then((done) => {
          if (done) {
            setPhrase("");
          }
        });
      }}
    >
      <p class="settings-help">
        Lost your devices? Enter your recovery phrase: this device becomes yours again and fetches your waves back from the
        people you share them with.
      </p>
      <label class="field">
        <span>Recovery phrase</span>
        <textarea
          class="input settings-phrase-input"
          name="recovery-phrase"
          rows={3}
          autoComplete="off"
          autoCapitalize="none"
          spellcheck={false}
          value={phrase}
          onInput={(e) => setPhrase(e.currentTarget.value)}
        />
      </label>
      <label class="settings-check">
        <input type="checkbox" checked={revokeOld} onChange={(e) => setRevokeOld(e.currentTarget.checked)} />
        <span>Revoke all my other devices (they were lost or stolen)</span>
      </label>
      <div class="settings-actions">
        <button type="submit" class="btn btn-primary" data-action="recover" disabled={busy || phrase.trim() === ""}>
          {store.busy.value === "recover" ? "Recovering…" : "Recover"}
        </button>
      </div>
    </form>
  );
}

function Hosting({ store, overview, now }: { readonly store: SettingsStore; readonly overview: PersonOverview; readonly now: number }) {
  const [domain, setDomain] = useState("");
  const busy = store.busy.value !== null;
  return (
    <>
      <p class="settings-lease" data-held={overview.lease?.heldHere === true ? "true" : "false"}>
        {leaseText(overview, now)}
      </p>
      <label class="settings-check">
        <input
          type="checkbox"
          data-action="anchor"
          checked={overview.anchor}
          disabled={busy}
          onChange={(e) => void store.setAnchor(e.currentTarget.checked)}
        />
        <span>This device is always on: prefer it to host my waves</span>
      </label>
      {store.admin ? (
        <>
          <h4 class="settings-subtitle">Co-hosts</h4>
          <p class="settings-help">
            A server you trust (a friend's, or your own always-on one) may host your waves while none of your devices is
            around.
          </p>
          {overview.coHosts.length > 0 ? (
            <ul class="settings-list">
              {overview.coHosts.map((grant) => (
                <li class="settings-row" key={grant.domain} data-cohost={grant.domain}>
                  <div class="settings-row-main">
                    <span class="settings-mono">{grant.domain}</span>
                    <span class="settings-row-sub">
                      {grant.active ? `until ${new Date(grant.until).toLocaleDateString()}` : "withdrawn"}
                    </span>
                  </div>
                  {grant.active ? (
                    <button type="button" class="btn btn-ghost" disabled={busy} onClick={() => void store.withdrawCoHost(grant.domain)}>
                      Withdraw
                    </button>
                  ) : null}
                </li>
              ))}
            </ul>
          ) : null}
          <form
            class="settings-inline-form"
            onSubmit={(event) => {
              event.preventDefault();
              void store.grantCoHost(domain).then(() => setDomain(""));
            }}
          >
            <input
              class="input"
              name="cohost-domain"
              placeholder="Server domain"
              autoComplete="off"
              autoCapitalize="none"
              value={domain}
              onInput={(e) => setDomain(e.currentTarget.value)}
            />
            <button type="submit" class="btn" disabled={busy || domain.trim() === ""}>
              Allow
            </button>
          </form>
        </>
      ) : null}
    </>
  );
}

function Contacts({ store }: { readonly store: SettingsStore }) {
  const [text, setText] = useState("");
  const busy = store.busy.value !== null;
  const contacts = store.contacts.value;
  return (
    <>
      <form
        class="settings-inline-form"
        onSubmit={(event) => {
          event.preventDefault();
          void store.addContact(text).then((added) => {
            if (added) {
              setText("");
            }
          });
        }}
      >
        <input
          class="input"
          name="contact"
          placeholder="Address or resonator:contact/ link"
          autoComplete="off"
          autoCapitalize="none"
          value={text}
          onInput={(e) => setText(e.currentTarget.value)}
        />
        <button type="submit" class="btn" data-action="add-contact" disabled={busy || text.trim() === ""}>
          Add
        </button>
      </form>
      {contacts.length === 0 ? (
        <p class="settings-help">No contacts yet. People you share a wave with show up here too.</p>
      ) : (
        <ul class="settings-list settings-contacts">
          {contacts.map((contact) => (
            <li class="settings-row settings-contact" key={contact.address} data-address={contact.address}>
              <div class="settings-row-main">
                <span>{contact.name !== contact.address ? contact.name : contact.address.split("@")[0]}</span>
                <span class="settings-row-sub settings-mono">{contact.address}</span>
              </div>
              {contact.added ? (
                <button
                  type="button"
                  class="btn btn-ghost"
                  data-action="remove-contact"
                  aria-label={`Remove ${contact.address}`}
                  disabled={busy}
                  onClick={() => void store.removeContact(contact.address)}
                >
                  Remove
                </button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

/** The calls of the name and avatar editor (`WaveHttpApi` has them). */
export interface ProfileEditorApi {
  myProfile(): Promise<MyProfile>;
  setMyName(name: string): Promise<MyProfile>;
  setMyAvatar(image: Blob): Promise<MyProfile>;
  removeMyAvatar(): Promise<MyProfile>;
}

/** Largest picture accepted (the server's limit). */
const MAX_PHOTO_BYTES = 5 * 1024 * 1024;

/** The name and photo others see next to what the user writes. */
function ProfileEditor(props: { readonly api: ProfileEditorApi; readonly onChanged?: (profile: MyProfile) => void }) {
  const { api, onChanged } = props;
  const [profile, setProfile] = useState<MyProfile | null>(null);
  const [draft, setDraft] = useState("");
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    let live = true;
    api.myProfile().then(
      (loaded) => {
        if (live) {
          setProfile(loaded);
          setDraft(loaded.customName ? loaded.name : "");
        }
      },
      () => live && setStatus("Could not load your name and photo."),
    );
    return () => {
      live = false;
    };
  }, [api]);
  const run = async (change: () => Promise<MyProfile>, done: string): Promise<void> => {
    setBusy(true);
    setStatus("");
    try {
      const changed = await change();
      setProfile(changed);
      setDraft(changed.customName ? changed.name : "");
      onChanged?.(changed);
      setStatus(done);
    } catch (error) {
      setStatus(error instanceof Error && error.message !== "" ? error.message : "That did not work. Try again.");
    } finally {
      setBusy(false);
    }
  };
  const pick = (event: Event): void => {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (file === undefined) {
      return;
    }
    if (file.size > MAX_PHOTO_BYTES) {
      setStatus("That picture is larger than 5 MB.");
      return;
    }
    void run(() => api.setMyAvatar(file), "Photo changed.");
  };
  const unchanged = profile !== null && draft.trim() === (profile.customName ? profile.name : "");
  return (
    <div class="settings-profile" data-testid="profile-editor">
      <div class="settings-row settings-profile-photo">
        {profile !== null ? (
          <img class="avatar avatar-xl" src={profile.imageUrl} alt="" width={64} height={64} />
        ) : (
          <span class="avatar avatar-xl" aria-hidden="true" />
        )}
        <div class="settings-row-main">
          <span>Photo</span>
          <span class="settings-row-sub">Shown next to what you write.</span>
        </div>
        <span class="settings-row-actions">
          <input
            ref={fileRef}
            type="file"
            accept="image/png,image/jpeg,image/gif,image/webp"
            class="visually-hidden"
            tabIndex={-1}
            aria-hidden="true"
            data-testid="photo-input"
            onChange={pick}
          />
          <button
            type="button"
            class="btn"
            data-action="change-photo"
            disabled={busy || profile === null}
            onClick={() => fileRef.current?.click()}
          >
            {profile?.hasAvatar === true ? "Change photo" : "Add photo"}
          </button>
          {profile?.hasAvatar === true ? (
            <button
              type="button"
              class="btn btn-ghost"
              data-action="remove-photo"
              disabled={busy}
              onClick={() => void run(() => api.removeMyAvatar(), "Photo removed.")}
            >
              Remove
            </button>
          ) : null}
        </span>
      </div>
      <form
        class="settings-profile-name"
        onSubmit={(event) => {
          event.preventDefault();
          void run(() => api.setMyName(draft), draft.trim() === "" ? "Name reset." : "Name saved.");
        }}
      >
        <span class="settings-profile-label">
          <label for="settings-name">Name</label>
          <span class="settings-profile-hint" id="settings-name-hint">
            Others see it with your address. Leave it empty to use the name from your address.
          </span>
        </span>
        <span class="settings-inline-form">
          <input
            id="settings-name"
            class="input"
            type="text"
            maxLength={64}
            autoComplete="name"
            aria-describedby="settings-name-hint"
            placeholder={profile !== null && !profile.customName ? profile.name : ""}
            value={draft}
            disabled={profile === null}
            onInput={(event) => setDraft((event.currentTarget as HTMLInputElement).value)}
          />
          <button type="submit" class="btn btn-primary" data-action="save-name" disabled={busy || profile === null || unchanged}>
            Save
          </button>
        </span>
      </form>
      <p class="settings-profile-status" role="status" data-testid="profile-status">
        {status}
      </p>
    </div>
  );
}

/** The whole address, wrapped as needed, selectable, with a Copy button. */
function Address({ address }: { readonly address: string }) {
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);
  const textRef = useRef<HTMLSpanElement>(null);
  const copy = async (): Promise<void> => {
    setCopied((await copyText(address, textRef.current)) ? "yes" : "failed");
    setTimeout(() => setCopied(null), 1500);
  };
  return (
    <div class="settings-row settings-address-row">
      <div class="settings-row-main">
        <span>Your address</span>
        <span ref={textRef} class="settings-row-sub settings-mono settings-address" data-testid="my-address">
          {address}
        </span>
      </div>
      <button type="button" class="btn settings-copy" data-action="copy-address" onClick={() => void copy()}>
        {copied === "yes" ? "Copied" : copied === "failed" ? "Select and copy" : "Copy"}
      </button>
    </div>
  );
}

const PREF_LABELS = {
  textSize: { system: "As the system", "100": "100%", "125": "125%", "150": "150%", "200": "200%" },
  theme: { system: "As the system", light: "Light", dark: "Dark" },
  contrast: { system: "As the system", normal: "Normal", more: "High" },
  motion: { system: "As the system", reduce: "Reduce" },
  spacing: { normal: "Normal", reading: "More, for reading" },
  targets: { normal: "Normal", large: "Large" },
} as const;

const PREF_NAMES: Record<keyof typeof PREF_LABELS, string> = {
  textSize: "Text size",
  theme: "Theme",
  contrast: "Contrast",
  motion: "Motion",
  spacing: "Text spacing",
  targets: "Buttons",
};

/** Text size, theme, contrast, motion, spacing, targets and links. */
function Accessibility({ prefs, onShortcuts }: { readonly prefs: PrefsStore; readonly onShortcuts?: () => void }) {
  const current = prefs.prefs.value;
  const select = <K extends keyof typeof PREF_LABELS>(key: K) => (
    <div class="settings-row settings-pref" key={key}>
      <label class="settings-row-main" for={`pref-${key}`}>
        {PREF_NAMES[key]}
      </label>
      <select
        id={`pref-${key}`}
        class="input settings-select"
        data-pref={key}
        value={current[key]}
        onChange={(e) => prefs.set(key, e.currentTarget.value as Prefs[K])}
      >
        {(CHOICES[key] as readonly string[]).map((value) => (
          <option key={value} value={value}>
            {(PREF_LABELS[key] as Record<string, string>)[value]}
          </option>
        ))}
      </select>
    </div>
  );
  return (
    <>
      {select("textSize")}
      {select("theme")}
      {select("contrast")}
      {select("motion")}
      {select("spacing")}
      {select("targets")}
      <div class="settings-row">
        <label class="settings-check">
          <input
            type="checkbox"
            data-pref="underlineLinks"
            checked={current.underlineLinks}
            onChange={(e) => prefs.set("underlineLinks", e.currentTarget.checked)}
          />
          <span>Underline links</span>
        </label>
      </div>
      {onShortcuts !== undefined ? (
        <div class="settings-row">
          <span class="settings-row-main">Keyboard shortcuts</span>
          <button type="button" class="btn" data-action="shortcuts" onClick={onShortcuts}>
            Show
          </button>
        </div>
      ) : null}
      <p class="settings-help settings-preview" data-testid="prefs-preview">
        Preview: the venue is booked for Friday, details at <a href="https://rsntr.net" target="_blank" rel="noreferrer">rsntr.net</a>.
      </p>
    </>
  );
}

function Connection({ connection }: { readonly connection: NonNullable<SettingsPageProps["connection"]> }) {
  return (
    <div class="settings-row">
      <div class="settings-row-main">
        <span>Server</span>
        <span class={`settings-row-sub settings-net is-${connection.status}`} data-testid="connection">
          <span class="settings-net-dot" aria-hidden="true" />
          {connection.text}
        </span>
      </div>
      {connection.status !== "online" ? (
        <button type="button" class="btn" data-action="connect-now" onClick={connection.connectNow}>
          Connect now
        </button>
      ) : null}
    </div>
  );
}

export function SettingsPage(props: SettingsPageProps) {
  const store = useMemo(() => props.store ?? new SettingsStore(props.api), [props.store, props.api]);
  const [now, setNow] = useState(() => (props.now ?? Date.now)());
  const device = props.device ?? true;
  useEffect(() => {
    if (device) {
      void store.load();
    }
    const timer = setInterval(() => setNow((props.now ?? Date.now)()), 30_000);
    return () => {
      clearInterval(timer);
      store.dispose();
    };
  }, [store, device]);

  const overview = store.overview.value;
  const message = store.message.value;
  const restart = store.restart.value;

  let body;
  if (!device) {
    body =
      props.address !== undefined && props.address !== "" ? (
        <Section title="You" id="you">
          {props.profile !== undefined ? <ProfileEditor api={props.profile} onChanged={props.onProfileChanged} /> : null}
          <Address address={props.address} />
        </Section>
      ) : null;
  } else if (restart !== null) {
    body = (
      <div class="settings-restart notice" role="status">
        <p>
          {restart.reason === "paired" ? "This device is now one of yours" : "Your person is back on this device"}. It is
          restarting: open Resonator again in a moment.
        </p>
        <button type="button" class="btn btn-primary" onClick={() => location.reload()}>
          Reload
        </button>
      </div>
    );
  } else if (overview === undefined) {
    body = (
      <p class="settings-loading" role="status">
        Loading…
      </p>
    );
  } else if (overview === null) {
    body = (
      <>
        {props.address !== undefined && props.address !== "" ? (
          <Section title="You" id="you">
            {props.profile !== undefined ? <ProfileEditor api={props.profile} onChanged={props.onProfileChanged} /> : null}
            <Address address={props.address} />
          </Section>
        ) : null}
        <Section title="Contacts" id="contacts">
          <Contacts store={store} />
        </Section>
      </>
    );
  } else {
    const devices = [...overview.devices].sort(
      (a, b) => Number(b.self) - Number(a.self) || Number(a.revoked) - Number(b.revoked) || deviceLabel(a).localeCompare(deviceLabel(b)),
    );
    body = (
      <>
        <Section title="You" id="you">
          {props.profile !== undefined ? <ProfileEditor api={props.profile} onChanged={props.onProfileChanged} /> : null}
          {props.address !== undefined && props.address !== "" ? <Address address={props.address} /> : null}
          <div class="settings-row">
            <div class="settings-row-main">
              <span>Your person</span>
              <span class="settings-row-sub settings-mono settings-person" title={overview.domain}>
                {overview.domain}
              </span>
            </div>
          </div>
          <div class="settings-row">
            <div class="settings-row-main">
              <span>This device</span>
              <span class="settings-row-sub">
                {overview.deviceName !== "" ? overview.deviceName : "unnamed"}
                {overview.admin ? " · admin" : ""}
              </span>
            </div>
          </div>
        </Section>
        <Section title="Devices" id="devices">
          <ul class="settings-list">
            {devices.map((device) => (
              <Device key={device.key} device={device} store={store} now={now} />
            ))}
          </ul>
        </Section>
        <Section title="Hosting" id="hosting">
          <Hosting store={store} overview={overview} now={now} />
        </Section>
        {store.admin ? (
          <Section title="Pair a device" id="pair">
            <Pairing store={store} now={props.now ?? Date.now} />
          </Section>
        ) : null}
        {store.admin ? (
          <Section title="Recovery phrase" id="phrase">
            <Phrase store={store} />
          </Section>
        ) : null}
        <Section title="Contacts" id="contacts">
          <Contacts store={store} />
        </Section>
        {store.alone ? (
          <>
            <Section title="Join with a code" id="join">
              <Join store={store} />
            </Section>
            <Section title="Recover from a phrase" id="recover">
              <Recover store={store} />
            </Section>
          </>
        ) : null}
      </>
    );
  }

  return (
    <section class="settings-page" aria-label="Settings">
      <header class="settings-header">
        <button type="button" class="icon-btn settings-back" aria-label="Back to the waves" onClick={props.onClose}>
          <BackIcon />
        </button>
        <h2 class="settings-title">Settings</h2>
      </header>
      <div class="settings-scroll">
        <div class="settings-body">
          {message !== null ? (
            <div class={`notice settings-message${message.kind === "error" ? " notice-error" : " notice-info"}`} role={message.kind === "error" ? "alert" : "status"}>
              <span>{message.text}</span>
              <button type="button" class="btn btn-ghost" onClick={store.dismissMessage}>
                Dismiss
              </button>
            </div>
          ) : null}
          {props.connection !== undefined ? (
            <Section title="Connection" id="connection">
              <Connection connection={props.connection} />
            </Section>
          ) : null}
          {props.prefs !== undefined ? (
            <Section title="Accessibility" id="accessibility">
              <Accessibility prefs={props.prefs} onShortcuts={props.onShortcuts} />
            </Section>
          ) : null}
          {body}
        </div>
      </div>
    </section>
  );
}
