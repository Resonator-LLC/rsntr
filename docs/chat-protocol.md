# Chat and File Transfer for the Resonator Network

Status: normative for v3, 2026-07-29. Designed fresh (no POC source exists);
implements decisions Q6 (rooms always have a host) and Q8
(chat state lives in user-space tables created by `rsntr chat init`).
Normative companions: [rdf-envelope-protocol.md](rdf-envelope-protocol.md)
(envelope, BlobRef, framing), [connection-protocol.md](connection-protocol.md)
(admission, peer gate), [projection-protocol.md](projection-protocol.md)
(points, entrainment). Implementation lands in milestone M7.

## 1. Chat is a customer, not a core feature

Chat is the reference modulation: it proves that a real application is
buildable from the four public verbs and nothing else. The core gains one
builtin modulation tag (`chat`) and one vocabulary class (`rsntr:Message`);
everything stateful lives in ordinary user-space tables that `rsntr chat
init` scaffolds, exactly as a third-party application would create its own.

The four verbs, as chat uses them:

- write own tables: `chat_messages`, `chat_rooms`, `chat_members`, plain
  tables with no leading underscore, owned by the user, editable with the
  same SQL as any other data;
- INSERT into `_outbox`: every send, online or offline, is one outbox row;
  the outbox worker is the only thing that ever touches the transport;
- entrain a Sympathetic point: `<urn:rsntr:chat-inbox>` vibrates when a
  message lands, nudging watchers to re-read;
- re-read a Radiant: history is a SQL query over `chat_messages`, exposed as
  the point `<urn:rsntr:chat:history>`.

There is no chat daemon, no chat queue, no chat sync protocol. A node with
the scaffold serves `chat`; a node without it does not advertise the tag.
Delete the three tables and the projection rows and chat is gone without a
trace in the core.

## 2. The message is a noun

Types are nouns, not verbs: there is no "send" request kind. A chat message
is an RDF object of class `rsntr:Message`, and sending is the ordinary write
choreography carrying that object. The same object is what gets stored, what
gets fanned out to room members, and what a client renders; the wire, the
database, and the UI all describe one noun.

A minimal direct message:

```turtle
[] a rsntr:Message ;
   rsntr:id "01K1CH4T0001XMPL0000000001" ;
   rsntr:at "2026-07-29T12:00:00"^^xsd:dateTime ;
   rsntr:body "lunch at noon?" .
```

A room message with an attachment:

```turtle
[] a rsntr:Message ;
   rsntr:id "01K1CH4T0002XMPL0000000002" ;
   rsntr:from "e00c36119fb26262a12a5571f918966caf0b4cefe3c72ed046a1c1bbf1cd63d9" ;
   rsntr:room <urn:rsntr:room:01K1R00M0001XMPL000000000A> ;
   rsntr:at "2026-07-29T12:03:00"^^xsd:dateTime ;
   rsntr:body "the seedlings came up" ;
   rsntr:attachment [ a rsntr:BlobRef ;
      rsntr:hash "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262" ;
      rsntr:bytes 4194304 ;
      rsntr:name "seedlings.jpg" ;
      rsntr:contentType "image/jpeg" ] .

<urn:rsntr:room:01K1R00M0001XMPL000000000A> rsntr:name "gardening" .
```

Properties of `rsntr:Message`:

| Property | Range | Required | Meaning |
|---|---|---|---|
| `rsntr:id` | `xsd:string` | no (defaulted) | message ULID, minted by the author; the cross-hop dedup key. Absent: the receiving node adopts the wire request ULID |
| `rsntr:from` | `xsd:string` | no (assigned) | author endpoint id (64-hex ed25519). Never trusted from the payload on a gated hop (section 5.4) |
| `rsntr:room` | IRI | no | the room this message belongs to; absent means a direct message, scoped by the transport-proven peer pair |
| `rsntr:at` | `xsd:dateTime` | no (defaulted) | author-claimed send time. Absent: the receiving node's clock. Advisory ordering only; no cross-peer clock guarantee |
| `rsntr:body` | `xsd:string` | yes | the message text, at most 65536 bytes UTF-8 |
| `rsntr:attachment` | `rsntr:BlobRef` node | no | at most one in v1; the blank node carries `rsntr:hash` (`"blake3:"` + 64-hex), `rsntr:bytes` (exact size), `rsntr:name` (file name, display only, never a path), `rsntr:contentType` (MIME type) |

