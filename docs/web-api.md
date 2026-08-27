# rsntr Web API: RDF over HTTP for the Browser

Status: accepted, 2026-07-29. This document is the normative HTTP API of the
rsntr web interface (crate `web`, milestone M5). It implements the transport
decision in [browser-client-http2.md](browser-client-http2.md) and resolves
its four open items (control feed format, URL scheme, in-stream record
framing, auth/session). Normative companions:
[rdf-envelope-protocol.md](rdf-envelope-protocol.md) (frames, classes, error
codes), [connection-protocol.md](connection-protocol.md) (choreography),
[projection-protocol.md](projection-protocol.md) (entrainment). Decision
record: ../PLAN.md (Q5).

The server is an axum HTTP server started inside the node process by
`rsntr serve --web [addr]` (default `127.0.0.1:2718`). It terminates iroh
entirely: the browser speaks RDF envelope frames over plain HTTP, and the
server is the node. There is one server per node, one node per user, no
accounts.

## 1. The model: iroh concepts, HTTP mirrors

Every byte of RDF that crosses this API is the same wire format the iroh
transport carries: u32-LE length-prefixed frames, each one self-contained
Turtle document, implied prefix block never transmitted, 256 KiB frame
budget (envelope doc, section 7). HTTP replaces only the stream layer.

| iroh concept | HTTP mirror |
|---|---|
| authenticated connection | the session: capability token + one origin |
| hello exchange | none per connection; `GET /api/meta` serves capabilities |
| client-opened bi stream (one request) | `POST /request`: request frame up, response frames stream down |
| entrainment stream | `POST /entrain`: Entrain frame up, Done + Vibration frames stream down |
| server-opened stream | announced on `GET /feed`, read via `GET /stream/{id}`, written via `POST /stream/{id}` |
| stream reset (either side) | `AbortController` / H2 RST_STREAM / connection close |
| frame | identical bytes: u32-LE length + Turtle document |

Everything the browser does, streaming or JSON, is translated by the server
into envelope requests and submitted into the node's serving pipeline with
the node's own EndpointId as the peer: the local surface acts as the owner,
and the node treats its own id as admitted at the peer gate. The policy
tier still needs allow rows for the owner identity: web-server startup
ensures, only when absent, a `_peers` row plus `(owner, '*',
{read,write,entrain}, 'allow')` policy rows. Nothing bypasses the peer
gate, the footprint authorizer, the authenticator chain, limits, or the
audit trail. A `_policy` row that denies the owner (more or equally
specific than the seeded allows) denies the browser.

## 2. Auth and session

A capability token is 32 random bytes, printed by the CLI as base64url (43
characters). The CLI persists it in the node directory (`rsntr.web-token`,
mode 0600, a secret file like `rsntr.key`), so it is stable across serve
runs; `rsntr serve --web --new-web-token` rotates it. The CLI prints the
entry URL with the token in the URL fragment, and includes it in its
`--json` output:

```
web interface: http://127.0.0.1:2718/#GLjcpeq0Zh0Cn9pCHmWXHxwLGSlWzcMV8H3B2S_Ph84
```

The fragment is chosen deliberately: fragments are never sent on the wire,
never appear in server or proxy logs, and never leak through Referer
headers. The token MUST NOT appear in a query string, on any endpoint.

The page script reads `location.hash`, clears the fragment via
`history.replaceState`, and proves the token to the server with a `POST
/api/session` (authenticated like any other call). The `204` response
installs the persistent cookie:

- cookie: `rsntr_token=<token>; Path=/; HttpOnly; SameSite=Strict;
  Max-Age=31536000`, set only by the server, and only in exchange for
  proof of the token: an authenticated `POST /api/session`, or refreshed
  on a `GET /` that already presents the valid token. A bare `GET /`
  never receives one. The cookie is what makes `EventSource`, plain `<a>`
  CSV downloads, and media elements work (none of them can set headers),
  and what lets a signed-in browser or an installed PWA come back with no
  fragment at all. `HttpOnly` keeps it out of script storage.
- header: `Authorization: Bearer <token>`, the equivalent for callers
  that hold the token (the page script uses it until the cookie is
  installed; scripts and curl use it directly).

