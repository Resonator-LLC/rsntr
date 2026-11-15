// The counter test guest in JavaScript. It mirrors counter-rs exactly; the
// runtime tests check that both give the same renders and effects.

import { get as kvGet, set as kvSet } from "resonator:app/kv@0.1.0";
import { post } from "resonator:app/wave@0.1.0";
import { invalidate } from "resonator:app/ui@0.1.0";
import { log } from "resonator:app/log@0.1.0";

let count = 0n;
let error = undefined;

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function failure(e) {
  return e && e.payload !== undefined ? String(e.payload) : String(e);
}

export function init(_appId, _selfPeer) {
  const stored = kvGet("count");
  if (stored !== undefined) {
    try {
      count = BigInt(decoder.decode(stored));
    } catch {
      count = 0n;
    }
  }
  log("info", "counter started");
}

export function onMessages(messages) {
  let changed = false;
  for (const m of messages) {
    if (m.kind !== "inc") continue;
    let n;
    try {
      n = BigInt(m.body);
    } catch {
      continue;
    }
    if (n > count) {
      count = n;
      changed = true;
    }
  }
  if (changed) invalidate(0);
}

export function onPeersChanged(_peers) {}
export function onPeerData(_from, _data) {}
export function onStream(_stream, _from, _mode) {}
export function onStreamData(_stream, _data) {}
export function onStreamClosed(_stream) {}

export function render(_view) {
  const nodes = [
    { parent: undefined, tag: "div", text: undefined, attrs: [["class", "app-row"]], on: [] },
    { parent: 0, tag: "span", text: count.toString(), attrs: [], on: [] },
    { parent: 0, tag: "button", text: "+1", attrs: [], on: ["click"] },
  ];
  if (error !== undefined) {
    nodes.push({ parent: 0, tag: "p", text: error, attrs: [["class", "app-error"]], on: [] });
  }
  return nodes;
}

export function onUi(_view, event) {
  if (event.name === "click" && event.node === 2) {
    count += 1n;
    const body = count.toString();
    const errors = [];
    try {
      kvSet("count", encoder.encode(body));
    } catch (e) {
      errors.push(`kv: ${failure(e)}`);
    }
    try {
      post("inc", body);
    } catch (e) {
      errors.push(`wave: ${failure(e)}`);
    }
    error = errors.length > 0 ? errors.join("; ") : undefined;
    invalidate(0);
  } else if (event.name === "spin") {
    let x = 0;
    for (;;) x = (x + 1) | 0;
  } else if (event.name === "hog") {
    const blocks = [];
    for (;;) blocks.push(new Uint8Array(1 << 20).fill(1));
  }
}