The frame carrying a Message MAY include further triples about resources the
message references, such as the room's `rsntr:name` above; receivers use
what they understand and ignore the rest (the open-world rule). Attachment
bytes never ride in the message; they are fetched out of band (section 6).

## 3. The chat modulation

`rsntr:Message` is not a request class. Requests parse strictly (envelope
doc section 5): an object typed with an rsntr: class the server does not
recognize as a request kind is a `protocol-error`, and widening the strict
set for chat would make every future application widen it again. The
Generic leniency of envelope section 10 applies to responses only, so it
cannot carry a request either. Instead the message rides where every
modulation payload rides: in `rsntr:signal`.

A chat send is an `rsntr:Execute` with `rsntr:mod "chat"` whose
`rsntr:signal` is a self-contained Turtle document containing exactly one
node typed `rsntr:Message`:

```turtle
[] a rsntr:Execute ;
   rsntr:id "01K1CH4T0001XMPL0000000001" ;
   rsntr:mod "chat" ;
   rsntr:signal """[] a rsntr:Message ;
   rsntr:id "01K1CH4T0001XMPL0000000001" ;
   rsntr:at "2026-07-29T12:00:00"^^xsd:dateTime ;
   rsntr:body "lunch at noon?" .""" .
```

This is the envelope's own design applied once more: the payload language of
a modulation is opaque to the envelope, and chat's payload language happens
to be Turtle. The precedent chain (SPIN's `sp:text`, R2RML's `rr:sqlQuery`)
carries one level deeper without any new wire mechanics, and it is exactly
what makes offline send free: `_outbox` stores `(mod, signal, params)`, so a
queued chat message is an ordinary outbox row with `mod = 'chat'` and the
Message Turtle in `signal`. Nothing in the outbox worker knows chat exists.

Rules:

- The signal parses with the implied prefix block (`rsntr:`, `xsd:`, `rdf:`,
  `rdfs:`), like every envelope frame; it MAY add `@prefix` lines of its
  own.
- The signal MUST contain exactly one node typed `rsntr:Message`; zero or
  several answer `rsntr:Error` with `rsntr:code "protocol-error"`.
- `rsntr:params` is unused; servers MUST ignore it.
- The producing client is responsible for Turtle string escaping of the
  body. Inside a `"""` long literal only the sequences `"""` and `\` need
  escaping; the CLI and web client serialize with a real Turtle writer and
  never hand-assemble the signal.
- Author identity, message id, and timestamp defaulting follow the table in
  section 2; the trust rules for `rsntr:from` are in section 5.4.
- A body over 65536 bytes, or a signal that would push the frame past the
  256 KiB budget, answers `rsntr:Error` with `rsntr:code "limit-exceeded"`.
  Anything bigger than a paragraph belongs in an attachment.

The response is an empty `rsntr:Result` header followed by `rsntr:Done`
(response readers treat a Done-first stream as a choreography error, and
the outbox worker would retry a lone Done forever):

```turtle
[] a rsntr:Result ;
   rsntr:id "01K1CH4T0001XMPL0000000001" .
```

```turtle
[] a rsntr:Done ;
   rsntr:id "01K1CH4T0001XMPL0000000001" ;
   rsntr:affectedRows 1 .
