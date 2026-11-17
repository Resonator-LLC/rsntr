import { describe, expect, it } from "vitest";

import { StandaloneDocument } from "../../model/doc";
import {
  type Ensemble,
  FIRST_LABEL,
  deleteEnsemble,
  ensembleQuery,
  isSqlQuery,
  nextLabel,
  orGroups,
  readEnsembles,
  settingsWaveId,
  writeEnsemble,
} from "./ensembles";

const relay: Ensemble = { id: 100, name: "Relay", colour: 1, query: "relay OR with:bob@example.com" };

describe("ensembles", () => {
  it("live in the user's own settings wave", () => {
    expect(settingsWaveId("alice@example.com")).toBe("example.com/settings+alice@example.com");
  });

  it("are written to and read from the settings document", () => {
    const doc = new StandaloneDocument().mutable();
    writeEnsemble(doc, relay);
    writeEnsemble(doc, { id: 101, name: "Lease <\"&'>", colour: 3, query: "" });
    expect(doc.toXmlString()).toContain('<e c="1" i="100" n="Relay" q="relay OR with:bob@example.com"/>');
    expect(readEnsembles(doc.view())).toEqual([
      relay,
      { id: 101, name: "Lease <\"&'>", colour: 3, query: "" },
    ]);
    // Changing one keeps its place.
    writeEnsemble(doc, { ...relay, name: "Relays", colour: 2 });
    expect(readEnsembles(doc.view()).map((e) => [e.id, e.name, e.colour])).toEqual([
      [100, "Relays", 2],
      [101, "Lease <\"&'>", 3],
    ]);
    expect(nextLabel(readEnsembles(doc.view()))).toBe(102);
    deleteEnsemble(doc, 100);
    deleteEnsemble(doc, 999);
    expect(readEnsembles(doc.view()).map((e) => e.id)).toEqual([101]);
  });

  it("skip entries that are broken", () => {
    const doc = new StandaloneDocument().mutable();
    doc.createElement(0, "e", [["i", "x"], ["n", "bad id"]]);
    doc.createElement(doc.size(), "e", [["i", "3"], ["n", "reserved folder"]]);
    doc.createElement(doc.size(), "x", [["i", "105"]]);
    doc.createElement(doc.size(), "e", [["i", "105"], ["c", "42"]]);
    doc.createElement(doc.size(), "e", [["i", "105"], ["n", "duplicate"]]);
    expect(readEnsembles(doc.view())).toEqual([{ id: 105, name: "", colour: 1, query: "" }]);
    expect(nextLabel([])).toBe(FIRST_LABEL);
  });

  it("split queries at a bare upper-case OR", () => {
    expect(orGroups("a b OR c")).toEqual([["a", "b"], ["c"]]);
    expect(orGroups('"a OR b" or c')).toEqual([['"a OR b"', "or", "c"]]);
    expect(orGroups("OR a OR OR b OR")).toEqual([["a"], ["b"]]);
    expect(orGroups("")).toEqual([]);
  });

  it("list their labelled waves and their query's waves in a box", () => {
    expect(ensembleQuery(relay, "inbox")).toBe(
      "in:inbox label:100 OR in:inbox relay OR in:inbox with:bob@example.com",
    );
    expect(ensembleQuery({ ...relay, query: "  " }, "archive")).toBe("in:archive label:100");
  });

  it("wrap SQL queries so that the box and the label still apply", () => {
    expect(isSqlQuery("  select wave_id from waves")).toBe(true);
    expect(isSqlQuery("selected words")).toBe(false);
    const sql = ensembleQuery({ ...relay, query: "SELECT wave_id FROM texts WHERE texts MATCH 'x';" }, "mute");
    expect(sql).toBe(
      "SELECT wave_id FROM waves WHERE box = 'mute' AND (" +
        "wave_id IN (SELECT wave_id FROM labels WHERE label = 100) OR " +
        "wave_id IN (SELECT wave_id FROM (SELECT wave_id FROM texts WHERE texts MATCH 'x'))) " +
        "ORDER BY last_modified DESC",
    );
    expect(isSqlQuery(sql)).toBe(true);
  });
});