The server accepts either; comparison is constant-time. The routes served
without a token are `GET`/`HEAD` of the UI shell (`/`, which contains no
data) and of the static PWA assets: `/manifest.webmanifest`, `/sw.js`,
`/icon-192.png`, `/icon-512.png`, `/icon-maskable-512.png`,
`/apple-touch-icon.png`,
`/favicon.ico` (all compiled in, all data-free). The committed icon PNGs
in `web-ui/` derive from the Resonator logo (`media/logo/` in the parent
repo): the `-192`/`-512` are the logo circle cut onto transparency, the
maskable and apple-touch variants compose it on the rim color `#50325D`
(ImageMagick; circle mask + `-gravity center -composite`). Every other
route answers `401` with body
`{"ok":false,"error":{"code":"unauthorized","reason":"missing or invalid token"}}`
when the token is absent or wrong.

Session model: one session daemon per user, no accounts, no login flow. All
holders of the token are the owner. The server sets no CORS headers, so
cross-origin pages cannot call the API even if they somehow obtained the
token; `SameSite=Strict` keeps the cookie out of cross-site requests.

Caveat: cookies ignore ports, so two nodes serving on the same host (say
`127.0.0.1:2718` and `127.0.0.1:2719`) share the one `rsntr_token` cookie
and sign each other out; use the fragment URL to switch. The service
worker is install-only (no caching, no offline behavior); installability
requires a secure context, which loopback is, and a non-loopback bind
needs a TLS proxy in front (the CLI already warns about cleartext).

## 3. Content types and record framing

Two content types carry the protocol:

- `application/rsntr-frames`: a sequence of envelope frames, each u32-LE
  length prefix + one complete UTF-8 Turtle document, exactly the protocol
  crate's framing (`MAX_FRAME_LEN` = 256 KiB, implied prefix block). Used
  for `/request`, `/entrain`, and `/stream/{id}` bodies in both directions.
- `text/event-stream`: the SSE control feed (`/feed`).

In-stream record framing is the length prefix, not HTTP chunk boundaries:
proxies may re-chunk at will, so a reader MUST reassemble frames from the
byte stream by length prefix and MUST NOT assume a chunk contains a whole
frame or a whole number of frames. This is open item 3 of the decision doc,
resolved: length-prefixed records, byte-identical to the iroh wire, so the
protocol crate's decoder is reused unchanged on both ends (the browser side
is a ~30-line reader over `response.body.getReader()`).

A request body frame that exceeds the budget answers `413`. A response
frame never exceeds it, by construction. Only the `turtle` encoding rides
this API; `compact-postcard` is never offered on the browser path.

## 4. The control feed: GET /feed

Open item 1, resolved: SSE. It is the simplest thing that survives every
proxy, has a built-in event framing and reconnect story, and needs zero
client library. ndjson was rejected because it reinvents exactly the parts
SSE ships for free.

The browser opens one long-lived `GET /feed` at session start (via `fetch`
with the Bearer header, or `EventSource` with the cookie) and keeps it open
for the life of the page. The server announces server-initiated streams and
session-level RDF notifications on it. Response headers:

```
Content-Type: text/event-stream
Cache-Control: no-store
X-Accel-Buffering: no
```

Events, by `event:` name:

- `stream`: a server-initiated stream exists; data is one JSON object
  `{"id":"01K1...","mod":"chat"}` where `id` is a server-minted ULID and
  `mod` names the modulation the stream belongs to. The browser reacts by
  fetching `GET /stream/{id}`.
- `closed`: data `{"id":"01K1..."}`; the announced stream ended or was
  abandoned before the browser fetched it.
- `envelope`: one session-level envelope object as a Turtle document in the
  data field (SSE carries multi-line payloads as repeated `data:` lines;
  the implied prefix block applies, no length prefix inside SSE). Used for
  connection-independent notifications: `rsntr:Presence` of peers,
  `rsntr:Decision` objects answering parked `_inbox` items, and any future
  session-scoped RDF the node wants the page to see. Unknown classes here
  follow the envelope's Generic rule: render generically or ignore, never
  error.

The server emits a comment line (`: ping`) at least every 30 seconds as
keepalive, and sets `retry: 2000`. The feed has no replay: there are no
`id:` fields and `Last-Event-ID` is ignored, matching the entrainment
stance (vibrations are ticks, not a log). On reconnect the server
re-announces every still-open server-initiated stream; anything else the
page missed it recovers by re-reading, exactly like a reconnecting iroh
peer. Multiple feeds may be open (several tabs); every feed receives every
announcement.

