# Wave apps

A wave app is a small program that lives in a wave. A blip holds a gadget
that names a bundle; every participant's own node runs the bundle for
them, and the browser in front of that node draws what the app renders.
There is no app server: the instances on the participants' nodes share
state through the wave and talk to each other directly.

## Pieces

| Where | What |
|---|---|
| `crates/resonator-apps` | The runtime: wasmtime, the WIT world, limits, bundles. Knows nothing of the server. |
| `crates/resonator-server/src/apps/` | The AppManager: which instances run, their snapshots and effects, kv, the app wavelet, peers. |
| `crates/resonator-federation/src/app_channel.rs` | `resonator/app/1`: peers on other nodes, over iroh. |
| `client/src/ui/waveapps/` | The app box: header, permission prompt, shadow root, patcher, Log, microphone and speaker. |
| `crates/resonator-app-sdk` | Helpers for apps in Rust. |
| `examples/wave-apps/` | `counter`, `chat-js`, `walkie`, `template-js`, and `bundle.sh`. |

## Turning it on

```toml
[apps]
enabled = true        # off by default on a server, on in device mode
max_instances = 64    # instances running at once on this node
idle_secs = 300       # how long an instance outlives its last viewer
```

Or `RESONATOR_APPS_ENABLED=true`. With apps off, a gadget naming an app
shows "Apps are off" and nothing runs.

## A gadget, a bundle, a grant

- Insert app (the edit toolbar) uploads a zip as an attachment of the wave
  and inserts `<gadget id="..." app="sha256:<hex>" bundle="<attachment>">`.
- The bundle is `manifest.json` (`name`, `version`, `permissions`,
  `memory_mb`) and `app.wasm`, a component of the world `wave-app` in
  `crates/resonator-apps/wit/app.wit`. The app id is the sha256 of the zip;
  a node refuses a bundle whose hash differs from the gadget's.
- Nothing runs until the person allows it. The prompt lists the
  permissions; Allow writes `allowed = <app id>` into the gadget's state in
  the person's user-data wavelet (`m/gadgets`), so their other devices
  know too. A changed bundle is a new app and is asked for again.

| Permission | The app may |
|---|---|
| (none) | draw in its box and take clicks |
| `wave` | read and post messages of the app wavelet |
| `kv` | keep data on this node (never federated) |
| `peers` | see and message the same app on other devices |
| `streams` | open reliable or datagram streams to them |
| `media` | hear the microphone while the person turns it on, and play sound |
| `camera` | see the camera while the person turns it on, and show video |
| `video` | show video, without the camera (no camera button) |

## Instances and limits

An instance is keyed by (local account, wave, gadget). It starts when
someone opens the app box and the gadget is allowed and its bundle is
here; it stops when the gadget or the grant is removed, the account leaves
the wave, the node goes to the background, or `idle_secs` after the last
viewer left. Each call has a deadline (200 ms for events, 50 ms for
`render`) and memory is capped by `memory_mb` (64 MB by default, at most
256). A trap or a missed deadline fails the instance;
the box offers Start again. WASI is linked with no files, sockets,
environment or stdio. iOS runs Pulley bytecode, everything else Cranelift.

## Drawing: a closed shadow root

`render(view)` returns a flat list of nodes in document order, each naming
its parent by index. The client builds them in a closed shadow root with
`createElement` and `textContent`, never markup, so no app code runs in
the browser. Only whitelisted tags and attributes are built: no `script`,
`iframe`, `style`, `href` or `on*`; `img src` must name an attachment of
the wave. The stylesheet is ours (`app-ui.css`, classes such as `app-row`,
`app-col`, `app-muted`, `app-error`). Events the app asks for (`click`,
`input`, `change`, `submit`, `keydown`) are sent by our listeners; form
submits send the named fields. A `password` input is drawn as a masked
text field, so the browser never offers the person's own saved password to
an app. The `surface` tag is reserved for a later WebGL scene.

## The app wavelet

