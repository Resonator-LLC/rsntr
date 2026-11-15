// A chat in a box: messages are posted to the wave (kind "chat"), so
// everyone in it sees them, in order, and the app's Log keeps them. What
// you are typing is kept in kv, so it survives a restart.

import { messages as waveMessages, post } from "resonator:app/wave@0.1.0";
import { get as kvGet, set as kvSet } from "resonator:app/kv@0.1.0";
import { invalidate } from "resonator:app/ui@0.1.0";

const SHOWN = 50;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

let lines = [];
let draft = "";
let error = "";

function failure(e) {
  return e && e.payload !== undefined ? String(e.payload) : String(e);
}

function take(messages) {
  for (const m of messages) {
    if (m.kind === "chat") {
      lines.push({ author: m.author, body: m.body });
    }
  }
  lines = lines.slice(-SHOWN);
}

export function init(_appId, _selfPeer) {
  take(waveMessages(0n));
  const stored = kvGet("draft");
  draft = stored === undefined ? "" : decoder.decode(stored);
}

export function onMessages(messages) {
  take(messages);
  invalidate(0);
}

export function onPeersChanged(_peers) {}
export function onPeerData(_from, _data) {}
export function onStream(_stream, _from, _mode) {}
export function onStreamData(_stream, _data) {}
export function onStreamClosed(_stream) {}

// Node indexes: 0 the column, 1 the list, then one row per line, then the
// form and its field and button.
export function render(_view) {
  const nodes = [
    { parent: undefined, tag: "div", text: undefined, attrs: [["class", "app-col"]], on: [] },
    { parent: 0, tag: "div", text: undefined, attrs: [["class", "app-col app-scroll"]], on: [] },
  ];
  if (lines.length === 0) {
    nodes.push({ parent: 1, tag: "p", text: "No messages yet", attrs: [["class", "app-muted"]], on: [] });
  }
  for (const line of lines) {
    const row = nodes.length;
    nodes.push({ parent: 1, tag: "div", text: undefined, attrs: [["class", "app-row"]], on: [] });
    nodes.push({ parent: row, tag: "strong", text: line.author.split("@")[0], attrs: [["title", line.author]], on: [] });
    nodes.push({ parent: row, tag: "span", text: line.body, attrs: [], on: [] });
  }
  const form = nodes.length;
  nodes.push({ parent: 0, tag: "form", text: undefined, attrs: [["class", "app-row"]], on: ["submit"] });
  nodes.push({
    parent: form,
    tag: "input",
    text: undefined,
    attrs: [
      ["name", "text"],
      ["value", draft],
      ["placeholder", "Say something"],
      ["class", "app-grow"],
    ],
    on: ["input"],
  });
  nodes.push({ parent: form, tag: "button", text: "Send", attrs: [["type", "submit"]], on: [] });
  if (error !== "") {
    nodes.push({ parent: 0, tag: "p", text: error, attrs: [["class", "app-error"]], on: [] });
  }
  return nodes;
}

export function onUi(_view, event) {
  if (event.name === "input") {
    draft = event.value ?? "";
    kvSet("draft", encoder.encode(draft));
    return;
  }
  if (event.name === "submit") {
    const field = event.fields.find(([name]) => name === "text");
    const text = (field ? field[1] : "").trim();
    if (text === "") return;
    try {
      post("chat", text);
      error = "";
      draft = "";
      kvSet("draft", encoder.encode(""));
    } catch (e) {
      error = failure(e);
    }
    invalidate(0);
  }
}