Example:

```
$ curl -sN -H "Authorization: Bearer $T" http://127.0.0.1:2718/feed
retry: 2000

: ping

event: stream
data: {"id":"01K1F2Q8Z3T9W6E2R8T4Y0X6A3","mod":"chat"}

event: envelope
data: [] a rsntr:Presence ;
data:    rsntr:at "2026-07-29T10:00:00"^^xsd:dateTime ;
data:    rsntr:status "around" ;
data:    rsntr:endpoint "e00c36119fb26262a12a5571f918966caf0b4cefe3c72ed046a1c1bbf1cd63d9" .
```

## 5. Streams: GET and POST /stream/{id}

Open item 2, resolved: `/stream/{id}` for both halves of a logical stream,
read by GET, written by POST. `{id}` is the ULID from the feed
announcement. This is the general mechanism for streams the server
initiates toward the browser (the HTTP stand-in for a server-opened iroh
stream, which H2 cannot express as push).

Read half:

- `GET /stream/{id}` answers `200` with `Content-Type:
  application/rsntr-frames` and streams the frames of the stream until it
  ends. One reader per stream: a second concurrent GET answers `409`. An
  unknown or already-consumed id answers `404`. Browser abort (H2
  RST_STREAM, or connection close on H1) resets the underlying stream, and
  the server releases everything attached to it.

Write half, for streams that have one (the announcing modulation defines
whether they do):

- `POST /stream/{id}` with `Content-Type: application/rsntr-frames` and one
  or more complete frames in the body appends those records to the stream's
  write half, in body order. Response is `204` with no body. Ordering
  across POSTs is the client's job: a client MUST await the previous POST
  on a stream before issuing the next (the baseline for all browsers).
  POSTing to a stream with no write half, or after FIN, answers `409`.
- FIN: the final POST carries the header `Rsntr-Fin: 1` (an empty body
  with only the header is a bare FIN). This maps to finishing the iroh
  send half.

Duplex upgrade (optional, Chromium and Safari): a client MAY replace the
POST sequence with one long-lived streaming-upload POST to the same URL
(`duplex: "half"`, `ReadableStream` body); end of body is the FIN.
Feature-detect and fall back to sequential POSTs on Firefox. Servers MUST
accept both; nothing about the stream's semantics changes.

Status: the endpoints and framing here are normative now; v1 of the web
crate implements them, but no v1 modulation opens server-initiated streams
yet. First real traffic arrives with chat (M7) and media viewing (M10).

## 6. Requests: POST /request

The HTTP mirror of one client-opened iroh bi stream, and the workhorse of
the API.

Request: `POST /request`, `Content-Type: application/rsntr-frames`, body =
exactly one framed envelope request: an `rsntr:Query` or `rsntr:Execute`
in any modulation the node serves (`sql-sqlite`, `sparql`, `help`,
`projection`, `media`, ...). Any other envelope class in the body is a
`protocol-error` (requests parse strictly; `/entrain` is the one carve-out
with its own endpoint). The server honors a well-formed client ULID in
`rsntr:id` and mints one otherwise, exactly as on the wire.

Response: `200` with `Content-Type: application/rsntr-frames`, the body
streaming the response frames exactly as they would cross an iroh stream:
`Result`/`Row`.../`Done`, or `Graph`.../`Done` for CONSTRUCT/DESCRIBE, or a
lone `Help`, `Done`, `Denied`, or `Error` frame; Generic frames pass
through verbatim. H2 flow control is the backpressure: a page that stops
reading stalls the pipeline's producer. Aborting the fetch resets the
request stream and stops execution.

Status codes carry the outcome only when it is known before the first
response frame is written: in that case the server SHOULD set the mapped
status from section 10 and the body still carries the corresponding
`rsntr:Error` or `rsntr:Denied` frame. Once streaming has begun the status
is already `200` and failures arrive in-band as frames, exactly like the
wire. Clients MUST therefore read outcomes from frames, not from status
codes; the status is a convenience for curl and logs.

