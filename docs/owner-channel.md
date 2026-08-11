# The Owner Channel: Commanding Your Own Node

Status: accepted, 2026-07-29. Normative for v3. This document specifies how
the `rsntr` CLI commands a node: not as a bag of ad hoc database writes, but
as a generator of ordinary RDF envelope objects delivered over a privileged
local path. Normative companions:
[rdf-envelope-protocol.md](rdf-envelope-protocol.md) (envelope classes,
framing, error codes), [connection-protocol.md](connection-protocol.md)
(the gated path this one bypasses),
[chat-protocol.md](chat-protocol.md) (the chat modulation),
[projection-protocol.md](projection-protocol.md) (entrainment). Implementation
lands in one pass over `crates/node` and `crates/cli`.

This design resolves two recorded findings:

- CLI writes into a served directory do not vibrate Sympathetic points,
  because sqlite update hooks are per-connection and the CLI's writes happen
  on a second connection the serving process never sees.
- `rsntr chat send --file` deadlocks while a node is serving, because the
  CLI process tries to open the iroh-blobs redb store that the serving
  process holds under its single-writer lock.

## 1. One more surface, same envelopes

A command from the node owner is a normal `rsntr:Query` or `rsntr:Execute`
frame, in the same vocabulary and the same Turtle a remote peer would send.
Nothing about the wire format changes; there is no owner request class, no
owner property, no new vocabulary in this document at all. What changes is
the path the envelope takes through the node:

- Owner requests skip the `_peers` gate entirely. The owner is not a row in
  a table; ownership is established by the transport (section 3).
- Owner requests skip the authenticator chain entirely. No `_policy` lookup,
  no script tier, no escalation, nothing parks in `_inbox`. The decision is
  constitutionally `allow`, recorded as decided by `owner`.
- Owner requests are still footprint-collected: the collect-mode authorizer
  runs as always, and the footprint JSON lands in the audit row. The
  footprint is kept for the ledger, not for a decision.
- Owner requests are still written to `_audit`, with `decided_by = 'owner'`
  and `direction = 'local'` (section 6.1).
- Owner requests run without the remote bans: DDL, PRAGMA, and transaction
  control are permitted (section 2). The owner may do anything to their own
  database.
- Resource limits stay. A runaway query is a runaway query regardless of who
  wrote it; the `NodeConfig` ceilings (rows, bytes, wall clock, VDBE steps)
  clamp owner requests exactly as they clamp remote ones.

The peer identity on the owner channel is the node's own endpoint id (the
64-hex key from `rsntr.key`, mirrored in `_rsntr.endpoint_id`). That is what
appears in the audit rows and what the chat modulation records as the author
of an owner-appended message.

Prior art in the tree: the web interface's local surface
(`crates/web/src/local.rs`) already feeds envelopes into `Node::handle` with
the node's own EndpointId as the peer. That path is deliberately gated: the
browser holds a capability token, not the filesystem, so it goes through
policy (`ensure_owner_admitted` seeds its `_peers` and `_policy` rows). The
owner channel is the ungated sibling for callers who hold the filesystem
itself. The web surface stays gated and is unchanged by this document.

## 2. What the owner may do

The remote path's collect authorizer categorically bans a set of actions
before the chain ever runs. On the owner channel that set shrinks:

| Action | Remote path | Owner channel |
|---|---|---|
| DDL (CREATE/DROP/ALTER, tables, indexes, triggers, views) | banned | permitted |
| PRAGMA | allowlist only | permitted, any pragma |
| transaction control (BEGIN/COMMIT/ROLLBACK, SAVEPOINT) | banned | permitted (see below) |
| ATTACH / DETACH | banned | still banned |
| load_extension | banned | still banned |

ATTACH and load_extension stay banned even for the owner: they reach beyond
the served database file into the filesystem and the process, and the owner
already has the shell for that. They answer `rsntr:Denied`, which is the only
`rsntr:Denied` the owner channel can produce.

Transaction control is permitted in the authorizer sense: a statement or a
trigger body that uses SAVEPOINT/RELEASE is no longer refused at prepare.
The execution model is unchanged, though: each request still runs inside its
own implicit transaction on the shared serving connection, and the request
is the transaction boundary. A signal that is bare transaction control (a
lone `BEGIN`) prepares (it is never `rsntr:Denied`) but fails at step with
`engine-error`: the request already runs inside its own transaction and
sqlite refuses to nest. A `SAVEPOINT` runs and answers Done but does not
outlive its request; a `RELEASE` in a later request answers `engine-error`
(no such savepoint). Cross-request owner transactions are out of scope for
v1; holding the serving connection open under an owner `BEGIN` would starve
every remote request on the node.

