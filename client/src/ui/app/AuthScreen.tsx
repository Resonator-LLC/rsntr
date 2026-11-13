// The sign-in and registration screens, after the pages of Apache Wave
// (`AuthenticationPage.gxp`, `UserRegistrationPage.gxp`; Apache License
// 2.0): a user name completed with the server's domain, a password, and on
// registration the password again.

import { useEffect, useRef, useState } from "preact/hooks";

import type { AppStore } from "./appStore";
import { validateRegistration, validateSignIn } from "./authMessages";
import { copyText } from "./clipboard";
import { WaveMark } from "./icons";

interface FormProps {
  store: AppStore;
  domain: string;
}

/** Domains longer than this (key domains are 52 characters) are shown shortened. */
const LONG_DOMAIN = 28;

/** A domain short enough to sit next to something: `ps9n4s…s8qgo` for a long one. */
export function displayDomain(domain: string): string {
  return domain.length > LONG_DOMAIN ? `${domain.slice(0, 6)}…${domain.slice(-5)}` : domain;
}

/**
 * A long (key) domain under the user name field: whole, on two lines, with
 * a button that copies the address (or the domain while no name is typed).
 */
function LongDomain({ domain, name }: { domain: string; name: string }) {
  const [copied, setCopied] = useState(false);
  const textRef = useRef<HTMLElement>(null);
  const copy = async (): Promise<void> => {
    const text = name !== "" ? `${name}@${domain}` : domain;
    if (await copyText(text, textRef.current)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }
  };
  return (
    <div class="auth-long-domain">
      <span class="auth-long-domain-text">
        @<span ref={textRef}>{domain}</span>
      </span>
      <button
        class="btn auth-copy"
        type="button"
        onClick={copy}
        aria-label={name !== "" ? "Copy your address" : "Copy the server's address"}
      >
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

/** The user name field with the domain it is completed with. */
function AddressField(props: {
  domain: string;
  value: string;
  onInput: (value: string) => void;
  invalid: boolean;
  inputRef: { current: HTMLInputElement | null };
}) {
  const { domain, value } = props;
  const bare = !value.includes("@");
  const long = domain.length > LONG_DOMAIN;
  return (
    <div class="field">
      <label for="address">Username</label>
      <div class={long ? "auth-address auth-address-stacked" : "auth-address"}>
        <input
          ref={props.inputRef}
          class="input"
          id="address"
          name="address"
          type="text"
          autocomplete="username"
          autocapitalize="none"
          autocorrect="off"
          spellcheck={false}
          inputMode="email"
          required
          aria-describedby="address-hint"
          aria-invalid={props.invalid}
          value={value}
          onInput={(event) => props.onInput(event.currentTarget.value)}
        />
        {long ? (
          <LongDomain domain={domain} name={bare ? value.trim() : ""} />
        ) : bare && domain !== "" ? (
          <span class="auth-domain" aria-hidden="true">
            @{domain}
          </span>
        ) : null}
      </div>
      {long ? (
        <span class="auth-hint" id="address-hint">
          Your address is your username followed by this server's key.
        </span>
      ) : (
        <span class="auth-hint" id="address-hint">
          {domain !== "" ? `Your address is your username @${domain}` : "Your wave address"}
        </span>
      )}
    </div>
  );
}

function SignInForm({ store, domain }: FormProps) {
  const [address, setAddress] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const addressRef = useRef<HTMLInputElement>(null);
  const passwordRef = useRef<HTMLInputElement>(null);
  const registrationEnabled = store.session.value?.registrationEnabled ?? false;
  const notice = store.authNotice.value;

  useEffect(() => {
    addressRef.current?.focus();
  }, []);

  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) {
      return;
    }
    const invalid = validateSignIn(address, password);
    if (invalid !== null) {
      setError(invalid);
      (address.trim() === "" ? addressRef : passwordRef).current?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    const failure = await store.signIn(address, password);
    if (failure !== null) {
      // Otherwise the screen is gone.
      setBusy(false);
      setError(failure);
      passwordRef.current?.focus();
      passwordRef.current?.select();
    }
  };

  return (
    <form class="auth-form" onSubmit={submit} noValidate aria-labelledby="auth-title">
      <h1 class="auth-title" id="auth-title">
        Sign in
      </h1>
      {notice !== null ? (
        <p
          class={notice.kind === "error" ? "notice notice-error" : "notice auth-notice-info"}
          role={notice.kind === "error" ? "alert" : "status"}
        >
          {notice.text}
        </p>
      ) : null}
      <AddressField
        domain={domain}
        value={address}
        onInput={setAddress}
        invalid={error !== null}
        inputRef={addressRef}
      />
      <div class="field">
        <label for="password">Password</label>
        <input
          ref={passwordRef}
          class="input"
          id="password"
          name="password"
          type="password"
          autocomplete="current-password"
          required
          aria-invalid={error !== null}
          value={password}
          onInput={(event) => setPassword(event.currentTarget.value)}
        />
      </div>
      {error !== null ? (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      ) : null}
      <div class="auth-actions">
        <button class="btn btn-primary" type="submit" disabled={busy}>
          {busy ? "Signing in..." : "Sign in"}
        </button>
      </div>
      {registrationEnabled ? (
        <p class="auth-switch">
          Don't have an account?{" "}
          <button class="auth-link" type="button" onClick={() => store.showAuthScreen("register")}>
            Register a new account
          </button>
        </p>
      ) : null}
    </form>
  );
}

/** Links to the other servers where one may register instead. */
function OtherServers({ domains }: { domains: readonly string[] }) {
  return (
    <p class="auth-switch auth-other-servers">
      Prefer another address?{" "}
      {domains.map((other, index) => (
        <span key={other}>
          {index > 0 ? ", " : null}
          <a class="auth-link" href={`https://${other}/`}>
            Register @{other}
          </a>
        </span>
      ))}
    </p>
  );
}

function RegisterForm({ store, domain }: FormProps) {
  const [address, setAddress] = useState("");
  const [password, setPassword] = useState("");
  const [verify, setVerify] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const addressRef = useRef<HTMLInputElement>(null);
  const otherServers = store.session.value?.otherServers;

  useEffect(() => {
    addressRef.current?.focus();
  }, []);

  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy) {
      return;
    }
    const invalid = validateRegistration(address, password, verify);
    if (invalid !== null) {
      setError(invalid);
      return;
    }
    setBusy(true);
    setError(null);
    const failure = await store.register(address, password);
    if (failure !== null) {
      setBusy(false);
      setError(failure);
    }
  };

  return (
    <form class="auth-form" onSubmit={submit} noValidate aria-labelledby="auth-title">
      <h1 class="auth-title" id="auth-title">
        Register a new account
      </h1>
      <AddressField
        domain={domain}
        value={address}
        onInput={setAddress}
        invalid={error !== null}
        inputRef={addressRef}
      />
      <div class="field">
        <label for="password">Password</label>
        <input
          class="input"
          id="password"
          name="password"
          type="password"
          autocomplete="new-password"
          required
          value={password}
          onInput={(event) => setPassword(event.currentTarget.value)}
        />
      </div>
      <div class="field">
        <label for="verifypass">Re-enter password</label>
        <input
          class="input"
          id="verifypass"
          name="verifypass"
          type="password"
          autocomplete="new-password"
          required
          value={verify}
          onInput={(event) => setVerify(event.currentTarget.value)}
        />
      </div>
      {error !== null ? (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      ) : null}
      <div class="auth-actions">
        <button class="btn" type="button" disabled={busy} onClick={() => store.showAuthScreen("signIn")}>
          Cancel
        </button>
        <button class="btn btn-primary" type="submit" disabled={busy}>
          {busy ? "Registering..." : "Register"}
        </button>
      </div>
      {otherServers !== undefined ? <OtherServers domains={otherServers} /> : null}
    </form>
  );
}

export function AuthScreen({ store }: { store: AppStore }) {
  const session = store.session.value;
  const domain = session?.domain ?? "";
  const register = store.authScreen.value === "register" && (session?.registrationEnabled ?? false);
  return (
    <main class="auth">
      <div class="auth-card">
        <div class="auth-brand">
          <WaveMark />
          <span class="auth-product">Resonator</span>
          {domain !== "" ? (
            <span class="auth-brand-domain" title={`@${domain}`}>
              @{displayDomain(domain)}
            </span>
          ) : null}
        </div>
        {session?.device !== undefined && !session.device.allowRemote ? (
          // Device mode: the app wrapping this node signs in (it opens the
          // page with its token); there is nothing to type here.
          <div class="auth-device" role="status">
            <h1 class="auth-title">Open Resonator from its app</h1>
            <p>
              This Resonator runs on this device for {session.device.account}. The app signs you in;
              reopen it from there.
            </p>
          </div>
        ) : register ? (
          <RegisterForm key="register" store={store} domain={domain} />
        ) : (
          <SignInForm key="signIn" store={store} domain={domain} />
        )}
      </div>
      <p class="auth-tagline">Communicate and collaborate in real time.</p>
    </main>
  );
}
