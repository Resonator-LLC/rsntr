// The bar at the top: product, saved state, and one button that shows the
// connection and opens Settings (where the address is), sign out
// (`TopBar.gxp`, `SavedStateIndicator` and the network status of
// `WebClient` in Apache Wave; Apache License 2.0).

import { type AppStore, NET_STATUS_TEXT, SAVED_TEXT, UNSAVED_TEXT, netStatus } from "./appStore";
import { HostPendingIndicator } from "./HostPendingIndicator";
import { SettingsIcon, SignOutIcon, WaveMark } from "./icons";

export function TopBar({ store }: { store: AppStore }) {
  // Device mode: the app wrapping the node signs in, and there is nothing
  // to sign out of.
  const device = store.session.value?.device;
  const unsaved = store.unsavedShown.value;
  const net = netStatus(store.connection.value);

  const signOut = (): void => {
    if (
      store.hasUnsavedWork() &&
      !window.confirm("Some changes have not been saved yet. Sign out anyway?")
    ) {
      return;
    }
    void store.signOut();
  };

  return (
    <header class="topbar">
      <a class="topbar-brand" href="#" onClick={(event) => {
        event.preventDefault();
        store.closeWave();
      }}>
        <WaveMark />
        <h1 class="topbar-product">Resonator</h1>
      </a>
      <div class="topbar-status">
        <span class={`topbar-saved ${unsaved ? "is-unsaved" : "is-saved"}`} title="Whether your changes have reached the server">
          {unsaved ? UNSAVED_TEXT : SAVED_TEXT}
        </span>
        <HostPendingIndicator count={store.awaitingHost} />
        {store.update?.available.value === true ? (
          <button
            class="topbar-update"
            type="button"
            title="A newer version of Resonator is on the server"
            onClick={() => store.update?.reloadNow()}
          >
            New version · Reload
          </button>
        ) : null}
        <button
          class={`topbar-net topbar-settings is-${net}`}
          type="button"
          aria-label={`${NET_STATUS_TEXT[net]}, settings`}
          title="Settings"
          aria-pressed={store.settingsOpen.value}
          onClick={() => (store.settingsOpen.value ? store.closeSettings() : store.openSettings())}
        >
          <span class="topbar-net-dot" aria-hidden="true" />
          <span class="topbar-net-text">{NET_STATUS_TEXT[net]}</span>
          <SettingsIcon />
        </button>
      </div>
      {device === undefined ? (
        <button class="topbar-signout" type="button" onClick={signOut} aria-label="Sign out" title="Sign out">
          <SignOutIcon />
          <span class="topbar-signout-text">Sign out</span>
        </button>
      ) : null}
    </header>
  );
}