`rsntr:rowLimit`, `rsntr:byteLimit`, and `rsntr:timeoutMs` on owner requests
are clamped by the same `NodeConfig` ceilings as remote ones.

## 3. Transports

The owner channel has two transports. Both deliver the same envelope objects
to the same dispatch entry; they differ in which process executes.

### 3.1 In-process

The CLI opens the node database itself (`resonator_node::open_node_db`, WAL,
busy timeout), builds the same pipeline `rsntr serve` builds minus the iroh
transport, and drives the owner dispatch entry directly with decoded
envelope objects. No socket, no frames: the envelope is constructed as a
typed object and handed to the dispatcher, so the 256 KiB frame budget does
not bind on this transport (relevant for `mod add`, section 5.2).

In-process works always: with no node serving it is the only path, and WAL
lets it run beside a serving node. Its known limitation is exactly recorded
finding 1: writes commit on the CLI's connection, so the update hooks in the
serving process do not fire. Sympathetic points do not vibrate, the outbox
worker is not woken (its poll cadence catches up), and `rsntr chat watch`
sees nothing until a re-read. In-process is correct but not live.

### 3.2 The control socket

When serving, the node listens on a unix domain socket at `<dir>/rsntr.sock`
next to `rsntr.db` and `rsntr.key`:

- Created at serve start, mode 0600, owned by the serving uid. A leftover
  socket path from a crashed process is detected at bind time (connect
  refused) and unlinked before rebinding; a live socket (the connect
  succeeds) is left alone, the node logs a warning and serves without a
  control socket (the blob store's single-writer lock already prevents two
  nodes serving one directory, so this only happens when a foreign process
  squats the path). Removed on clean shutdown.
- `sockaddr_un` caps socket paths at about 104 bytes on macOS. A node
  directory deep enough to exceed the cap is reached through a short-lived
  `/tmp/rsntr-<pid>-<n>` symlink alias for the bind/connect syscalls; the
  socket inode itself always lives in the node directory and the alias is
  removed immediately after.
- The socket speaks exactly the framed envelope protocol: u32-LE
  length-prefixed self-contained Turtle documents, implied prefix block,
  256 KiB budget, byte-identical to the iroh stream encoding.
- No hello. The owner already knows their node, and the socket's existence
  is the capability advertisement. The first (and only) frame a client
  writes is one request envelope: `rsntr:Query`, `rsntr:Execute`, or
  `rsntr:Entrain`. Anything else answers `rsntr:Error` code
  `protocol-error` and the node closes.
- The node streams the response frames back on the same connection
  (Result/Row/Done, Graph/Done, Help, Projection, Error, Denied), then
  closes. One request per connection; concurrent connections are fine and
  serialize on the db thread like all requests.
- For `rsntr:Entrain`, the response is the Done acknowledgment followed by
  Vibration frames for as long as the connection lives. The client damps by
  closing the connection; no in-band `rsntr:Damp` is needed on this
  transport (connection-scoped entrainment, closing is damping).
- The media modulation's raw byte feed is not served on the socket in v1; a
  media query answers `mod-unsupported` with a reason saying so. (No CLI
  command needs it: `rsntr watch` is remote-addressed.)

Possession of socket access is ownership. The filesystem permission (0600 in
a directory the owner controls) is the authority; there is no token, no
in-band credential, and nothing to leak. This is the same authority model as
`rsntr.key` itself: whoever can read the node directory already owns the
node.

