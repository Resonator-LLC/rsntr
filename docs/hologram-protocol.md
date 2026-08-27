# The Hologram: Mod-Served Web Views for the Resonator Network

Status: normative for v3, 2026-08-03 (M12). Discharges the deferral
recorded in [observer.md](observer.md) section 5 and the reserved
iframe + postMessage pattern of PLAN.md 6.3. Normative companions:
[rdf-envelope-protocol.md](rdf-envelope-protocol.md) (envelope, framing,
Generic frames), [projection-protocol.md](projection-protocol.md)
(points, entrainment), [web-api.md](web-api.md) (browser transport).
Ontology: [rsntr-hologram.ttl](rsntr-hologram.ttl).

## 1. Why a hologram

A projection tells the observer what a node can do; the observer owns the
rendering. That division is right for data, and it stays. But some
capabilities are experiences, not menus: a shop wants a catalog with a
cart, not sixteen numbered points. The hologram is the escape hatch that
keeps the protocol honest while allowing this: the serving mod ships its
own interactive rendering as an HTML document, and the observer mounts it
in a sealed web view whose every network act is wrapped in the resonator
envelope and answered by the mod that served it.

The name is the physics: a projection is a shadow of the node on the
observer's wall; a hologram is a recorded wavefront the observer
reconstructs into a scene it can step into. Both are made of the same
light. Nothing about the hologram bypasses the protocol: fetching one is
an ordinary Query, everything the mounted document does is an ordinary
Query or Execute against the serving mod, and every one of those requests
passes the peer gate, the `mod:<name>` policy action, and the audit trail
exactly as if typed by hand.

## 2. Vocabulary

Two new response classes and two new properties; everything else is
reused from the envelope and projection vocabularies. Big nouns get the
metaphor, mechanics stay plain.

### Classes

| Class | Meaning |
|---|---|
| `rsntr:Hologram` | Header frame of a hologram response: announces content type and total size. Also usable as a `_projection` point kind (an IRI in the `kind` column) to hint that invoking the point yields a hologram |
| `rsntr:Chunk` | One slice of the document body, base64-encoded, ordered by `rsntr:seq` |

### Properties

| Property | Range | On | Meaning |
|---|---|---|---|
| `rsntr:contentType` | `xsd:string` | Hologram | MIME type of the reassembled document, normally `"text/html; charset=utf-8"` (reused from the Media header) |
| `rsntr:size` | `xsd:integer` | Hologram | total raw byte count of the document before base64; lets the client preallocate and detect truncation |
| `rsntr:name` | `xsd:string` | Hologram | optional; echoes the asset path of a `hologram <path>` fetch (reused from the BlobRef vocabulary) |
| `rsntr:seq` | `xsd:integer` | Chunk | 0-based slice order (reused from Row and Vibration) |
| `rsntr:data` | `xsd:base64Binary` | Chunk | the slice bytes |

Neither class is a request class: both ride the response path and decode
to Generic objects on clients that predate them (envelope doc section
10), so a hologram degrades on an old client to an inert blob of Turtle,
never an error.

## 3. Fetching a hologram

A hologram is fetched with an ordinary Query naming the serving
modulation; the signal is the verb `hologram`:

```turtle
[] a rsntr:Query ;
   rsntr:id "01K1H0GRAM00XMP00000000001" ;
   rsntr:mod "shop" ;
   rsntr:signal "hologram" .
```

The response stream is one header frame, then chunk frames in `seq`
order, then the ordinary trailer with `rowCount` equal to the chunk
count:

```turtle
# frame 1: header
[] a rsntr:Hologram ;
   rsntr:id "01K1H0GRAM00XMP00000000001" ;
   rsntr:contentType "text/html; charset=utf-8" ;
   rsntr:size 15 .
```

```turtle
# frame 2..n: chunks, seq order
[] a rsntr:Chunk ;
   rsntr:id "01K1H0GRAM00XMP00000000001" ;
   rsntr:seq 0 ;
   rsntr:data "PCFkb2N0eXBlIGh0bWw+"^^xsd:base64Binary .
```

```turtle
# final frame: trailer
[] a rsntr:Done ;
   rsntr:id "01K1H0GRAM00XMP00000000001" ;
   rsntr:rowCount 1 ;
   rsntr:truncated false .
```

The client concatenates the decoded chunks and MUST verify the total
against `rsntr:size`; a mismatch is treated as a truncated response.