Targeting a peer: `POST /request?peer=<64-hex-endpoint-id>` makes the
server dial that admitted peer over iroh, forward the request frame, and
relay the response frames verbatim into the response body. Absent `peer`,
the request is served by the local node through the local pipeline. This
is how the browser reaches the rest of the network: the server is its iroh
stack. An unknown or undialable peer answers `502` with an `rsntr:Error`
frame (`engine-error` is not used for this; the code is `protocol-error`
with a reason naming the dial failure).

Media: a `media` modulation request answers, as on the wire, with one
framed `rsntr:Media` header followed by the raw unframed byte stream to
end of body. This is the defined shape for feeding MSE/WebCodecs; it ships
with media viewing (M10), not v1.

Worked example, a SQL query end to end (the helper writes the length
prefix; the response is framed, shown here unframed for readability):

```
$ python3 -c '
import struct, sys
doc = ("[] a rsntr:Query ; "
       "rsntr:id \"01K1F3A9B2C4D6E8F0G2H4J6K8\" ; "
       "rsntr:mod \"sql-sqlite\" ; "
       "rsntr:signal \"SELECT title, mtime FROM notes\" .").encode()
sys.stdout.buffer.write(struct.pack("<I", len(doc)) + doc)
' | curl -sN -X POST http://127.0.0.1:2718/request \
    -H "Authorization: Bearer $T" \
    -H "Content-Type: application/rsntr-frames" \
    --data-binary @-

[] a rsntr:Result ; rsntr:id "01K1F3A9B2C4D6E8F0G2H4J6K8" ;
   rsntr:column ("title" "mtime") ; rsntr:declType ("TEXT" "TEXT") .
[] a rsntr:Row ; rsntr:seq 0 ; rsntr:col_title "groceries" ; rsntr:col_mtime "2026-07-04T10:11:12" .
[] a rsntr:Done ; rsntr:id "01K1F3A9B2C4D6E8F0G2H4J6K8" ; rsntr:rowCount 1 ; rsntr:truncated false .
```

## 7. Entrainment: POST /entrain

Request: `POST /entrain`, `Content-Type: application/rsntr-frames`, body =
one framed `rsntr:Entrain` naming a Sympathetic point. `?peer=` targets a
remote node as in section 6.

Response: `200 application/rsntr-frames`, streaming exactly the wire
choreography: one `rsntr:Done` (entrained), then one `rsntr:Vibration`
frame per tick, until damped. `rsntr:Denied` or `rsntr:Error`
(`point-unknown`) instead of the Done when entrainment is refused.

Damping: the HTTP response has no write half, so the in-band `rsntr:Damp`
of the wire protocol does not exist here; aborting the fetch is the damp,
and it is equivalent (entrainment is connection-scoped, and the aborted
exchange is the connection). The server damps a slow consumer the same way
it does on the wire: coalesce first, then an `rsntr:Error` frame with
`limit-exceeded` and end of body. The page re-entrains and catches up
through a Radiant; nothing is replayed.

The well-known point `<urn:rsntr:projection-changed>` is entrainable here
by IRI without a prior projection fetch, per the projection doc; the UI
uses it to keep its panels fresh.

## 7.1 Duplex upstream: POST /duplex/{id}

The audio-duplex modulation (envelope doc section 4.3) is the one
exchange where the browser must also SEND raw bytes. The wire is a full
bi stream, but a browser on plain HTTP/1.1 cannot stream a request body
(`duplex: "half"` fetch is HTTP/2-only), so the upstream is a sequence of
ordinary POSTs, in the spirit of the /stream write path the decision doc
specified.

Opening: the exchange starts as a normal `POST /request` carrying a Query
with `rsntr:mod "audio-duplex"` (with `?peer=` for a remote serving
node). The server registers an upstream channel under the request's
normalized ULID before the response begins, and the response's
`rsntr:AudioDuplex` header carries that id back, so the id in the header
is always POSTable by the time the client reads it. After the header the
response body is the raw downstream feed (absent when the header carries
no `rsntr:contentType`).

Upstream: `POST /duplex/{id}` with `Content-Type:
application/octet-stream`; the body's bytes (in the header's
`rsntr:accepts` format) are appended to the wire. The body is read
incrementally and the bounded channel toward the wire is the
backpressure. The header `Rsntr-Fin: 1` appends the wire Fin after the
body (an empty body with only the Fin header is the clean hangup): the
source's stdin sees EOF while the downstream may keep flowing.