```

`rsntr:affectedRows 1` means the message was appended; `0` means the
recipient already had this message id (a resend, or a second delivery path)
and the send still succeeded. `rsntr:Denied` and `rsntr:Error` as ever.

Idempotency is two-level, and both levels are existing machinery:

- wire level: chat is a write modulation, so an applied request id is
  recorded in `_applied`; a retransmitted frame is answered from the
  recorded outcome without re-running the handler;
- message level: `chat_messages.id` is the primary key and the append is
  `INSERT OR IGNORE`, so the same message arriving under a different request
  id (room fan-out reaching a member twice) lands exactly once.

`chat` ships in the default `_rsntr.modulations` list, so a fresh node
advertises it in its hello `rsntr:mods` before the scaffold exists; the
handler creates the chat tables on first use (`CREATE TABLE IF NOT
EXISTS`). `rsntr chat init` still installs the projection points and
policy rows. An owner who removes `chat` from `_rsntr.modulations` gets
the fast `mod-unsupported` fail before the authenticator runs.

## 4. Delivery choreography: direct chat

alice sends bob a direct message. alice is admitted on bob's node (`_peers`
row; strangers knock first, connection doc section 4).

### 4.1 The send path

`rsntr chat send bob "lunch at noon?"` does two local writes in one
transaction and touches no network:

```sql
INSERT INTO chat_messages (id, scope, sender, at, body, outgoing)
VALUES ('01K1CH4T0001XMPL0000000001', '<bob 64-hex>', '<alice 64-hex>',
        '2026-07-29T12:00:00', 'lunch at noon?', 1);

INSERT INTO _outbox (request_id, peer, mod, signal)
VALUES ('01K1CH4T0001XMPL0000000001', 'bob', 'chat',
        '[] a rsntr:Message ; rsntr:id "01K1CH4T0001XMPL0000000001" ; ...');
```

The message ULID doubles as the `_outbox.request_id` for the direct hop, so
delivery state is a join away: `chat log` reads `_outbox.status`
(`queued | sent | done | denied | error | expired`) by message id. Online,
the outbox `update_hook` wakes the worker and the message ships within
milliseconds; offline, the row waits and ships on reconnection, with the
worker's backoff and expiry semantics unchanged. There is no separate
online path: send always means enqueue.

### 4.2 The receive path

The worker opens a stream to bob and sends the `rsntr:Execute` of section 3.
On bob's node:

1. peer gate: alice's proven endpoint id must be in `_peers` (stranger
   traffic never reaches a modulation);
2. mod gate: `chat` must be served, else `mod-unsupported`;
3. the handler parses the Message from the signal (malformed Turtle or a
   missing body is a `protocol-error`);
4. authenticator: the request is decided with action `chat` against the
   resource `chat:direct` (for a room message, the room IRI; section 5).
   The chat handler is node code writing its own tables, so there is no
   SQL footprint to collect; the action/resource pair is the policy
   surface, exactly as the media modulation gates by source name;
5. on allow: assign `rsntr:from` = the transport-proven sender, default any
   absent id/at, then idempotent append:

```sql
INSERT OR IGNORE INTO chat_messages (id, scope, sender, at, body,
                                     blob_hash, blob_bytes, blob_name, blob_type)
VALUES ('01K1CH4T0001XMPL0000000001', '<alice 64-hex>', '<alice 64-hex>',
        '2026-07-29T12:00:00', 'lunch at noon?', NULL, NULL, NULL, NULL);
```

6. record the outcome in `_applied` and `_audit`, answer `rsntr:Done`.

For a direct message the stored `scope` is the other end of the
conversation: the proven sender for incoming rows, the destination peer for
outgoing ones; the `outgoing` flag distinguishes the two.

### 4.3 Nudge and history

The append in 4.2 fires the sqlite `update_hook` the pipeline already owns;
`chat_messages` is the `resource` of the Sympathetic point
`<urn:rsntr:chat-inbox>`, so every entrained observer receives a
`rsntr:Vibration`. Vibrations are ticks, not a log (projection doc section
5): on a vibration the watcher re-reads the history Radiant from its cursor
(last seen `received_at` plus id) and renders whatever is new. A watcher
that was offline missed nothing, because the catch-up read is the same read.

Bob's own surfaces are the consumers of the inbox point: the web client, and
`rsntr chat watch`. A node MUST treat its own proven endpoint id as admitted,
and `chat init` writes the self-scoped policy rows that make the chat points
visible and entrainable for exactly that identity and nobody else (section
7.3). Note that iroh refuses to dial its own endpoint id (SelfConnect), so
over the iroh transport the self-dial path cannot actually be exercised;
`rsntr chat watch` instead mints an ephemeral identity, admits it for read +
entrain on `chat_messages` for the lifetime of the watch (rows tagged and
deleted on exit), and dials with that. The self-is-admitted rule stands for
local surfaces (the web interface) and future transports. Remote peers never
read your `chat_messages`; they only excite the send point.

### 4.4 Sequence summary

```
alice's node                                bob's node
  |                                             |
  | chat send: INSERT chat_messages (outgoing)  |
  |            INSERT _outbox  --wakes worker   |
  |                                             |
  |  --- rsntr:Execute mod "chat" -----------> peer gate -> parse Message
  |      signal = Message Turtle               -> policy (chat, chat:direct)
  |                                            -> INSERT OR IGNORE chat_messages
  |                                            -> _applied, _audit
  |  <-- rsntr:Done affectedRows 1 ----------  |
  | worker: _outbox.status = 'done'            | update_hook ->
  |                                            | <urn:rsntr:chat-inbox> vibrates
  |                                            | watchers re-read history Radiant