`wave.post(kind, body)` adds a document `msg+<id>` (`<m kind t>body</m>`)
to the wavelet `app+<gadget id>` of the wave, with the participants of the
conversation. It is not conversational: it shows in no blip, search or
unread count, and it federates like any wavelet. A message's sequence
number is the version that created it, so every node sees the same order.
The Log button in the app box shows the messages read-only.

## Peers

`peers.list()` names the other instances of the same gadget and app:

- `<participant>` for one on this node (another account here);
- `<participant>/<device>` for one on another node, reached over the iroh
  ALPN `resonator/app/1`;
- `viewer` (`local`), the browser in front of this node, for apps with
  `media`, `camera` or `video`.

On `resonator/app/1` every stream starts with a length-prefixed JSON
header (`hello`, `bye`, `send`, `open`). The connection's key must speak
for the sender's domain, and the receiver checks that the sender is a
participant of the wave and runs the same app. `send` is one uni stream,
a reliable stream is a bi stream of length-prefixed frames, and a datagram
stream is QUIC datagrams tagged with the stream. Without federation there
are no remote peers.

## Sound and video

With `media`, the box header has a Mic button. While it is on, the client
captures the microphone, encodes Opus with WebCodecs and sends each frame
(`AppMediaRequest`); the app sees it as data on a datagram stream from
`viewer`. Whatever the app sends to `viewer` (a channel byte, then an Opus
frame) comes back in `AppUpdate`s with status `media` and is decoded and
played, one decoder per channel. The app never touches the devices.
With `camera`, the bar also has a camera button. Camera frames are
encoded with WebCodecs (VP8, or H.264 where there is no VP8; 320x240 at
15 frames a second, a key frame every 2 seconds) and reach the app on a
reliable stream from `viewer`, each a flags byte (bit 0 a key frame, bits
1-3 the codec: 0 VP8, 1 H.264) and the frame. What the app writes on a
reliable stream it opens to `viewer` (a channel byte, then a camera frame)
is decoded and shown, one picture per channel, next to the person's own.
With `video` and not `camera`, the app's video is shown the same way but
there is no camera button and no picture of the person's own: for an app
that only plays video (a door camera, say). `media` and `video` together
give a Mic button and video. A picture from the app fills the width of the
box, at the frame's own aspect ratio.
Video goes between peers on reliable streams: key frames are too large for
datagrams.

### Over WebRTC

The above is the fallback. Where the node has `[webrtc]` on and the
browser has `RTCPeerConnection`, a box with `media`, `camera` or `video` also opens
a WebRTC connection to its own node (signalled with `AppRtcRequest` on
the socket; the node is ICE-lite, on one UDP port for every session). The
microphone and camera then go out as tracks (the browser encodes them and
adapts to the network), and what the app sends back comes in on 8 audio
and 8 video receiving tracks, the slots: the node gives each app channel
a slot and says which in `AppUpdate`s with status `slots`. Sound plays in
`<audio>` elements, so the browser's echo cancellation covers it. The app
sees the same frames either way. If the connection is not up within 5
seconds, or breaks, media stays on (or goes back to) the socket; the
box's `data-media-path` says which (`rtc`, `connecting`, `socket`). The
localStorage key `resonator.app-media` set to `socket` keeps a browser
off WebRTC.

```toml
[webrtc]
enabled = true
listen = "0.0.0.0:7779"            # one UDP port
advertise = "203.0.113.5:7779"     # where browsers reach it
```

Or `RESONATOR_WEBRTC_ENABLED`, `RESONATOR_WEBRTC_LISTEN` and
`RESONATOR_WEBRTC_ADVERTISE`. Off by default. The UDP port must be open
to browsers; there is no TURN, so where UDP is blocked the socket is used.

`getUserMedia` needs a secure page: `localhost`, HTTPS (on a tailnet,
`tailscale serve` gives a node a real certificate), or the iOS and Android
apps (which ask the system once).

## Writing an app

See `examples/wave-apps/README.md`. Rust apps use `resonator-app-sdk`
and the `wasm32-wasip2` target; JavaScript apps are built with
componentize-js. `./bundle.sh <app>` builds and packs one.