Requests arriving on the socket are decoded and dispatched on the serving
node's live connection. This is the point of the socket: commits fire the
serving process's update hook, so Sympathetic points vibrate, `rsntr chat
watch` sees the append immediately, and the outbox worker wakes. This closes
recorded finding 1 for every command that uses the socket.

### 3.3 Selection

The CLI selects the transport per invocation:

- Try `<dir>/rsntr.sock` first. If it connects, use it.
- Otherwise fall back to in-process.
- `--socket` forces the socket and fails with a clear error when no node is
  serving. `--local` forces in-process even when a node serves (writes are
  then not live; the flag exists for debugging and for scripted bulk work
  that does not want to compete with live traffic).

Commands that only make sense live require the socket and fail without it
rather than silently watching nothing. (In v1 the one such command,
`chat watch`, still reaches the serving node by iroh self-dial with an
ephemeral identity it admits for its own lifetime; converting it to an
`rsntr:Entrain` over the socket is a recorded follow-up, not part of the
first implementation pass.)

### 3.4 Windows

No named-pipe implementation in v1. On Windows the in-process transport is
the baseline, as it is everywhere: every owner command works, minus the
liveness guarantee of section 3.2. A named-pipe transport with the same
framing is the designated extension point when Windows serving matters.

## 4. Dispatch semantics on the node

The owner dispatch entry mirrors `Node::handle` with the owner lane engaged:

- peer identity: the node's own endpoint id, taken from `_rsntr`, never from
  the client;
- no `_peers` lookup, no `Chain::decide`;
- sql-sqlite: collect-mode footprint with the reduced ban set (section 2),
  audit row (`allow`, `owner`, `local`), enforce-mode execution under
  limits, Result/Row/Done streamed;
- sparql: same, gated by nothing, executed with the same row caps;
- chat: the handler runs with the owner as the proven peer and appends a
  note-to-self (scope self, not outgoing); the outgoing local-append leg of
  `chat send` rides sql-sqlite instead (section 5.3);
- help and projection: served as on the remote path, except the projection
  is unfiltered: the owner sees every point (policy filtering exists to keep
  the projection honest toward peers; the owner is not a peer);
- entrain: no policy gate; any registered point may be entrained;
- registered wasm mods: reachable, with their internal `db_query` /
  `db_execute` statements still decided by the chain as the plugin
  contract requires (a mod does not inherit owner powers; the owner invoked
  it, but the plugin's own writes remain policy-bound exactly as on the
  remote path).

## 5. Command mapping

Every `rsntr` subcommand is either a node command (it generates envelopes on
the owner channel), a remote command (it already speaks envelopes over iroh
to a peer), or a native command (process-level, no node to command).

| Command | Kind | Envelope generated |
|---|---|---|
| `init` | native | none; creates the directory, key, and database |
| `serve` | native | none; is the node |
| `id`, `ticket` | native | none; key-file derivation only |
| `fetch` | native | none; iroh-blobs transfer |
| `peer add` | node | `Execute` mod `sql-sqlite`: upsert into `_peers` |
| `media add` | node | `Execute` mod `sql-sqlite`: upsert into `_media` |
| `media allow` | node | `Execute` mod `sql-sqlite`: INSERT into `_policy` |
| `media list` | node | `Query` mod `sql-sqlite`: SELECT from `_media` |
| `mod add` | node | `Execute` mod `sql-sqlite`: INSERT OR REPLACE into `_modulations`, wasm as a blob param |
| `mod enable` / `disable` | node | `Execute` mod `sql-sqlite`: UPDATE `_modulations` SET enabled |
| `mod rm` | node | `Execute` mod `sql-sqlite`: DELETE from `_modulations` |
| `mod list` | node | `Query` mod `sql-sqlite`: SELECT from `_modulations` |
| `mod describe` | mixed | wasm loaded natively to call `describe()`; the row read rides a `Query` mod `sql-sqlite` |
| `csv export` | node | `Query` mod `sql-sqlite`: SELECT over the table |
| `csv import` | node | `Execute` mod `sql-sqlite` per chunk: one multi-row INSERT (section 5.1); with `--create`, a preceding DDL `Execute` |
| `chat init` | node | owner `Execute`s: DDL for the chat tables, seed rows for `_projection`, `_policy`, `_rsntr` |
| `chat send` (local append leg) | node | `Execute` mod `sql-sqlite`: INSERT into `chat_messages` (section 5.3); the delivery enqueue is an `Execute` mod `sql-sqlite` INSERT into `_outbox`; with `--file` on the socket, an additional `Execute` mod `chat` delegates the blob import |
| `chat log` | node | `Query` mod `sql-sqlite` over `chat_messages` |
| `chat room create/add/join` | node | `Execute` mod `sql-sqlite` on the chat tables |
| `chat watch` | node | intended: `rsntr:Entrain` on the inbox point over the socket; v1 still self-dials (section 3.3) |
| `query`, `help`, `projection`, `entrain`, `watch` (to a peer) | remote | already envelopes over iroh, unchanged |

A remote-addressed command whose target resolves to this node's own endpoint
id should ride the owner channel instead of dialing (iroh self-dial is a
trick, the owner channel is the design). In v1 it still dials; the rewrite
onto the owner channel is a recorded follow-up.

Because owner commands are envelopes into the pipeline, the direct-db helper
functions in the CLI (`store::peer_add`, `store::media_add`, and friends)
are reduced to envelope builders; nothing in the CLI writes application or
registry state to the database directly anymore. The only direct database
access remaining is `init` (which creates the database) and the in-process
transport itself (which is the pipeline, not a bypass of it).

### 5.1 csv import chunking

One `Execute` per row is unacceptable: a hundred-thousand-row import must
not cost a hundred thousand round trips, audit rows, and `_applied` rows.
The CLI batches rows into one multi-row INSERT per chunk:

```sql
INSERT INTO t (a, b, c) VALUES (?1, ?2, ?3), (?4, ?5, ?6), ...
```

Chunk sizing obeys two caps: the framed envelope must stay inside the
256 KiB frame budget (the CLI targets about 64 KiB of parameter payload per
chunk), and rows x columns per chunk must stay under sqlite's bound
parameter ceiling. Each chunk is its own `Execute` with its own ULID, so a
retried import resumes idempotently chunk by chunk via `_applied`. With
`--create`, the table is created by a preceding DDL `Execute`, which the
owner channel permits.

### 5.2 mod add and large blobs

The wasm rides as an ordinary blob parameter (`xsd:base64Binary` inline).
Base64 inflates by a third, so wasm up to roughly 180 KiB fits a socket
frame. A larger wasm does not fit, and `rsntr:BlobRef` parameters are not
served in v1; the CLI then uses the in-process transport, where the
envelope is handed to the dispatcher as a decoded object and never framed,
so the budget does not bind. Nothing is lost by the fallback: mod
registration takes effect at the next `rsntr serve` start regardless, so
the socket's liveness guarantee buys nothing here. The same fallback covers
`mod describe`'s wasm row read.

### 5.3 chat send and the attachment

`rsntr chat send` becomes two owner envelopes: an `Execute` mod `sql-sqlite`
appending the outgoing row to `chat_messages` (the local append, whose
commit vibrates the inbox point when it arrives over the socket) and an
`Execute` mod `sql-sqlite` enqueueing the `_outbox` row for delivery. The
append is plain SQL rather than an `Execute` mod `chat` because the chat
handler's owner lane stores an incoming note-to-self (scope self,
outgoing 0); it cannot express an outgoing append to a target scope, and
the send's row shape (scope, outgoing, blob columns, delivery join on the
message ULID) is exactly what the chat tables are public user-space tables
for.

With `--file`, the CLI computes the BLAKE3 hash natively (pure hashing, no
blob store, no lock) and builds the `rsntr:Message` with its BlobRef. The
bytes must land in the serving process's blob store, and that store is
locked by the serving process; so on the socket a third envelope, an
`Execute` mod `chat` addressed to self, carries one positional parameter:
the source file path, as a text literal. The serving node imports that path
into its own blob store; because this `Execute` carries the real message id
and the append already happened, the chat modulation's message-id dedup
makes its append leg a no-op and only the import remains. This closes
recorded finding 2: the process that holds the redb lock is the process
that does the import.

The path parameter is an owner-channel-only affordance. Passing a local
filesystem path is legitimate exactly because socket access proves
filesystem access; a chat `Execute` carrying params on any gated surface is
refused as `protocol-error`. In-process (no node serving), the CLI imports
into the blob store directly as before; the lock is free because nothing
else holds it.

## 6. Semantics

### 6.1 Audit shape

Every owner request writes the same `_audit` row a remote request writes,
with three fixed values:

| Column | Owner value |
|---|---|
| `direction` | `local` (new value beside `in` and `out`; TEXT column, no schema change) |
| `peer` | the node's own endpoint id |
| `decided_by` | `owner` |
| `decision` | `allow` (or `deny` for the residual bans of section 2) |
| `signal` | the statement or signal text, verbatim |
| `footprint` | the collected footprint JSON, as on the remote path |
| `rows_out`, `bytes_out`, `duration_ms` | filled by execution, as on the remote path |

The ledger is unconditional: the owner channel is privileged, not invisible.
`SELECT * FROM _audit WHERE direction = 'local'` is the complete history of
what the owner did to their own node through the CLI.

### 6.2 Entrainment and vibration

Owner writes delivered over the control socket execute on the serving
connection, so the one sqlite update hook fires: entrainments on points
whose resource is a changed table receive Vibrations, `_projection` and
`_policy` changes signal the projection-changed point, and the composed
table observer wakes the outbox worker. This is a guarantee of the socket
transport, not of the owner channel in general: the in-process transport
explicitly does not carry it (section 3.1), which is why the CLI prefers
the socket whenever it connects.

### 6.3 Idempotency

Unchanged. An owner `Execute` records its ULID in `_applied` with its
outcome; a retransmit of a recorded id is answered from the stored outcome
instead of applied twice. Queries are not recorded. The chat modulation's
message-id dedup applies on top, as on any surface.

### 6.4 Error mapping

Identical to the remote path, minus what cannot happen:

- `auth-denied` cannot occur: there is no authenticator on this path.
- `rsntr:Denied` occurs only for the residual bans (ATTACH, DETACH,
  load_extension).
- `timeout`, `limit-exceeded`, `engine-error`, `protocol-error`,
  `mod-unsupported`, and `point-unknown` mean exactly what they mean on the
  remote path, and map to the same CLI exit codes (0 ok, 1 error, 2 denied,
  3 timeout).

## 7. Worked examples

What `rsntr peer add bob e00c...63d9 192.168.1.7:4433` emits, byte-for-byte
the shape of any remote Execute (implied prefix block, one frame):

```turtle
[] a rsntr:Execute ;
   rsntr:id "01K1PEERADD00XMPL000000001" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal """INSERT INTO _peers (endpoint_id, name, addrs, added_at) VALUES (?1, ?2, ?3, datetime('now')) ON CONFLICT (endpoint_id) DO UPDATE SET name = ?2, addrs = ?3""" ;
   rsntr:params ("e00c36119fb26262a12a5571f918966caf0b4cefe3c72ed046a1c1bbf1cd63d9"
                 "bob"
                 "[\"192.168.1.7:4433\"]") .