```

## 5. Rooms: creator-hosted

Q6 is decided: a room always has a host. The room lives on the host's node;
there is no replicated room state, no consensus, and no gossip fan-out in
v1 (iroh-gossip stays presence-only). This keeps the single-writer-per-db
consistency model intact: the host's `chat_messages` is the room's
authoritative transcript, and every member holds a best-effort local copy
fed by the host.

### 5.1 Creating a room and admitting members

On the host, `rsntr chat room create gardening` mints the room IRI and
writes local rows only:

```sql
INSERT INTO chat_rooms (room_id, name, host)
VALUES ('urn:rsntr:room:01K1R00M0001XMPL000000000A', 'gardening', '<host 64-hex>');

INSERT INTO chat_members (room_id, member)
VALUES ('urn:rsntr:room:01K1R00M0001XMPL000000000A', '<host 64-hex>');
```

`rsntr chat room add gardening carol` admits a member: carol must already be
an admitted peer (`_peers`), and membership is one roster row plus one
policy row, which is the entire authorization model:

```sql
INSERT INTO chat_members (room_id, member)
VALUES ('urn:rsntr:room:01K1R00M0001XMPL000000000A', '<carol 64-hex>');

INSERT INTO _policy (peer_or_group, table_name, action, effect, note)
VALUES ('<carol 64-hex>', 'urn:rsntr:room:01K1R00M0001XMPL000000000A',
        'chat', 'allow', 'chat room member: gardening');
