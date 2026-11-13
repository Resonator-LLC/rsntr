// What the parts of the user interface agree on. The shell (`app/`) owns
// the `WaveClient`, the profile store and the route; the search panel
// (`search/`) and the wave panel (`wave/`) are given them as props.

import type { WaveClient } from "../client";
import type { ProfileStore } from "./profiles/profileStore";

/** What the URL fragment names: a wave and optionally one blip of it. */
export interface Route {
  /** Serialised wave id (`example.com/w+abc`), or null: no wave is open. */
  readonly waveId: string | null;
  /** Serialised wavelet id (`example.com/conv+root`), if the fragment names one. */
  readonly waveletId: string | null;
  /** The blip to focus (a deep link), or null. */
  readonly blipId: string | null;
}

export interface WavePanelProps {
  readonly client: WaveClient;
  readonly profiles: ProfileStore;
  /** The wave to show; null shows the empty state. */
  readonly route: Route;
  /**
   * The wave was created by this session a moment ago ("New wave"): start
   * editing its root blip once it is shown.
   */
  readonly startEditing: boolean;
  /** The user asked to leave the wave (the back button of the phone layout). */
  readonly onClose: () => void;
  /**
   * Something the search panel shows may have changed (read state, title,
   * participants, inbox/archive...). The shell refreshes the search soon.
   */
  readonly onWaveChanged: () => void;
  /** Navigate within the app, e.g. after following a wave link. */
  readonly onNavigate: (route: Route) => void;
  /** The absolute URL of a route (for "Link to this blip"). */
  readonly linkFor: (route: Route) => string;
}

export interface SearchPanelProps {
  readonly client: WaveClient;
  readonly profiles: ProfileStore;
  /** The wave shown in the wave panel, for the selection highlight. */
  readonly selectedWaveId: string | null;
  readonly onSelect: (waveId: string) => void;
  /** Creates a wave and opens it. */
  readonly onNewWave: () => void;
  /**
   * Incremented by the shell when the result may be stale (see
   * `WavePanelProps.onWaveChanged`); the panel then searches again.
   */
  readonly refreshToken: number;
}