```

The node answers with the ordinary write choreography:

```turtle
[] a rsntr:Result ;
   rsntr:id "01K1PEERADD00XMPL000000001" ;
   rsntr:column () .

[] a rsntr:Done ;
   rsntr:id "01K1PEERADD00XMPL000000001" ;
   rsntr:rowCount 0 ;
   rsntr:affectedRows 1 ;
   rsntr:truncated false .
```

What `rsntr mod enable time` emits:

```turtle
[] a rsntr:Execute ;
   rsntr:id "01K1MDENABLE0XMPL000000001" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "UPDATE _modulations SET enabled = 1 WHERE name = ?1" ;
   rsntr:params ("time") .
```

Both leave an `_audit` row with `direction = 'local'`, `decided_by =
'owner'`, the footprint of the statement, and the affected-row count; the
peer-add Execute additionally records its ULID in `_applied`. Sent over the
socket while a node serves, the `_peers` commit vibrates any Sympathetic
point watching that table in the serving process.

## 8. Implementation notes

One pass over two crates:

- `crates/node`: an owner dispatch entry beside `Node::handle` (an owner
  lane flag through the sql/sparql screening) that skips `peer_known` and
  `Chain::decide`, collects the footprint with the reduced ban set, and
  audits with `direction = 'local'`, `decided_by = 'owner'`. The audit
  helpers grow a direction argument. The chat handler grows the
  owner-lane attachment-path import.
- `crates/cli`: a channel module that builds envelopes, picks socket vs
  in-process (section 3.3), and decodes response frames into the existing
  output layer; `serve.rs` binds the `UnixListener`, bridges socket bytes
  through the protocol codec into the owner dispatch entry, and unlinks the
  socket on shutdown; `store.rs`'s registry writers become envelope
  builders. The gated local-pipeline construction in `csvcmd.rs` (and its
  `ensure_owner_admitted` policy seeding) is superseded by the owner
  channel; the web surface keeps `ensure_owner_admitted` and stays gated.

No new wire vocabulary. This document adds no classes and no properties;
the owner channel is a path, and the whole point is that the language on it
is the one everything else already speaks.