```

Removing a member deletes both rows; from the next request on, the member's
sends answer `rsntr:Denied` and the host stops fanning out to them. Nothing
is retracted from copies already delivered.

### 5.2 Sending to a room

A member sends a room message as the ordinary chat Execute of section 3,
addressed to the host, with `rsntr:room` set. The host gates it with action
`chat` against the room IRI as the resource, which the policy tier resolves
most-specific-first exactly like any other resource; the membership row of
5.1 is what makes it pass. The host assigns `rsntr:from` = the proven
member endpoint, appends to its own `chat_messages` with `scope` = the room
IRI, and answers Done. When the sender is the host itself, the network hop
disappears: local append, then fan-out.

### 5.3 Fan-out is the host's outbox

After appending, the host enqueues one `_outbox` row per member other than
the author: same Message payload (original `rsntr:id`, original
`rsntr:from`, `rsntr:room`, plus the `<room> rsntr:name` triple so members
can label the room), fresh `request_id` per row, since request ids are
per-hop and `_outbox.request_id` is the primary key. Fan-out therefore
inherits everything the outbox already does: offline members receive the
message when they reconnect, retries are idempotent (`_applied` per hop,
`INSERT OR IGNORE` by message id at the member), expiry bounds the queue,
and delivery state per member is readable in the host's `_outbox`.

A member receiving a fan-out frame gates it like a direct message from the
host (action `chat`, resource `chat:direct`; it is the host speaking to
them), preserves the payload's `rsntr:from` as the author, and appends with
`scope` = the room IRI.

### 5.4 Who is trusted about what

`rsntr:from` in a payload is a claim; the transport-proven endpoint id is a
fact. The rules:

- direct send: the recipient MUST ignore any payload `rsntr:from` and store
  the proven sender;
- member to host: the host MUST ignore any payload `rsntr:from` and store
  the proven member;
- host to member (fan-out): the member stores the payload `rsntr:from`,
  trusting the host for authorship attribution. Trusting the host is the
  definition of a hosted room, and the honest statement of the model is
  that the host reads and could forge all room traffic; members chose the
  host when they joined. End-to-end guarantees inside rooms are explicitly
  out of scope for v1;
- room binding: a member accepts a fan-out for room R only from the peer
  its local `chat_rooms` row records as R's host. The first fan-out for an
  unknown room creates that row (host = the proven sender, name from the
  `rsntr:name` triple if present): trust-on-first-use, bounded by the fact
  that only admitted peers can deliver anything at all. `rsntr chat room
  join` (section 8) sets the binding explicitly instead.

### 5.5 Member bootstrap

Until a member has a `chat_rooms` row for R, it can receive from R (the row
is created on first fan-out) but cannot send to it (nothing resolves the
host). Either the first message creates the row, or the member runs
`rsntr chat room join <host> <room-iri>`, a purely local INSERT. A
structured invitation object is deferred; in v1 the host tells the member
the room IRI in a direct message or out of band.

## 6. File transfer

Attachment bytes move over iroh-blobs, never through the envelope. A file
message carries only the `rsntr:BlobRef` metadata (hash, exact size, name,
content type); the blob itself is content-addressed by its BLAKE3 hash and
fetched by whoever wants it, whenever the provider is reachable. This also
completes the general story for oversized query-result cells: the same
fetch path serves any `rsntr:BlobRef` the envelope ever emits.

### 6.1 Sending a file

`rsntr chat send bob "here" --file seedlings.jpg`:

1. the file is imported into the sender's local iroh-blobs store (BLAKE3
   hashed and pinned; the store lives beside `rsntr.db` in the node dir).
   While a node serves the directory, the serving process holds the blob
   store's lock, so the CLI hashes natively and delegates the import to
   the node via an owner-lane chat `Execute` whose single parameter is the
   source path (owner-channel-only; see
   [owner-channel.md](owner-channel.md) section 5.3);
2. the Message gets the attachment node of section 2, with the exact byte
   count and a best-effort `rsntr:contentType`;
3. the message is sent as usual. Frame size is unaffected: the metadata is
   a few hundred bytes regardless of blob size.

The sender's default per-file cap is 512 MiB (`_rsntr` key
`chat_blob_max_bytes`, an owner tunable, not protocol); the recipient
applies the same cap when deciding whether to fetch.

### 6.2 Fetching

The recipient sees the message immediately; the bytes come when asked:

```
rsntr fetch bob blake3:af1349b9...3262 -o seedlings.jpg
```

Fetch flow:

1. dial the provider's endpoint on the iroh-blobs ALPN; the node's iroh
   endpoint is shared, so the provider is the same identity that sent the
   message (the transport crate's `endpoint()` accessor exists precisely so
   sibling protocols attach to one endpoint);
2. request the hash; iroh-blobs streams the content with incremental BLAKE3
   verification (bao), so a corrupt or wrong blob fails during transfer,
   not after;
3. the fetcher additionally aborts if the stream exceeds the announced
   `rsntr:bytes` and treats a short blob as failure; on success the bytes
   go to `-o path`, or to stdout without `-o`;
4. the hash the user typed (or the client took from `chat_messages`) is the
   only integrity anchor needed: verification is by construction, not by
   comparing after download.

Provider policy in v1: a node answers blob requests only on connections
from endpoints present in `_peers` (the peer gate applied at the blobs
ALPN), and beyond that the 256-bit hash is the capability; a peer cannot
enumerate the store and can only fetch hashes it was handed. Per-blob
policy (`_policy` action `fetch`, resource = hash) is the designed
extension and is deferred.

Who to fetch from:

- direct message: the sender;
- room message: the host. On receiving a member's message with an
  attachment, the host SHOULD fetch the blob from the author and pin it
  before or alongside fan-out, becoming the room's provider, so members
  need reachability to the host only (the peer they already depend on).
  If the host could not fetch, members MAY fall back to the author
  directly, admission permitting.

### 6.3 When the provider is offline

The fetch fails with a dial timeout (`rsntr fetch` exits 3). Nothing is
lost: the BlobRef metadata is durable in `chat_messages`, and the fetch can
be retried any time the provider is back; the blob store is content
addressed, so a partially transferred blob resumes rather than restarts.
(Resume holds for store-backed nodes; `rsntr fetch` on a client without a
local blob store fetches through an in-memory target and restarts on
retry.)
The message-level experience is explicitly asynchronous by design, the same
way the message itself was: metadata now, bytes when both ends are up.
Automatic background fetch with retry, and small-attachment auto-fetch, are
client conveniences and are deferred.

Blob lifetime: the sender's import pins the blob; the pin is released when
the referencing `chat_messages` row is deleted (`rsntr chat` performs the
unpin; a plain SQL DELETE leaves an orphan pin until a future `rsntr blob
gc`, which is deferred).

## 7. The scaffold: rsntr chat init

`rsntr chat init <dir>` is idempotent and performs four things against the
node dir's database: create the user-space tables, insert the projection
rows, insert the policy rows, and add `chat` to `_rsntr.modulations`. It
also ensures `_outbox`/`_results` exist (the surfaces DDL, normally created
at first serve) so offline sends work before the node has ever served.

Since 2026-08-04, `rsntr init` runs this scaffold by default: a fresh node
chats out of the box. `rsntr chat init` remains the explicit form for
directories created before that default (and stays idempotent on both).

### 7.1 Tables

```sql
CREATE TABLE IF NOT EXISTS chat_messages (
  id          TEXT PRIMARY KEY,     -- message ULID (rsntr:id)
  scope       TEXT NOT NULL,        -- peer endpoint id (direct) or room IRI
  sender      TEXT NOT NULL,        -- author endpoint id (rsntr:from, assigned per sec 5.4)
  at          TEXT NOT NULL,        -- author-claimed time (rsntr:at)
  received_at TEXT NOT NULL DEFAULT (datetime('now')),
  body        TEXT NOT NULL,
  blob_hash   TEXT,                 -- 'blake3:' + 64-hex, when an attachment rides along
  blob_bytes  INTEGER,
  blob_name   TEXT,
  blob_type   TEXT,
  outgoing    INTEGER NOT NULL DEFAULT 0   -- 1 = authored on this node
);
CREATE INDEX IF NOT EXISTS chat_messages_scope
  ON chat_messages (scope, received_at);

