// The entry point of the browser client.

import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/shell.css";
import "./styles/search.css";
import "./styles/wave.css";
import "./styles/editor.css";
import "./styles/mobile.css";

import { render } from "preact";

import { WaveClient } from "../client";
import { App } from "./app/App";
import { AppStore, windowLocation } from "./app/appStore";
import { ProfileStore } from "./profiles/profileStore";
import { browserUpdateCheck } from "./app/updateCheck";
import { followVisualViewport } from "./app/viewport";
import { browserPrefs } from "./app/prefs";

const root = document.querySelector<HTMLDivElement>("#app");
if (root !== null) {
  followVisualViewport(window);
  // Before the first render: the theme and text size are right at once.
  const prefs = browserPrefs();
  prefs.apply(document.documentElement);
  // Same origin: the page is served by the wave server (or, in development,
  // by a dev server that proxies to it).
  const client = new WaveClient({ baseUrl: "" });
  const profiles = new ProfileStore(client, (path) => client.api.resolve(path));
  // The store reads unsaved state; the check is made before it, so it asks late.
  let unsaved = (): boolean => false;
  const update = browserUpdateCheck(() => unsaved());
  const store = new AppStore({ client, profiles, location: windowLocation(), update, prefs });
  unsaved = () => store.hasUnsavedWork();
  render(<App store={store} />, root);
  void store.start();
}
