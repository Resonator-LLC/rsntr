// Accessibility and display preferences of this device, kept in
// localStorage and applied as attributes on <html>, where tokens.css and
// mobile.css read them:
//
// - data-theme: light | dark            (system resolved here)
// - data-contrast: normal | more        (system resolved here)
// - data-motion: normal | reduce        (system resolved here)
// - data-text-size: system | 100 | 125 | 150 | 200
// - data-spacing: normal | reading
// - data-targets: normal | large
// - data-underline-links: present when on
//
// "system" values follow the media queries and change with them. Native
// apps may set --system-text-scale on <html> (the system text size).

import { type Signal, effect, signal } from "@preact/signals";

export interface Prefs {
  readonly theme: "system" | "light" | "dark";
  readonly contrast: "system" | "normal" | "more";
  readonly motion: "system" | "reduce";
  readonly textSize: "system" | "100" | "125" | "150" | "200";
  readonly spacing: "normal" | "reading";
  readonly targets: "normal" | "large";
  readonly underlineLinks: boolean;
}

export const DEFAULT_PREFS: Prefs = {
  theme: "system",
  contrast: "system",
  motion: "system",
  textSize: "system",
  spacing: "normal",
  targets: "normal",
  underlineLinks: false,
};

/** The choices of every preference but the switch, in display order. */
export const CHOICES = {
  theme: ["system", "light", "dark"],
  contrast: ["system", "normal", "more"],
  motion: ["system", "reduce"],
  textSize: ["system", "100", "125", "150", "200"],
  spacing: ["normal", "reading"],
  targets: ["normal", "large"],
} as const satisfies { [K in keyof Prefs]?: readonly Prefs[K][] };

const KEY = "resonator.prefs";

/** Valid preferences from stored JSON; anything unknown falls back. */
export function parsePrefs(text: string | null): Prefs {
  let raw: unknown = null;
  try {
    raw = text === null ? null : JSON.parse(text);
  } catch {
    raw = null;
  }
  if (typeof raw !== "object" || raw === null) {
    return DEFAULT_PREFS;
  }
  const r = raw as Record<string, unknown>;
  const pick = <K extends keyof typeof CHOICES>(key: K): Prefs[K] =>
    (CHOICES[key] as readonly unknown[]).includes(r[key]) ? (r[key] as Prefs[K]) : DEFAULT_PREFS[key];
  return {
    theme: pick("theme"),
    contrast: pick("contrast"),
    motion: pick("motion"),
    textSize: pick("textSize"),
    spacing: pick("spacing"),
    targets: pick("targets"),
    underlineLinks: r.underlineLinks === true,
  };
}

/** What the media queries say, for the "system" values. */
export interface SystemPrefs {
  readonly dark: boolean;
  readonly moreContrast: boolean;
  readonly reduceMotion: boolean;
}

/** The attributes of <html> for these preferences. */
export function attributes(prefs: Prefs, system: SystemPrefs): Record<string, string | null> {
  return {
    "data-theme": prefs.theme === "system" ? (system.dark ? "dark" : "light") : prefs.theme,
    "data-contrast": prefs.contrast === "system" ? (system.moreContrast ? "more" : "normal") : prefs.contrast,
    "data-motion": prefs.motion === "reduce" || system.reduceMotion ? "reduce" : "normal",
    "data-text-size": prefs.textSize,
    "data-spacing": prefs.spacing,
    "data-targets": prefs.targets,
    "data-underline-links": prefs.underlineLinks ? "" : null,
  };
}

export class PrefsStore {
  readonly prefs: Signal<Prefs>;
  private readonly system: Signal<SystemPrefs>;

  constructor(
    private readonly storage: Pick<Storage, "getItem" | "setItem"> | null,
    private readonly media: ((query: string) => MediaQueryList) | null,
  ) {
    let stored: string | null = null;
    try {
      stored = storage?.getItem(KEY) ?? null;
    } catch {
      stored = null;
    }
    this.prefs = signal(parsePrefs(stored));
    this.system = signal(this.readSystem());
    for (const query of QUERIES) {
      this.media?.(query).addEventListener?.("change", () => {
        this.system.value = this.readSystem();
      });
    }
  }

  /** Changes one preference and keeps it. */
  set<K extends keyof Prefs>(key: K, value: Prefs[K]): void {
    this.prefs.value = { ...this.prefs.value, [key]: value };
    try {
      this.storage?.setItem(KEY, JSON.stringify(this.prefs.value));
    } catch {
      // Private mode or blocked storage: the change holds for this visit.
    }
  }

  /** Keeps the attributes of `root` up to date; returns the stop function. */
  apply(root: HTMLElement): () => void {
    return effect(() => {
      for (const [name, value] of Object.entries(attributes(this.prefs.value, this.system.value))) {
        if (value === null) {
          root.removeAttribute(name);
        } else {
          root.setAttribute(name, value);
        }
      }
    });
  }

  private readSystem(): SystemPrefs {
    const matches = (query: string): boolean => {
      try {
        return this.media?.(query).matches ?? false;
      } catch {
        return false;
      }
    };
    return {
      dark: matches(QUERIES[0]),
      moreContrast: matches(QUERIES[1]),
      reduceMotion: matches(QUERIES[2]),
    };
  }
}

const QUERIES = [
  "(prefers-color-scheme: dark)",
  "(prefers-contrast: more)",
  "(prefers-reduced-motion: reduce)",
] as const;

/** The store of this page: window's storage and media queries. */
export function browserPrefs(): PrefsStore {
  let storage: Storage | null = null;
  try {
    storage = window.localStorage;
  } catch {
    storage = null;
  }
  return new PrefsStore(storage, typeof matchMedia === "function" ? (q) => matchMedia(q) : null);
}
