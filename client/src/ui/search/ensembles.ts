// Ensembles: named, coloured views over the user's waves (not in the
// original). An ensemble is a query, and a label of its own that the user
// puts on waves by dragging them in (a folder id of the per-wave user-data
// supplement). A wave is in the ensemble if it has the label or matches the
// query. A wave can be in several ensembles.
//
// The list of ensembles lives in the user's own user-data wavelet
// `user+<address>` of the wave `settings+<address>`, in a document
// `ensembles` of `<e i="100" n="Relay" c="1" q="relay"/>` elements, so it
// syncs between devices like any wavelet, and nobody else can read or
// change it. That wave has no conversation, so search never lists it.
// See docs/search.md.

import { type ReadonlySignal, signal } from "@preact/signals";

import type { OpenWave, WaveClient } from "../../client";
import type { DocView, MutableDocument, NodeId } from "../../model/doc";
import type { Unsubscribe } from "../../util/events";

/** The document of the settings wavelet that holds the ensembles. */
export const ENSEMBLES_DOCUMENT = "ensembles";
const ENSEMBLE_TAG = "e";

/**
 * The first label id. Folder ids 1, 3 and 8 mean inbox, all and trash to
 * the supplement, so labels keep well away from the small numbers.
 */
export const FIRST_LABEL = 100;
/** Number of colours (`--c-ens-1` .. `--c-ens-6`). */
export const COLOURS = 6;

export type Box = "inbox" | "archive" | "mute";

export interface Ensemble {
  /** The label id; also the ensemble's identity. */
  readonly id: number;
  readonly name: string;
  /** 1 .. {@link COLOURS}. */
  readonly colour: number;
  /** A token query, or SQL starting with `SELECT `; may be empty. */
  readonly query: string;
}

/** The wave holding a user's settings: `<domain>/settings+<address>`. */
export function settingsWaveId(address: string): string {
  const at = address.lastIndexOf("@");
  return `${address.slice(at + 1)}/settings+${address}`;
}

/** Whether the server runs a query as SQL (`SELECT ` at the start, any case). */
export function isSqlQuery(query: string): boolean {
  return /^select\s/i.test(query.trimStart());
}

function clampColour(value: number): number {
  return Number.isInteger(value) && value >= 1 && value <= COLOURS ? value : 1;
}

/** The ensembles in a settings document, in document order; broken entries are skipped. */
export function readEnsembles(view: DocView): Ensemble[] {
  const out: Ensemble[] = [];
  const seen = new Set<number>();
  for (const element of view.childElements(view.root())) {
    if (!view.hasTag(element, ENSEMBLE_TAG)) {
      continue;
    }
    const id = Number(view.attribute(element, "i"));
    if (!Number.isInteger(id) || id < FIRST_LABEL || id > 2147483647 || seen.has(id)) {
      continue;
    }
    seen.add(id);
    out.push({
      id,
      name: view.attribute(element, "n") ?? "",
      colour: clampColour(Number(view.attribute(element, "c"))),
      query: view.attribute(element, "q") ?? "",
    });
  }
  return out;
}

/** A label id no ensemble has. */
export function nextLabel(ensembles: readonly Ensemble[]): number {
  return ensembles.reduce((max, e) => Math.max(max, e.id + 1), FIRST_LABEL);
}

function attributes(ensemble: Ensemble): [string, string][] {
  return [
    ["c", String(ensemble.colour)],
    ["i", String(ensemble.id)],
    ["n", ensemble.name],
    ["q", ensemble.query],
  ];
}

function findElement(doc: MutableDocument, id: number): NodeId | null {
  const view = doc.view();
  for (const element of view.childElements(view.root())) {
    if (view.hasTag(element, ENSEMBLE_TAG) && view.attribute(element, "i") === String(id)) {
      return element;
    }
  }
  return null;
}