CREATE TABLE IF NOT EXISTS chat_rooms (
  room_id    TEXT PRIMARY KEY,      -- 'urn:rsntr:room:' + ULID
  name       TEXT NOT NULL,         -- display only, not unique
  host       TEXT NOT NULL,         -- host endpoint id; own id when hosting
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS chat_members (
  room_id  TEXT NOT NULL REFERENCES chat_rooms(room_id),
  member   TEXT NOT NULL,           -- endpoint id
  added_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (room_id, member)
);
```

No leading underscore: these are the user's tables. The owner may query,
export, or extend them freely; the chat modulation only ever appends to
`chat_messages` and reads the roster.

### 7.2 Projection rows

Three points, straight into `_projection` (datatype IRIs written in full;
`ord` groups the chat points after whatever the owner already offers):

```sql
INSERT OR IGNORE INTO _projection
  (point_iri, kind, label, icon, ord, modulation, signal_template, fields, resource, note)
VALUES
  ('urn:rsntr:chat:send', 'excitable', 'send me a message', 'chat', 100, 'chat',
   '[] a rsntr:Message ; rsntr:body "{body}" .',
   '[{"name":"body","datatype":"http://www.w3.org/2001/XMLSchema#string",
      "required":true,"hint":"message text"}]',
   'chat:direct', 'rsntr chat init');

