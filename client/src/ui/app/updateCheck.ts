// A new client build on the server: an open page keeps running the build it
// loaded, so it asks the server (`/version`) which build it serves now, and
// reloads or offers to.
//
// When it asks: when the connection comes back (a restarted server), when
// the page is shown again, and every ten minutes.
// What it does on a new build: reloads at once if the page is hidden or
// was just shown again and no change is unsaved; otherwise it shows
// "New version" in the top bar, and reloads when the page is next hidden
// with nothing unsaved.

import { type Signal, signal } from "@preact/signals";

/** The build of a script URL: its file name, if it is a built asset. */
export function buildOfScript(url: string): string | null {
  const match = /\/assets\/([^/?#]+\.js)(?:[?#].*)?$/.exec(url);
  return match === null ? null : (match[1] ?? null);
}

export interface UpdateCheckEnv {
  /** The build this page runs; `null` (a development server) turns the check off. */
  readonly ownBuild: string | null;
  /** The build the server serves now; `null` when it cannot tell. */
  fetchBuild(): Promise<string | null>;
  /** Whether a change has not reached the server yet. */
  hasUnsavedWork(): boolean;
  isHidden(): boolean;
  reload(): void;
  setInterval(callback: () => void, ms: number): unknown;
  clearInterval(handle: unknown): void;
  now(): number;
}

export const CHECK_INTERVAL_MS = 10 * 60 * 1000;
/** Checks on showing the page closer together than this are skipped. */
export const MIN_GAP_MS = 30 * 1000;

export class UpdateCheck {
  /** A newer build is served than the one running. */
  readonly available: Signal<boolean> = signal(false);

  private readonly env: UpdateCheckEnv;
  private interval: unknown = null;
  private lastCheck = Number.NEGATIVE_INFINITY;
  private checking = false;

  constructor(env: UpdateCheckEnv) {
    this.env = env;
  }

  start(): void {
    if (this.env.ownBuild === null || this.interval !== null) {
      return;
    }
    this.interval = this.env.setInterval(() => void this.check("timer"), CHECK_INTERVAL_MS);
  }

  stop(): void {
    if (this.interval !== null) {
      this.env.clearInterval(this.interval);
      this.interval = null;
    }
  }

  /** The connection to the server is back (after being lost). */
  onReconnected(): void {
    void this.check("reconnect");
  }

  /** The page was shown again. */
  onShown(): void {
    if (this.env.now() - this.lastCheck < MIN_GAP_MS) {
      return;
    }
    void this.check("shown");
  }

  /** The page was hidden: a good moment to take a known new build. */
  onHidden(): void {
    if (this.available.value && !this.env.hasUnsavedWork()) {
      this.env.reload();
    }
  }

  /** "Reload" in the top bar. */
  reloadNow(): void {
    this.env.reload();
  }

  async check(reason: "timer" | "reconnect" | "shown"): Promise<void> {
    if (this.env.ownBuild === null || this.checking) {
      return;
    }
    this.checking = true;
    this.lastCheck = this.env.now();
    try {
      const served = await this.env.fetchBuild();
      if (served === null || served === "" || served === this.env.ownBuild) {
        return;
      }
      const unnoticed = reason === "shown" || this.env.isHidden();
      if (unnoticed && !this.env.hasUnsavedWork()) {
        this.env.reload();
        return;
      }
      this.available.value = true;
    } catch {
      // Unreachable server: the next reconnect checks again.
    } finally {
      this.checking = false;
    }
  }
}

/** The check for this page in the browser. */
export function browserUpdateCheck(hasUnsavedWork: () => boolean): UpdateCheck {
  const check = new UpdateCheck({
    ownBuild: buildOfScript(import.meta.url),
    fetchBuild: async () => {
      const response = await fetch("/version", { cache: "no-store", credentials: "same-origin" });
      if (!response.ok) {
        return null;
      }
      const body = (await response.json()) as { client?: unknown };
      return typeof body.client === "string" ? body.client : null;
    },
    hasUnsavedWork,
    isHidden: () => document.visibilityState === "hidden",
    reload: () => window.location.reload(),
    setInterval: (callback, ms) => setInterval(callback, ms),
    clearInterval: (handle) => clearInterval(handle as ReturnType<typeof setInterval>),
    now: Date.now,
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") {
      check.onShown();
    } else {
      check.onHidden();
    }
  });
  window.addEventListener("pageshow", (event) => {
    if (event.persisted) {
      check.onShown();
    }
  });
  check.start();
  return check;
}