The ordering contract: bytes are appended in POST-completion order, so
the client MUST serialize its POSTs (await each before issuing the next)
and MUST put the Fin on the last one. Concurrent POSTs to one id have
unspecified interleaving.

Status codes: `204` accepted (including the empty Fin-only body); `404`
unknown or already-ended exchange (also what a straggler sees after Fin
or after the response ended - harmless, the session is over); `409` the
exchange died mid-body; `401` as everywhere. The exchange's registry
entry lives exactly as long as its response stream; aborting the
`/request` fetch tears the whole session down, source process included.

When an h2 front end exists, one streaming-upload POST can replace the
sequence with the same wire shape; the sequential form stays the
baseline (browser-client-http2.md).

## 8. Convenience API for the UI

The single-file UI drives everything through JSON endpoints under `/api`.
They exist so the UI (and curl, and agents) need no frame codec for the
day-1 tools; each one is a thin translation onto the same envelope
requests, submitted through the same local pipeline as section 1 describes.
Nothing under `/api` can do anything `/request` cannot.

Conventions:

- Requests and responses are `application/json` unless stated. All
  responses carry `Cache-Control: no-store`.
- Success shapes carry `"ok": true`. Failures carry
  `{"ok":false,"error":{"code":"...","reason":"..."}}` with the mapped
  status of section 10, except denials:
  `{"ok":false,"denied":"<reason>"}` with `403`. These are the CLI's
  stable `--json` shapes, kept field-for-field.
- Engine values in JSON, both directions (matching the CLI): SQL NULL is
  JSON `null`, integers and floats are JSON numbers (a non-finite float
  falls back to its string form), text is a JSON string, a blob is
  `{"blob_hex":"<hex>"}`, a blob reference is
  `{"blobref":{"hash":"blake3:...","bytes":N}}` (read-only).
- Table-name and rowid path segments address existing objects; identifiers
  are quoted server-side, never interpolated. An unknown table or rowid is
  `404`. Reserved `_` tables are addressable like any other and stand or
  fall by policy alone.

### GET /

The UI: one self-contained HTML file, inline assets, no build step. Served
without a token (section 2), alongside the static PWA assets (manifest,
install-only service worker, icons; also section 2). This route and the
`/api` family are the only HTML/JSON surface; everything else on the
server speaks frames or SSE.

### GET /o/{...} - deep links

Every projection path and hologram has a URL. Any GET under `/o/` serves
the same data-free shell as `/` (no token needed to fetch it; the
exemption is GET/HEAD only); the page's client router reads the path
after authenticating and opens the addressed view directly, so a
hologram link behaves as a single-page app of that hologram. Forms:

- `/o/<peer>/proj` - the peer's root projection
- `/o/<peer>/proj/<path>` - a drilled projection; `<path>` is the opaque
  projection path percent-encoded as one segment (paths have no grammar;
  bookmarking one is projection-protocol.md's own clause)
- `/o/<peer>/holo/<mod>` - the mod's hologram (signal `hologram` by
  convention)

`<peer>` is a 64-hex endpoint id, `local` for the serving node, or a
petname (petnames resolve after the contact list loads; hex opens
immediately). Auth: the fragment still carries the token
(`/o/.../holo/cameras#<token>`), or the persistent cookie signs the
request in, or the token overlay asks once. The console reflects
navigation into the address bar with `history.replaceState`, so reload
and bookmark always match the screen; the copy-link buttons deliberately
produce URLs WITHOUT the token - a shared capability is a decision, not
an accident.

### POST /api/session

Issues the persistent auth cookie (section 2). Authenticated like any
other route, so reaching it is the proof of token; answers `204` with the
`Set-Cookie` header and no body.

### GET /api/meta

The session's hello substitute.

```json
{
  "ok": true,
  "node_id": "e00c36119fb26262a12a5571f918966caf0b4cefe3c72ed046a1c1bbf1cd63d9",
  "ver": "0.1",
  "mods": ["help", "sql-sqlite", "sparql", "projection", "media", "chat"],
  "tables": [
    {"name": "notes", "reserved": false, "comment": "meeting notes by date"},
    {"name": "_peers", "reserved": true, "comment": "admitted peers: endpoint id, petname, dial address hints"}
  ]
}
```