INSERT OR IGNORE INTO _projection
  (point_iri, kind, label, ord, resource, note)
VALUES
  ('urn:rsntr:chat-inbox', 'sympathetic', 'a message arrived', 101,
   'chat_messages', 'rsntr chat init');

INSERT OR IGNORE INTO _projection
  (point_iri, kind, label, ord, modulation, signal, params_order, fields, resource, note)
VALUES
  ('urn:rsntr:chat:history', 'radiant', 'chat history', 102, 'sql-sqlite',
   'SELECT id, scope, sender, at, body, blob_hash, blob_name
      FROM chat_messages
     WHERE (?1 IS NULL OR scope = ?1)
     ORDER BY received_at DESC, id DESC LIMIT ?2',
   '["scope","limit"]',
   '[{"name":"scope","datatype":"http://www.w3.org/2001/XMLSchema#string",
      "required":false,"hint":"peer endpoint or room IRI; empty = all"},
     {"name":"limit","datatype":"http://www.w3.org/2001/XMLSchema#integer",
      "required":false,"default":"50"}]',
   'chat_messages', 'rsntr chat init');
```

Notes:

- The send Excitable uses the template binding (projection doc section 4,
  form 2): the chat signal is Turtle, which has no positional placeholders,
  and the template lets a teletype or generic client compose a valid
  minimal Message from one prompted field. Absent `rsntr:id`/`rsntr:at`
  default server-side (section 2), which is what makes the template form
  workable at all. The scaffolded point sends direct messages only; room
  sending is a full-client affair.
- The inbox point's `resource` does double duty by design: it is both the
  policy gate and the table whose `update_hook` changes vibrate the point.
- The history Radiant is deliberately plain `sql-sqlite`: history is just a
  SELECT, and any client that can invoke a Radiant can read it. An empty
  optional `scope` binds `rsntr:null`, hence the `?1 IS NULL` arm.
- Visibility: the projection's excitable gate checks action `write` on the
  resource in the M4 code; M7 extends the visibility rule so a point whose
  `modulation` is `chat` is emitted when the caller holds an allow row for
  action `chat` on the point's resource. This keeps "the projection never
  lies" exact for chat without dummy policy rows (section 9).

### 7.3 Policy rows

```sql
-- any admitted peer may send me direct messages (the peer gate already
-- keeps strangers out; flip to deny, or per-peer rows, to tighten)
INSERT INTO _policy (peer_or_group, table_name, action, effect, note)
VALUES ('*', 'chat:direct', 'chat', 'allow', 'rsntr chat init: DMs open to admitted peers');

-- my own surfaces (self-dial, section 4.3) may read and entrain my chat;
-- <self> is this node's endpoint id, known to init from rsntr.key
INSERT INTO _policy (peer_or_group, table_name, action, effect, note)
VALUES ('<self>', 'chat_messages', 'read', 'allow', 'rsntr chat init: own history'),
       ('<self>', 'chat_messages', 'entrain', 'allow', 'rsntr chat init: own inbox');
```

`chat:direct` is a policy resource name, not a table; the colon keeps it
out of any plausible SQL table namespace. Room membership rows (section
5.1) are added by `room add`, one per member, resource = the room IRI. No
peer ever gets `read` on `chat_messages`, so remote history reads and
remote inbox entrainment are denied by default and the two local points do
not appear in any remote peer's projection.

## 8. CLI surface

All commands take the usual `--dir` (node directory) and `--json`; exit
codes are the CLI standard (0 ok, 1 error, 2 denied, 3 timeout). `<target>`
is a peer (petname or 64-hex endpoint id) or a room (name or room IRI);
rooms are checked first on ambiguity, and a room name that matches several
rooms requires the IRI.

```
rsntr chat init <dir>
    Scaffold: tables, projection points, policy rows, mods entry (sec 7).

rsntr chat send <target> <text> [--file <path>]
    Append locally, enqueue in _outbox (direct: to the peer; room: to the
    host, or local fan-out when this node hosts it). --file imports the
    blob and attaches a BlobRef. Prints the message id.