`hologram <path>` (the verb, a space, an opaque asset path) is reserved
for named sub-documents. A mod that serves no such asset answers
`rsntr:Error` with code `point-unknown`. As with projection paths, asset
paths are discovered, never constructed.

### Chunking and budgets

The frame budget binds the serialized Turtle (envelope doc section 7),
and mod-emitted blob properties ride as base64 literals, so the raw slice
size must leave headroom for the 4/3 inflation plus frame overhead. The
normative bound: a Chunk's serialized frame MUST stay under the 256 KiB
frame cap. The recommended slice is 128 KiB raw (about 175 KiB framed).
The whole stream counts against the node's response byte budget (8 MiB
by default), which bounds a hologram at roughly 5.8 MiB raw; authors
SHOULD keep documents under 512 KiB, which is four chunks.

## 4. Rendering is response-driven

A client switches to hologram rendering when the first frame of a
response is `rsntr:Hologram`, whatever the invoked point looked like. A
`_projection` row MAY hint by using the `rsntr:Hologram` IRI as its point
kind, so menus can label the entry as an app before invoking it; the hint
is a courtesy, not a contract. Clients that do not render holograms fall
back to the open-world rule and show the frames inertly.

Staleness follows the projection rules: the document is stable for a
serve run (the mod registry loads at start), clients SHOULD refetch when
`urn:rsntr:projection-changed` vibrates, and MUST NOT cache a hologram
across sessions.

## 5. The containment contract

The hologram document is authored by the serving peer: it is exactly as
trusted as that peer, which is to say admitted, policy-bound, and still
not yours. The observer therefore MUST mount it so that the document
holds nothing and reaches nothing except the protocol:

- The document is mounted in an opaque-origin sandboxed iframe:
  `sandbox="allow-scripts"` and srcdoc, never `allow-same-origin`. The
  guest gets no cookies, no storage, no origin, and the observer's web
  session token is never visible to it.
- The mounting client (the broker) pins the serving peer and the serving
  modulation at mount time. Guest messages name neither: a guest
  `query`/`execute` can only reach the pinned modulation on the pinned
  peer, and a guest `entrain` only the pinned peer. There is no way for
  guest content to address `sql-sqlite`, another modulation, another
  peer, or the browser's own network.
- Entrain gating stays server-side: an Entrain names only a point, so the
  serving node's `_policy` decides it exactly as it would for a direct
  client.
- The media and audio-duplex lanes are the exceptions to the modulation
  pin, on the same grounds as entrain: guest `media` and `duplex`
  requests ride the builtin `media`/`audio-duplex` modulations to the
  pinned peer, and their authority is the serving node's per-source
  `_policy` rows (actions `media` and `audio-duplex`), not the broker.
  The peer pin is never excepted.
- The microphone is a broker capability: the guest cannot obtain device
  permission from an opaque origin and never touches raw audio. A guest
  `duplex` start makes the BROKER prompt for (or reuse) mic permission,
  capture, encode to the header's `rsntr:accepts` format, and ship the
  bytes upstream; the guest only starts and stops the session and sees
  the downstream bytes, if any.
- On teardown (tab closed, point re-invoked, document replaced) the
  broker MUST damp every guest entrainment, abort in-flight requests, and
  reject pending guest promises.

Non-goals, stated so nobody reinvents them: no same-origin hosting, no
service-worker fetch interception (both would hand the guest ambient
authority), no hot mod install (the registry loads at serve start,
owner-channel.md section 5), no multi-file bundling beyond
`hologram <path>`.

## 6. The guest wire protocol

The guest speaks `postMessage` directly; the wire shapes below are the
normative contract, and the document ships its own shim (the reference
one is about thirty lines). The broker injects nothing into the srcdoc.

Every message both ways is `{hg: 1, m: <message>}`. The broker only
accepts events whose source is the mounted iframe's content window and
whose payload carries `hg: 1`; the guest posts to `parent` with target
origin `"*"`, because an opaque origin cannot be named (the broker's
source check is what stands in for origin checks).

Guest to broker:

- `{t: "ready"}` - handshake once listeners are installed; the broker
  answers `init`.
- `{t: "query", id, signal, params}` and `{t: "execute", id, signal,
  params}` - one envelope request. `id` is a guest-chosen integer used
  only for correlation. `params` is an array of JSON scalars; the broker
  maps string to a plain literal, integral number to `xsd:integer`,
  non-integral number to `xsd:decimal`, boolean to a boolean literal, and
  `null` to the `rsntr:null` marker.
- `{t: "entrain", id, point}` - entrain the pinned peer's point named by
  IRI string.