`node_id` is the node's EndpointId (64 hex), `ver` and `mods` come from the
same `_rsntr`-backed source as the wire hello, `tables` lists every table
in the database with the reserved flag for `_` names and its description
from the reserved `_comments` table (`null` when missing).

Table descriptions are mandatory in spirit: every reserved table ships a
seeded comment, scaffolds (chat init, example mod seeds) describe the
tables they create, and the console demands a description before an
operator creates a table (csv import) and nags on any table without one.
A database a human can operate starts with knowing what each table is
for.

### GET /api/peer/{endpoint_id}

One admitted peer: the local `_peers` row plus a live probe surfacing
the peer's wire hello, which every dial receives and previously
discarded. `?local=1` skips the probe (list rendering needs rows only);
a server without a transport answers `"live": null`.

```json
{
  "ok": true,
  "peer": {
    "endpoint_id": "<64 hex>",
    "name": "alice",
    "addrs": ["192.168.1.7:41641"],
    "added_at": "2026-08-01 10:00:00",
    "last_seen": "2026-08-06 09:12:44",
    "notes": "front desk"
  },
  "live": {
    "reachable": true,
    "ver": "0.1",
    "encodings": ["turtle"],
    "mods": ["help", "sql-sqlite", "sparql", "projection", "media", "chat"],
    "hint": "ask help"
  }
}
```

An unreachable peer answers `"live": {"reachable": false, "error":
"..."}` within the 6 second probe bound. Honesty notes: `last_seen` is
written only by presence beacons and is often null (it is not a
liveness signal), and a pooled connection to a peer that just died can
report `reachable: true` for up to the transport's 10 second idle
timeout - the first real request is the authoritative probe. A peer id
that is not 64 hex answers 400; an id absent from `_peers` answers 404.

### GET /api/table/{name}?limit&offset

Page through a table. `limit` defaults to 100, caps at 1000; `offset`
defaults to 0.

```json
{
  "ok": true,
  "table": "notes",
  "columns": [
    {"name": "title", "decltype": "TEXT", "pk": false, "notnull": false},
    {"name": "body", "decltype": "TEXT", "pk": false, "notnull": false}
  ],
  "rows": [["groceries", "milk, eggs"], ["reading list", null]],
  "rowids": [1, 2],
  "total": 2,
  "limit": 100,
  "offset": 0
}
```

`rows` are arrays in `columns` order. `rowids` is parallel to `rows`; for
a WITHOUT ROWID table it is all `null` and the row endpoints below do not
apply. `total` is the unpaged row count.

### POST /api/table/{name}/rows

Insert one row. Body: `{"values": {"title": "groceries", "body": null}}`,
column names to values; omitted columns take their defaults. Answers `201`:

```json
{"ok": true, "last_insert_rowid": 3}
```

### PATCH /api/table/{name}/rows/{rowid}

Update one row by rowid. Body: `{"values": {"body": "milk, eggs, bread"}}`.
Answers `{"ok": true, "affected_rows": 1}`; a vanished rowid answers `404`.

### DELETE /api/table/{name}/rows/{rowid}

Answers `{"ok": true, "affected_rows": 1}`, or `404`.

These three are the data editor's verbs; on the pipeline they are ordinary
`rsntr:Execute` statements in `sql-sqlite`, so the audit trail records
every edit and policy can forbid any of it.

### POST /api/sql

Body: `{"sql": "SELECT title FROM notes WHERE mtime > ?", "params": ["2026-07-01"]}`.
`params` is optional, positional, values encoded as above. The server
classifies the statement (query vs execute) exactly as the CLI does.
Response is the CLI's query report shape:

```json
{
  "ok": true,
  "id": "01K1F3A9B2C4D6E8F0G2H4J6K8",
  "columns": ["title"],
  "rows": [["groceries"]],
  "row_count": 1,
  "affected_rows": null,
  "last_insert_rowid": null,
  "truncated": false
}
```

For a write, `columns`/`rows` are empty and `affected_rows` /
`last_insert_rowid` are set.

### POST /api/sparql

Body: `{"query": "SELECT ?s ?title WHERE { ?s <http://example.org/notes#title> ?title }"}`.
The form of the query decides the response:

- SELECT and ASK: the `columns`/`rows` report shape above, one column per
  projected variable. Cells are N-Triples lexical text end to end, exactly
  as the sparql modulation puts them on the wire: an IRI as
  `<http://...>`, a literal as `"groceries"` or
  `"42"^^<http://www.w3.org/2001/XMLSchema#integer>`, a blank node as
  `_:b0`; an unbound variable is `null`. The UI renders these; it does not
  get pre-cooked values.
- CONSTRUCT and DESCRIBE:
  `{"ok":true,"id":"...","triples":["<s> <p> \"o\" .", ...],"triple_count":N,"truncated":false}`,
  each entry one N-Triples statement.
- SPARQL Update (INSERT DATA / DELETE DATA / DELETE WHERE), classified as
  the CLI classifies it and ridden as `rsntr:Execute`:
  `{"ok":true,"id":"...","affected_rows":N}`.

```
$ curl -s -X POST http://127.0.0.1:2718/api/sparql \
    -H "Authorization: Bearer $T" -H "Content-Type: application/json" \
    -d '{"query":"ASK { ?s ?p ?o }"}'
{"ok":true,"id":"01K1F3C2D4E6F8G0H2J4K6M8N0","columns":["ask"],"rows":[["\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"]],"row_count":1,"affected_rows":null,"last_insert_rowid":null,"truncated":false}
```

### POST /api/turtle

Load Turtle into the store. Body: raw Turtle, `Content-Type: text/turtle`;
optional `?base=<iri>` for relative-IRI resolution. Implementation note:
this cannot ride the pipeline as an `sql-sqlite` execute, because the
enforce-mode authorizer denies `rdf_load_turtle`'s inner `rdf_*` writes.
The server instead gates it with exactly the sparql modulation's write
path (same authenticator chain, same `rdf_*` write footprint, same
`_audit` row) and then applies the load on the db thread; `_policy` on
the `rdf_*` tables gates it like every other store write.

```
$ curl -s -X POST "http://127.0.0.1:2718/api/turtle" \
    -H "Authorization: Bearer $T" -H "Content-Type: text/turtle" \
    --data-binary '@notes.ttl'
{"ok":true,"triple_count":12}
```

`triple_count` is the number of triples loaded. A body that is not valid
Turtle answers `400` with `engine-error` and the parser's message.

### GET /api/csv/{table}

The whole table as RFC 4180 CSV, `Content-Type: text/csv; charset=utf-8`,
`Content-Disposition: attachment; filename="{table}.csv"`, streamed. First
line is the header (column names). Cell encoding: NULL is an empty
unquoted field, an empty string is `""`, numbers are their SQL lexical
form, text is quoted when it contains commas, quotes, or newlines, a blob
is `x'<hex>'`. This is the file Excel opens; it is also byte-compatible
with the importer below and with `rsntr csv export`, which shares the
implementation.

### POST /api/csv/{table}

Upload CSV, `Content-Type: text/csv`, body as above (header line
required). Creates-or-appends:

- If the table does not exist, it is created with one TEXT column per
  header field, then the rows are inserted.
- If it exists, the header must be exactly the table's column set (any
  order); rows are inserted with sqlite's usual type affinity. A
  mismatched header answers `400`.

Cell decoding mirrors export: empty unquoted field is NULL, `""` is the
empty string, `x'<hex>'` is a blob, everything else binds as text.

```
$ curl -s -X POST http://127.0.0.1:2718/api/csv/notes \
    -H "Authorization: Bearer $T" -H "Content-Type: text/csv" \
    --data-binary @notes.csv
{"ok":true,"table":"notes","created":false,"rows_inserted":41}
```

Row data in both CSV directions runs through the pipeline as
`sql-sqlite` statements, so a large import is as audited and as
policy-bound as typing the INSERTs by hand. Two implementation notes:
the CREATE TABLE of a first-time import cannot ride the pipeline (DDL is
categorically refused there); it runs directly with its own `_audit`
record, and only after the authenticator has allowed the import's
writes. Export builds the document server-side and answers
`limit-exceeded` (422) if the table exceeds the node row cap; a
truncated file is never silently produced.

## 9. What rides which surface

The split, so implementers and the UI stay honest:

- Frames (`/request`, `/entrain`, `/stream`): everything that is a wire
  conversation: queries in any modulation, projection fetches, help,
  entrainment, remote peers, media, chat. The projection browser and the
  RDF renderer consume frames directly, and so does the hologram broker:
  a hologram fetch and every request a mounted hologram guest makes ride
  `/request` (with `?peer=` for a remote serving node), guest
  entrainments ride `/entrain`, and guest media streams ride `/request`
  as ordinary media opens, relayed to the guest as transferable byte
  chunks (hologram-protocol.md).
- SSE (`/feed`): announcements and session-level envelope objects only;
  never bulk data.
- JSON (`/api`): the day-1 human tools (SQL composer, table viewer and
  editor, SPARQL composer, Turtle loader, CSV both ways) and `meta`.
  JSON endpoints are local-node only; anything remote goes through
  `/request?peer=`.

## 10. Error mapping

Envelope error codes (envelope doc, section 4) map to HTTP statuses
wherever a status can still be chosen (JSON endpoints always; framed
endpoints only before the first response frame, per section 6):

| envelope outcome | HTTP status |
|---|---|
| `rsntr:Denied` (any tier) | 403 |
| `auth-denied` | 403 |
| `timeout` | 504 |
| `limit-exceeded` | 422 |
| `mod-unsupported` | 501 |
| `engine-error` | 400 |
| `protocol-error` | 400 |
| `point-unknown` | 404 |

HTTP-native conditions, with no envelope counterpart:

| condition | HTTP status |
|---|---|
| missing or invalid token | 401 |
| unknown route, stream id, table, or rowid | 404 |
| method not allowed on route | 405 |
| stream conflict (second reader, write after FIN, no write half) | 409 |
| frame or body over budget | 413 |
| wrong content type | 415 |
| peer dial failure on `?peer=` | 502 |

On JSON endpoints the body is the error shape of section 8; on framed
endpoints the body carries the corresponding `rsntr:Error` or
`rsntr:Denied` frame so a frame-only client never needs the status.
Exit-code note for agents: these statuses line up with the CLI convention
(0 ok, 1 error, 2 denied = 403, 3 timeout = 504).

## 11. v1 versus upgrade path

Implemented in v1 (crate `web`, M5):

- `GET /` and the single-file UI.
- Token auth as specified (header and cookie), 401 everywhere else.
- `GET /feed` (SSE) with `stream`, `closed`, `envelope` events.
- `POST /request` with local pipeline and `?peer=` relay; sequential-POST
  ordering rules.
- `POST /entrain` with abort-as-damp and server-side damping.
- `GET`/`POST /stream/{id}` mechanics (announced streams first appear with
  chat, M7).
- The whole `/api` family of section 8.
- Error mapping of section 10.

Specified now, implemented later:

- Duplex streaming-upload POST (Chromium/Safari) as the write-half upgrade
  on `/stream/{id}`: optional, feature-detected, semantics unchanged.
- Media responses (`rsntr:Media` header + raw bytes) feeding MSE or
  WebCodecs: M10, on the `/request` shape already defined.
- Chat streams over `/feed` + `/stream/{id}`: M7.
- `.xlsx` export next to CSV: later nice-to-have (PLAN.md Q7).

Never on this API: `compact-postcard` (Turtle only in the browser path),
accounts, CORS, tokens in query strings.

## 12. Operational notes

- HTTP versions: browsers speak HTTP/2 only over TLS. On the default plain
  `http://127.0.0.1` the browser uses HTTP/1.1, where each stream costs a
  TCP connection and browsers cap at ~6 per origin; the feed plus a
  handful of streams fits, and the v1 local UI is fine there. For many
  parallel streams, heavy media, or any remote access, front the server
  with a TLS-terminating proxy (Caddy, nginx) so the browser gets h2; the
  server itself speaks h2 (prior-knowledge/h2c and via ALPN) and h1.1.
- Any reverse proxy in front must not buffer streaming responses (the
  server sets `X-Accel-Buffering: no`; configure `proxy_buffering off` /
  flush-on-write anyway) and must allow long-lived requests on `/feed`,
  `/request`, `/entrain`, and `/stream`.
- Raise `SETTINGS_MAX_CONCURRENT_STREAMS` above the h2 default if many
  parallel streams are expected.
- The server binds loopback by default. Binding a non-loopback address
  without TLS in front sends the bearer token in cleartext; the CLI warns
  when asked to do it.
- Give heavy media streams their own origin (a second `--web` port) if
  they starve RDF traffic in practice, per the decision doc.
