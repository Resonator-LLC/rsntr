// A wave app to start from. Rename it in manifest.json, add the
// permissions you need (wave, kv, peers, streams, media), and fill in the
// entry points. `render` returns the view as a flat list of nodes in
// document order, each naming its parent by index; `onUi` gets the events
// a node asks for in `on`. Call `invalidate(0)` when the view changed.

import { invalidate } from "resonator:app/ui@0.1.0";

let clicks = 0;

export function init(_appId, _selfPeer) {}
export function onMessages(_messages) {}
export function onPeersChanged(_peers) {}
export function onPeerData(_from, _data) {}
export function onStream(_stream, _from, _mode) {}
export function onStreamData(_stream, _data) {}
export function onStreamClosed(_stream) {}

export function render(_view) {
  return [
    { parent: undefined, tag: "div", text: undefined, attrs: [["class", "app-row"]], on: [] },
    { parent: 0, tag: "span", text: `Clicked ${clicks} times`, attrs: [], on: [] },
    { parent: 0, tag: "button", text: "Click", attrs: [], on: ["click"] },
  ];
}

export function onUi(_view, event) {
  if (event.name === "click" && event.node === 2) {
    clicks += 1;
    invalidate(0);
  }
}
