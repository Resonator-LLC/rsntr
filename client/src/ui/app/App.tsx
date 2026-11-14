// The application: the sign-in screen, or the top bar over the search
// panel (left) and the wave panel (right) as in the Apache Wave web client
// (`WebClient.ui.xml`). At phone widths one panel shows at a time: the
// list while no wave is open, the wave otherwise.

import { useEffect } from "preact/hooks";

import { SearchPanel } from "../search/SearchPanel";
import { SettingsPage } from "../settings/SettingsPage";
import { WavePanel } from "../wave/WavePanel";
import { type AppStore, NET_STATUS_TEXT, netStatus } from "./appStore";
import { AuthScreen } from "./AuthScreen";
import { WaveMark } from "./icons";
import { ShortcutsDialog, typingIn } from "./ShortcutsDialog";
import { TopBar } from "./TopBar";

function Shell({ store }: { store: AppStore }) {
  const route = store.route.value;
  const problem = store.problem.value;
  const waveOpen = route.waveId !== null;
  // The settings page instead of the panels.
  const settings = store.settingsOpen.value;
  const net = netStatus(store.connection.value);
  return (
    <div class="app">
      {store.shortcutsOpen.value ? <ShortcutsDialog onClose={() => (store.shortcutsOpen.value = false)} /> : null}
      <TopBar store={store} />
      {problem !== null ? (
        <div class="app-problem notice notice-error" role="alert">
          <span>{problem}</span>
          <button class="btn btn-ghost" type="button" onClick={store.dismissProblem}>
            Dismiss
          </button>
        </div>
      ) : null}
      {settings ? (
        <main class="app-body app-body-settings" id="main">
          <SettingsPage
            api={store.client.api.person}
            address={store.session.value?.address ?? ""}
            connection={{ status: net, text: NET_STATUS_TEXT[net], connectNow: store.reconnect }}
            device={store.session.value?.device !== undefined}
            prefs={store.prefs ?? undefined}
            onShortcuts={() => (store.shortcutsOpen.value = true)}
            onClose={store.closeSettings}
          />
        </main>
      ) : (
      // One main landmark around both panels: on phones only one shows.
      <main class={`app-body ${waveOpen ? "app-body-wave" : "app-body-list"}`} id="main">
        <div class="app-search">
          <SearchPanel
            client={store.client}
            profiles={store.profiles}
            selectedWaveId={route.waveId}
            onSelect={store.selectWave}
            onNewWave={store.newWave}
            refreshToken={store.refreshToken.value}
          />
        </div>
        <div class="app-main">
          <WavePanel
            client={store.client}
            profiles={store.profiles}
            route={route}
            startEditing={waveOpen && store.newWaveId.value === route.waveId}
            onClose={store.closeWave}
            onWaveChanged={store.waveChanged}
            onNavigate={store.navigate}
            linkFor={store.linkFor}
          />
        </div>
      </main>
      )}
    </div>
  );
}

function Splash({ store }: { store: AppStore }) {
  const unreachable = store.phase.value === "unreachable";
  return (
    <main class="app-splash">
      <WaveMark />
      {unreachable ? (
        <>
          <p class="notice notice-error" role="alert">
            The server could not be reached.
          </p>
          <button class="btn btn-primary" type="button" onClick={() => void store.start()}>
            Try again
          </button>
        </>
      ) : (
        <p role="status">Loading...</p>
      )}
    </main>
  );
}

export function App({ store }: { store: AppStore }) {
  const phase = store.phase.value;

  useEffect(() => {
    // Browsers show their own text; a handler that asks is all there is.
    const onBeforeUnload = (event: BeforeUnloadEvent): void => {
      if (store.hasUnsavedWork()) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    // "?" lists the keyboard shortcuts, unless it is typed into something.
    const onKey = (event: KeyboardEvent): void => {
      if (
        event.key === "?" &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.altKey &&
        !event.defaultPrevented &&
        !typingIn(event.target) &&
        store.phase.value === "signedIn"
      ) {
        event.preventDefault();
        store.shortcutsOpen.value = true;
      }
    };
    window.addEventListener("keydown", onKey);
    // Back from the background: reconnect now, not after the backoff.
    const onVisible = (): void => {
      if (document.visibilityState === "visible") {
        store.resumed();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("pageshow", onVisible);
    return () => {
      window.removeEventListener("beforeunload", onBeforeUnload);
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("pageshow", onVisible);
    };
  }, [store]);

  if (phase === "signedIn") {
    // A new session starts the panels afresh.
    return <Shell key={store.epoch.value} store={store} />;
  }
  if (phase === "signedOut") {
    return <AuthScreen store={store} />;
  }
  return <Splash store={store} />;
}