rsntr chat log <target> [--limit <n>]
    Local read of chat_messages for that scope (default limit 50), joined
    with _outbox.status for own outgoing messages (queued/sent/done/
    denied/error/expired). --json emits one object per message.

rsntr chat watch <target>
    Entrain <urn:rsntr:chat-inbox> on the local serving node (self-dial)
    and re-read history from the cursor on each vibration, printing new
    messages for that scope until interrupted. Falls back with a clear
    error when no node is serving.

rsntr chat room create <name>
    Mint urn:rsntr:room:<ULID>, insert chat_rooms + own member row; this
    node is the host. Prints the room IRI.

rsntr chat room add <room> <peer>
    Host only: roster row + membership policy row (sec 5.1). The peer must
    already be admitted.

rsntr chat room join <peer> <room-iri> [--name <n>]
    Member side: record <peer> as the host of <room-iri> locally (sec 5.5).

rsntr fetch <peer> <hash> [-o <path>]
    Fetch a blob by BLAKE3 hash from <peer> over iroh-blobs, verified
    streaming; bytes to <path> or stdout (sec 6.2). Not chat-specific: it
    fetches any rsntr:BlobRef the envelope handed you.
```

## 9. Implementation notes for M7

Code changes this design requires, all small and local:

- node pipeline: a `chat` handler arm (parse signal, gate on
  `chat`/resource, append, fan-out enqueue when hosting the room);
- outbox worker: `infer_kind` gains a `chat` arm mapping to
  `RequestKind::Execute` (today unknown mods default to Query);
- projection visibility: the `modulation = 'chat'` rule of section 7.2;
- transport/serve: attach the iroh-blobs protocol handler on the shared
  endpoint, accept-gated by `_peers`; a blob store beside `rsntr.db`;
- CLI: the `chat` and `fetch` subcommands; `chat init` writes the section 7
  scaffold.

No envelope codec change is needed: `rsntr:Message` never appears as a
frame-level object, and the new vocabulary terms live inside signal
payloads the codec does not interpret.

## 10. Deferred, explicitly

- Multiple attachments per message, and inline (base64) small attachments.
- Message edit and delete (tombstones), delivery/read receipts, typing
  indicators, reactions: all representable as future Message-referencing
  nouns; none in v1.
- Structured room invitations and a member-visible roster protocol (v1:
  first fan-out or `room join`; roster lives on the host).
- Gossip-based or hostless rooms; host migration. Rooms are creator-hosted,
  period (Q6).
- End-to-end confidentiality inside rooms (the host reads room traffic by
  design; transport encryption covers every hop).
- Per-blob fetch policy, blob garbage collection (`rsntr blob gc`),
  automatic/background attachment fetch, resumable-fetch UX.
- Per-member delivery dashboards on the host (the data already sits in
  `_outbox`; only presentation is deferred).

## 11. Vocabulary added by this doc

Additive to the envelope vocabulary, following the versioning rule (new
terms, never repurposed; unknown predicates ignored). These terms appear
only inside chat signal payloads, so no decoder changes:

| Term | Kind | Meaning |
|---|---|---|
| `rsntr:Message` | class | one chat message; the noun of this protocol |
| `rsntr:from` | property | author endpoint id (64-hex); assignment rules in sec 5.4 |
| `rsntr:room` | property | IRI of the room a message belongs to; absent = direct |
| `rsntr:body` | property | the message text |
| `rsntr:attachment` | property | links a Message to an `rsntr:BlobRef` node |

Reused unchanged: `rsntr:id`, `rsntr:at`, `rsntr:BlobRef`, `rsntr:hash`,
`rsntr:bytes`, `rsntr:contentType`, `rsntr:name` (a BlobRef's file name and
a room's display name, same "human label" sense as on Field). The policy
action `chat` and the resource name `chat:direct` are data conventions, not
vocabulary. The room IRI scheme `urn:rsntr:room:<ULID>` joins
`urn:rsntr:projection-changed` and `urn:rsntr:chat-inbox` in the
`urn:rsntr:` namespace of well-known and node-minted identifiers.
