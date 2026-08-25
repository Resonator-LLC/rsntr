# Observer v1

Status: accepted 2026-07-30 (M10); zoomable surface RETIRED 2026-08-06.
The observer is the browser of node projections; that role now lives in the console's contacts master-detail tab
(list of contacts, per-peer profile with identicon/presence/mod chips,
and the peer's root projection embedded in the detail). Sections 1, 2
and 5 below describe the retired zoomable surface and are kept as
history; sections 3 (the default RDF renderer) and 4 (media viewing)
remain normative and are cited from code comments in
`web-ui/index.html`. The server contract of `docs/web-api.md` is
unchanged except for `GET /api/peer/{id}`, added with the contacts
browser to surface a peer's wire hello.

## 1. The zoomable surface

A new OBSERVER nav panel holds one world (`#obs-world`) inside one
clipped viewport (`#obs-viewport`). Every contact the console knows
(the local node, admitted peers, chat rooms, and the read-only
"(all messages)" view) is one object laid out on a fixed grid in world
coordinates. The camera is a plain `{tx, ty, s}` transform applied as
`translate(tx,ty) scale(s)` on the world element; there is no canvas
and no render loop - the transform is restyled per input event, and a
`setTimeout`-stepped flight animates click-to-fly.

Input:

- wheel zooms about the cursor (trackpad pinch arrives as ctrl+wheel
  and gets a steeper factor), drag pans, click on a small object flies
  to it, Escape or clicking the background releases engagement.
- Click-to-fly interpolates log(s) with a smoothstep ease and a
  duration derived from zoom distance plus pan distance, so long and
  short flights feel the same speed.
- Input handoff: objects never see pointer events until engaged.
  A click on an object that already (nearly) fills the view engages it
  (accent border); only then does its full layer accept pointer events,
  and wheel events originating inside it scroll the content instead of
  zooming the camera. Everything else on the surface stays camera
  input.

## 2. Level-of-detail ladder

Each object owns three stacked representations, mounted/unmounted by
zoom (constants adapted from the research ZUI prototypes,
`research/html-ui/v1/lod.js`):

- chip: contact name plus a status/unread dot. Shown when the object
  is small.
- card: petname, kind (local/peer/room), shortened scope id, and an
  unread marker. Chip vs card is a snap at the band midpoint; a
  crossfade would double-expose two layouts.
- full: the working representation, faded in near f = 1 and mounted
  only when the object is near the viewport. It embeds the existing
  contact tooling: a chat pane and a projection browser pane (the same
  `buildChatTab` / `buildProjTab` builders the contacts panel uses),
  switched by a small strip inside the object. Unmounting damps any
  entrainments and aborts any media stream the object opened.

The band variable is f = s / sFit(o), where sFit fits the object to
the viewport (capped just below max zoom so f can reach 1). Bands:
chip below ~0.42, card to ~0.72, full fading in 0.72..0.92; full
mounts at 0.68 and unmounts below 0.55 or offscreen (hysteresis).

Unread state is shared with the contacts panel: fresh chat messages
mark the object's chip/card dot, and engaging an object's chat pane
clears the scope exactly like activating a chat tab.

## 3. The default RDF renderer (lenses)

Adopting the lens-mediated direction of research/generative-ui-2026.md
section 4: the hot path is a deterministic render of an RDF graph
through built-in lenses; LLM-generated lenses for unknown vocabularies
are a later milestone, as is treating lenses as shareable RDF
resources.

v1 ships three built-in lenses, chosen per subject by rdf:type:

- rsntr:Message: a chat-message card (sender, timestamp, room, body).
- projection points (rsntr:Excitable / Radiant / Sympathetic /
  Point): label, kind badge, mod/signal, comment.
- generic subject card (fallback): triples grouped by subject, one
  card per subject, typed header when a type exists, predicate/object
  rows with `shortIri` labels and literal datatype notes.

Wherever the console shows a constructed graph - SPARQL CONSTRUCT /
DESCRIBE results in the composer, and rsntr:Graph payloads returned by
projection points - the lens cards are now the default view, with a
"raw" toggle back to the previous Turtle `pre` block (and the .ttl
download in the composer). Blank-node subjects referenced from another
card render inline where cheap and as their own card otherwise.

## 4. Media viewing

A projection point whose `rsntr:mod` is `media` is an "open stream"
affordance (the menu marks it). Invoking it does not go through the
row/graph collector; instead the client POSTs the Query to `/request`
(with `?peer=` for a contact) and reads the response body itself:
framed envelopes up to the `rsntr:Media` header, then the raw unframed
byte feed to end of body - exactly the wire shape web-api.md defines.

- If `MediaSource.isTypeSupported` accepts the header's
  `rsntr:contentType` (the fMP4/mp4 family, plus whatever the browser
  says it can demux), the bytes feed an MSE `SourceBuffer` under a
  `<video>` element.
- Otherwise (e.g. `video/mp2t`) the console shows the content type and
  hands the user the working command:
  `rsntr watch <peer> <source> | ffplay -f mpegts -` (copy button).
  Transmuxing mpegts to fMP4 in the browser is explicitly out of
  scope for v1.

A stop button (and pane teardown) aborts the fetch, which resets the
relayed stream. Note the owner channel refuses the media raw feed; the
web surface serves it through the ordinary peer pipeline, so a local
`_media` source needs a `_policy` media allow row for the node's own
id, and remote sources need one on the serving peer.

## 5. Deferred

- Sandboxed mod-pushed UI hosting (iframe + postMessage): DONE in M12 as
  the hologram (hologram-protocol.md); the reserved iframe pattern
  is implemented as the hologram broker in the projection tabs.
- LLM-generated lenses, lenses as shareable RDF resources, per-lens
  catalogs.
- Constant-speed log-zoom tuning beyond the v1 flight curve, touch
  pinch on the observer surface (wheel/ctrl-wheel and drag work; the
  two-pointer pinch of the research prototype is not carried over).
- mpegts transmuxing, WebCodecs playback, audio-only styling.