/** Adds an ensemble at the end, or changes the one with its id. */
export function writeEnsemble(doc: MutableDocument, ensemble: Ensemble): void {
  const element = findElement(doc, ensemble.id);
  if (element === null) {
    doc.createElement(doc.size(), ENSEMBLE_TAG, attributes(ensemble));
    return;
  }
  for (const [name, value] of attributes(ensemble)) {
    if (doc.view().attribute(element, name) !== value) {
      doc.setElementAttribute(element, name, value);
    }
  }
}

/** Removes an ensemble; the label stays on waves but means nothing any more. */
export function deleteEnsemble(doc: MutableDocument, id: number): void {
  const element = findElement(doc, id);
  if (element !== null) {
    doc.deleteNode(element);
  }
}

/**
 * Splits a token query into its `OR` groups as the server does: a bare,
 * unquoted, upper-case `OR` between tokens.
 */
export function orGroups(query: string): string[][] {
  const groups: string[][] = [[]];
  const tokens = query.match(/(?:[^\s"]+|"[^"]*"?)+/g) ?? [];
  for (const token of tokens) {
    if (token === "OR") {
      groups.push([]);
    } else {
      groups[groups.length - 1]!.push(token);
    }
  }
  return groups.filter((group) => group.length > 0);
}

function sqlString(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

/**
 * The query that lists an ensemble's waves in a box: the waves with its
 * label, and those matching its query, newest first.
 */
export function ensembleQuery(ensemble: Ensemble, box: Box): string {
  const query = ensemble.query.trim();
  if (isSqlQuery(query)) {
    const inner = query.replace(/[\s;]+$/, "");
    return (
      `SELECT wave_id FROM waves WHERE box = ${sqlString(box)} AND (` +
      `wave_id IN (SELECT wave_id FROM labels WHERE label = ${ensemble.id}) OR ` +
      `wave_id IN (SELECT wave_id FROM (${inner}))) ORDER BY last_modified DESC`
    );
  }
  const groups = [[`label:${ensemble.id}`], ...orGroups(query)];
  return groups.map((group) => [`in:${box}`, ...group].join(" ")).join(" OR ");
}

/**
 * The ensembles of the signed-in user, from their settings wave, which is
 * kept open while the store lives.
 */
export class EnsembleStore {
  private readonly listSignal = signal<readonly Ensemble[]>([]);
  readonly list: ReadonlySignal<readonly Ensemble[]> = this.listSignal;
  private readonly readySignal = signal(false);
  /** The settings wave has been read. */
  readonly ready: ReadonlySignal<boolean> = this.readySignal;
  private wave: OpenWave | null = null;
  private readonly off: Unsubscribe[] = [];
  private watching: Unsubscribe | null = null;
  private disposed = false;

  constructor(private readonly client: Pick<WaveClient, "openWave" | "address">) {}

  async start(): Promise<void> {
    const address = this.client.address;
    if (address === null || this.wave !== null) {
      return;
    }
    const wave = await this.client.openWave(settingsWaveId(address.address));
    if (this.disposed) {
      wave.close();
      return;
    }
    this.wave = wave;
    const watch = (): void => {
      this.watching?.();
      this.watching = wave.view.userData()?.events.on("operation", () => this.read()) ?? null;
    };
    this.off.push(
      wave.view.events.on("waveletAdded", () => {
        watch();
        this.read();
      }),
    );
    watch();
    this.read();
  }

  private read(): void {
    const view = this.wave?.userDataDocument(ENSEMBLES_DOCUMENT) ?? null;
    this.listSignal.value = view === null ? [] : readEnsembles(view);
    this.readySignal.value = true;
  }

  private edit(change: (doc: MutableDocument) => void): void {
    if (this.wave === null) {
      throw new Error("the ensembles are not loaded");
    }
    this.wave.editUserDataDocument(ENSEMBLES_DOCUMENT, change);
    this.read();
  }

  /** Adds or changes an ensemble. */
  save(ensemble: Ensemble): void {
    this.edit((doc) => writeEnsemble(doc, ensemble));
  }

  remove(id: number): void {
    this.edit((doc) => deleteEnsemble(doc, id));
  }

  dispose(): void {
    this.disposed = true;
    this.off.forEach((off) => off());
    this.watching?.();
    this.wave?.close();
    this.wave = null;
  }
}