- `{t: "damp", id}` - end that entrainment.
- `{t: "media", id, signal}` - open the pinned peer's media source named
  by `signal` (the source name registered in its `_media` table). Rides
  mod `media`, gated server-side per source (section 5).
- `{t: "media-stop", id}` - close that media stream. Teardown closes all.
- `{t: "duplex", id, signal}` - open the pinned peer's audio-duplex
  source named by `signal` and start talking into it: the broker
  captures the microphone and streams it upstream (envelope doc section
  4.3, web-api.md section 7.1). Gated server-side by the `audio-duplex`
  action per source.
- `{t: "duplex-stop", id}` - hang up: the broker stops the microphone,
  sends the wire Fin, and ends the exchange. Teardown hangs up all.

Broker to guest:

- `{t: "init", peer, mod}` - answers `ready`; `peer` is the pinned
  64-hex endpoint id or `""` for the local node, `mod` the pinned tag.
  Informational: the guest cannot change them.
- `{t: "result", id, ok: true, res}` - the collected response. `res`
  carries `columns`, `rows` (each `{seq, cells}` with cells mapping
  column name to `{t, v}` typed values), `done` (`rowCount`,
  `affectedRows`, `lastInsertRowid`, `truncated`), and any `generic`
  frames as raw Turtle documents.
- `{t: "result", id, ok: false, denied, error: {code, reason}}` - the
  request failed; `denied` is true when the node answered
  `rsntr:Denied`.
- `{t: "entrained", id}` - the Entrain was acknowledged with Done.
- `{t: "vibration", id, seq, at}` - one vibration on that entrainment.
- `{t: "damped", id, reason}` - the entrainment ended; `reason` is
  present when it ended with an error.
- `{t: "media-header", id, contentType}` - the stream opened; the
  `rsntr:Media` header's content type, e.g.
  `"video/mp4; codecs=\"avc1.640028, mp4a.40.2\""`. What follows on this
  id is the raw byte feed, exactly the wire shape of web-api.md
  section 6.
- `{t: "media-data", id, chunk}` - one slice of the raw feed as an
  `ArrayBuffer`, passed as a postMessage transferable (zero-copy). The
  guest typically appends it to an MSE `SourceBuffer`.
- `{t: "media-end", id}` - the feed ended (source closed, `media-stop`,
  or teardown).
- `{t: "media-error", id, error: {code, reason}}` - the stream failed to
  open or died; same code vocabulary as `result` errors.
- `{t: "duplex-open", id, contentType, accepts}` - the talk session is
  live: the mic is flowing upstream in the `accepts` format.
  `contentType` is null for a pure talk sink; when present, the source's
  downstream bytes follow as `duplex-data`.
- `{t: "duplex-data", id, chunk}` - one slice of the source's downstream
  feed as a transferable `ArrayBuffer` (only when `contentType` is
  present).
- `{t: "duplex-ended", id}` - the session ended (source closed,
  `duplex-stop`, or teardown).
- `{t: "duplex-error", id, error: {code, reason}}` - the session failed
  to open or died; a refused microphone permission arrives here as
  `engine-error`.

## 7. Errors

`error.code` is always one of the seven envelope error codes
(`auth-denied`, `timeout`, `limit-exceeded`, `mod-unsupported`,
`engine-error`, `protocol-error`, `point-unknown`), so guest code handles
one vocabulary:

- an `rsntr:Error` frame passes its code and message through;
- `rsntr:Denied` maps to `auth-denied` with `denied: true`;
- an HTTP 401 from the web session maps to `auth-denied` with reason
  `"web session expired"` (the console surfaces its token overlay; the
  guest is never shown the token);
- transport and fetch failures map to `protocol-error`;
- teardown rejects everything pending with `protocol-error` and reason
  `"hologram unmounted"`;
- a malformed guest message is answered with `protocol-error` and is
  never forwarded to the node.

## 8. Settled questions

- Response classes only, requests ride `rsntr:signal` verbs: the request
  path stays strict (envelope doc section 5), exactly the chat move
  (chat-protocol.md section 3).
- Opaque-origin sandbox plus postMessage broker, not service-worker
  interception and not same-origin hosting: the guest must never inherit
  the console's origin, cookies, or token (PLAN.md Q14).
- Peer and mod pinning happens in the broker, at mount time, from the
  fetch that produced the hologram; guest input is never consulted.
- The wire contract, not a JS API, is normative: documents bring their
  own shim, so guests and brokers evolve independently.
