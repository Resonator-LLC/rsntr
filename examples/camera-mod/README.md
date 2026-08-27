# camera-mod

A camera wall served as a hologram (docs/hologram-protocol.md): the mod
ships a viewer app that the observer mounts in a sealed iframe, and each
stream rides the hologram broker's media lane - the node's builtin
`media` modulation over iroh, relayed to the guest as transferable byte
chunks and fed into MSE. The pure-config companion to shop-mod: no
database capabilities at all.

Standalone cargo project (not a workspace member), `resonator-mod-pdk`
via a path dependency. Capabilities: none.

Verbs:

- `hologram` streams the embedded single-file viewer (`app/index.html`).
- `sources` lists the cameras from the mod config
  (`_modulations.config` key `cameras`, a JSON array of
  `{"name", "label"}`; `name` is a `_media` source name on this node).

The video authority stays where it always was: the per-source `_policy`
media rows on the serving node. The mod only publishes the menu; a
listed camera the caller may not view answers Denied when opened.

## Build

    rustup target add wasm32-unknown-unknown
    cargo build --target wasm32-unknown-unknown --release

Artifact: `target/wasm32-unknown-unknown/release/camera_mod.wasm`.

## Install

    rsntr mod add cameras target/wasm32-unknown-unknown/release/camera_mod.wasm -d <dir>
    rsntr sql <dir> --file seed.sql        # then edit the camera list to taste
    rsntr mod enable cameras -d <dir>
    rsntr serve <dir> --web

Each camera needs a `_media` source that emits browser-playable bytes.
MSE wants fragmented mp4 with a codecs string, e.g. a source command
ending in:

    ffmpeg ... -c:v libx264 -tune zerolatency \
      -f mp4 -movflags +frag_keyframe+empty_moov+default_base_moof pipe:1

registered with content type `video/mp4; codecs="avc1.640028, mp4a.40.2"`.
Plain mpegts (`video/mp2t`) is not MSE-playable; remux on the serving
side (the clubserver station's cam-nvr.sh is the reference).

Grant each viewer the sources they may open:

    INSERT INTO _policy (peer_or_group, table_name, action, effect)
    VALUES ('<peer>', '<source>', 'media', 'allow');

## The door call (optional)

A camera entry may carry a `talk` field naming an audio-duplex source on
the same node (a `_media` row with `accepts` set, envelope doc sec 4.3):

    {"name": "door", "label": "entrance door", "talk": "door-talk"}

The app then shows a talk toggle on that camera (the broker captures the
microphone and streams it up the wire; viewers need the `audio-duplex`
allow on the talk source). For the incoming direction, create a
`door_calls` table and a sympathetic point, and have the bell event
INSERT a row over the owner channel:

    CREATE TABLE IF NOT EXISTS door_calls (
      call_id TEXT PRIMARY KEY,
      state   TEXT NOT NULL DEFAULT 'ringing',
      at      TEXT NOT NULL DEFAULT (datetime('now'))
    );
    INSERT INTO _projection (point_iri, kind, label, ord, resource, note)
    VALUES ('urn:door:ringing', 'sympathetic', 'door bell', 110,
            'door_calls', 'camera-mod');
    -- viewers: read + entrain on door_calls, and grant the mod db_read
    -- (beyond its empty needs) so the `calls` verb can read the table.

The app entrains `urn:door:ringing`, shows a ring banner with an answer
button (opens the door camera and starts the talk), and stops ringing
everywhere when the call leaves the `ringing` state. The talk source's
own command should silence the physical panel and mark the call answered
as its first acts, so answering is nothing but opening the duplex.
