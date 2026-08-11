# The RDF Envelope: A Universal Protocol for the Resonator Network

Status: normative for v3. Date: 2026-07-29. This document supersedes the
v2 POC doc (research/rsntr-sqlite3/docs/rdf-envelope-protocol.md); the POC
remains the behavioral oracle, but this text is authoritative for v3.
Companions: [projection-protocol.md](projection-protocol.md) (projection
vocabulary and entrainment) and [browser-client-http2.md](browser-client-http2.md)
(browser transport).

Pinned facts carried from the research (2026-07): oxrdf 0.3.x, oxttl 0.2.x
(push parser), iroh-gossip (4 KiB default max message), Jelly-RDF spec 1.1.3
(Rust impl experimental, unreleased), CBOR-LD W3C WD (JSON-LD-only, no Rust).

## 1. Why a universal envelope

The first draft of the architecture put SQL text in a postcard-encoded Rust
struct on the wire. That binds the network to three accidents at once:
sqlite's five storage classes as the value model, sqlite's SQL as the only
payload anyone can parse, and Rust serde as the de facto schema language. A
postgres node could not even read a request, let alone serve one.

The network's ambition is symmetric heterogeneous peers: a postgres, duckdb,
or mongo node joins, serves queries through its own authenticator, and issues
queries of its own. That requires splitting two things the first draft fused:

- the wire language, which every peer must speak, and must therefore be
  engine-neutral, and
- the query payload, which is inherently engine-specific and should stay that
  way (translating every modulation into one universal query language is the
  graveyard of data-integration projects).

The wire language is RDF. Every message on the network is an RDF object
serialized as Turtle; queries are RDF objects that carry their
modulation-specific text as a literal. Clients speak the payloads they
understand and render what they can.

## 2. Turtles all the way down, again

This reverses the first draft's "deliberate divergence" from Resonator's RDF
protocol, and the reversal should be owned: the relational network and the
RDF network were briefly two designs, and they are now one wire format with
two payload families.

The precedent was already in the house:

- Carrier converts every Tox network event into a one-line Turtle statement
  and accepts Turtle commands back; "no command vocabulary, only RDF objects
  flowing in and out" (v2 wiki, rdf-protocol).
- SPIN represents SPARQL queries as RDF objects: `[] a sp:Select ; sp:text
  "SELECT ..."`. Antenna dispatches on `rdf:type` and executes. The envelope
  below is SPIN with the modulation generalized: `rsntr:Query ; rsntr:signal
  "SELECT ..." ; rsntr:mod "sql-sqlite"` is the same shape carrying SQL
  instead of SPARQL.
- Even SQL-text-inside-RDF-literals has standards precedent: R2RML's
  `rr:sqlQuery` property holds "a valid SQL query" as a literal in Turtle
  mapping documents (W3C Rec 2012). What has no precedent anywhere (verified
  mid-2026) is using that shape as a p2p request/response protocol; that is
  the novel part, not the wrapping.

What stays deferred: SQL-to-SPARQL translation. The envelope carries
modulations; it does not translate between them.

## 3. The layered model

```
+---------------------------------------------------------------+
|  surfaces        vtabs, outbox/inbox, extension  (per node)   |
+---------------------------------------------------------------+
|  payload         sql-sqlite | sparql | sql-postgres | ...     |
|  modulations     opaque signals + params, run if demodulated  |
+---------------------------------------------------------------+
|  envelope        RDF objects in the rsntr: vocabulary         |
|  (this doc)      queries, results, knocks, presence, hellos   |
+---------------------------------------------------------------+
|  transport       iroh QUIC: streams, keys, relays, gossip     |
+---------------------------------------------------------------+
```

The rule that makes heterogeneity work: a node MUST speak the envelope; it
MAY execute any subset of modulations (including none: a pure client).
Modulation tags are data, so an envelope parser never breaks on a modulation
it has never heard of; it just cannot serve it.

One modulation is mandatory: every node MUST serve `help`. Help is the
network's built-in self-documentation, so a node is never a black box: a
human or an AI that reaches any node can ask it, in the same envelope
everything else uses, how to use it. Because help is how a stranger learns to
knock and what a node offers, help is answerable before admission (a stranger
may ask for help and knock; nothing else). Section 4.1 defines the help
modulation and the plain-text hint that points hand-driven callers at it.
Help's machine-readable sibling is the recommended `projection` modulation:
the node's capability surface as RDF data a client can render as menus,
defined in [projection-protocol.md](projection-protocol.md).

A node's modulation set is builtins plus registered extensions. Extension
modulations (wasm plugins hosted by the mods crate) are registered in the
reserved `_modulations` table; the hello's `rsntr:mods` list is the builtins
plus the enabled rows of `_modulations`. On the wire nothing distinguishes a
builtin from a plugin-served modulation.

## 4. The rsntr: vocabulary

Namespace, following the wiki convention
(`http://resonator.network/v3/<name>#`):

```turtle
@prefix rsntr: <http://resonator.network/v3/rsntr#> .
```

The implied prefix block for all envelope parsing is `rsntr:`, `xsd:`,
`rdf:`, `rdfs:` (the wiki's standard block plus `rsntr:`). It is prepended by
convention on parse and never transmitted.

### Classes

| Class | Direction | Meaning |
|---|---|---|
| `rsntr:Query` | request | read statement in some modulation |
| `rsntr:Execute` | request | write statement in some modulation |
| `rsntr:Result` | response | header: column names/types, in order |
| `rsntr:Row` | response | one result row |
| `rsntr:Graph` | response | one chunk of a result graph (section 4.2) |
| `rsntr:Done` | response | trailer: counts, truncation flag |
| `rsntr:Denied` | response | authenticator said no |
| `rsntr:Error` | response | execution or protocol failure |
| `rsntr:Hello` | control | capability advertisement, both directions |
| `rsntr:Knock` | control | stranger's introduction |
| `rsntr:Presence` | gossip | liveness beacon |
| `rsntr:Decision` | control/audit | an authorization outcome as data |
| `rsntr:Help` | response | human-readable usage guidance in plain text |

The projection companion ([projection-protocol.md](projection-protocol.md))
defines further kinds on top of these: the projection vocabulary itself
(`rsntr:Projection` and its resonance points) and `rsntr:Entrain` /
`rsntr:Vibration` / `rsntr:Damp`, connection-scoped subscription to
Sympathetic points. The media modulation adds `rsntr:Media`; the
audio-duplex modulation adds `rsntr:AudioDuplex` (section 4.3). An rsntr:
class a decoder does not know is not automatically an error: on the
response path it decodes to a Generic object (section 10).

### Properties

| Property | Range | On | Meaning |
|---|---|---|---|
| `rsntr:id` | `xsd:string` | requests, responses | ULID; idempotency key; correlates response frames |
| `rsntr:mod` | `xsd:string` | Query/Execute, Hello | payload modulation tag, e.g. `"sql-sqlite"` |
| `rsntr:signal` | `xsd:string` | Query/Execute | the statement, verbatim (mirrors `sp:text`) |
| `rsntr:params` | rdf:List | Query/Execute | ordered bound parameters |
| `rsntr:database` | `xsd:string` | requests | reserved for future multi-database endpoints |
| `rsntr:rowLimit`, `rsntr:byteLimit`, `rsntr:timeoutMs` | `xsd:integer` | requests | client-side caps, clamped by server |
| `rsntr:column` | rdf:List | Result | ordered column names (strings) |
| `rsntr:declType` | rdf:List | Result | ordered declared types, engine-native strings |
| `rsntr:seq` | `xsd:integer` | Row, Graph | row or chunk ordinal within the result |
| `rsntr:rowCount`, `rsntr:affectedRows`, `rsntr:lastInsertRowid` | `xsd:integer` | Done | outcome counts |
| `rsntr:truncated` | `xsd:boolean` | Done | limits hit before exhaustion |
| `rsntr:code` | `xsd:string` | Error | `auth-denied`, `timeout`, `limit-exceeded`, `mod-unsupported`, `engine-error`, `protocol-error`, `point-unknown` |
| `rsntr:reason` | `xsd:string` | Denied, Error, Decision | human-readable explanation |
| `rsntr:ver` | `xsd:string` | Hello | minor version within the ALPN major |
| `rsntr:enc` | `xsd:string` (repeated) | Hello | `"turtle"` (mandatory), `"compact-postcard"`, future `"jelly"` |
| `rsntr:mods` | `xsd:string` (repeated) | Hello | modulations this node works: executes for peers and can issue itself (always includes `"help"`); an engine-backed tag MAY suffix its engine version (`sql-sqlite-3.46.0`, sec 8) - the builtin v3 node advertises plain tags (`sql-sqlite`) |
| `rsntr:hint` | `xsd:string` | Hello | one-line plain-text pointer to help, for a human or AI reading the hello |
| `rsntr:topic` | `xsd:string` (repeated) | Help | names of further help topics that can be requested |
| `rsntr:at` | `xsd:dateTime` | Presence, Decision | timestamp |
| `rsntr:status` | `xsd:string` | Presence | optional freetext status |
| `rsntr:endpoint` | `xsd:string` | Presence | beacon author endpoint id (64-hex ed25519); the receiver treats the gossip-proven sender as authoritative and uses this only as a cross-check |
| `rsntr:message` | `xsd:string` | Knock | the introduction text |
| `rsntr:decision` | `xsd:string` | Decision | `allow`, `allow-narrowed`, `deny` |
| `rsntr:decidedBy` | `xsd:string` | Decision | `policy`, `script`, `ai`, `human`, `cache` |

### Values and typed literals

Parameters and result cells map engine values to RDF literals:

- integer -> `xsd:integer`, float -> `xsd:double` (canonical lexical form;
  round-tripping notes in section 12), text -> plain literal, blob ->
  `xsd:base64Binary` inline.
- Large blobs go out of band: a cell or param may be an `rsntr:BlobRef` node,
  `[] a rsntr:BlobRef ; rsntr:hash "blake3:..." ; rsntr:bytes 104857600`,
  fetched via iroh-blobs.
- NULL: RDF has no null, so the envelope designates one. In parameter lists,
  the individual `rsntr:null` holds a positional slot: `rsntr:params ("alice"
  rsntr:null 42)`. In result rows, NULL is the absence of the column's
  predicate; the `rsntr:Result` header declares all columns, so absence is
  unambiguous. This asymmetry (marker in ordered lists, omission in property
  sets) is a designed wart; both alternatives in each position are worse.

## 4.1 The help modulation

Help is a modulation served by every node. It reuses the ordinary request
choreography: a caller sends an `rsntr:Query` with `rsntr:mod "help"`, and
`rsntr:signal` naming a topic (an empty signal means the overview). The
response is a single `rsntr:Help` object, not the Result/Row/Done sequence,
because help is prose, not a table.

Ask for the overview:

```turtle
[] a rsntr:Query ; rsntr:id "01J9V8..." ; rsntr:mod "help" ; rsntr:signal "" .
```

Get back plain, human-and-AI-readable text, plus the names of drill-down
topics so the guidance is itself navigable:

```turtle
[] a rsntr:Help ;
   rsntr:id "01J9V8..." ;
   rsntr:topic "modulations", "tables", "knock", "examples" ;
   rsntr:signal """This is a resonator node (rsntr, envelope 0.1).
You talk to me in RDF objects over QUIC; I serve the modulations: sql-sqlite, sparql, help.
Publicly readable now: notes(title, body, mtime).
To run a read:
  [] a rsntr:Query ; rsntr:mod \"sql-sqlite\" ; rsntr:signal \"SELECT title FROM notes\" .
Not admitted yet? Introduce yourself and I may let you in:
  [] a rsntr:Knock ; rsntr:message \"who you are and what you want\" .
Ask for more: send a help query with signal one of: modulations, tables, knock, examples.""" .
```

Content is part owner-authored and part generated. The owner writes an
overview in `_rsntr` (key `help_text`); the node augments it with facts it
already knows: the modulations from its own hello, the tables a stranger or
this peer may currently read (derived from `_policy`), and the knock
instructions. So help never lies about access, because it is computed from
the same policy that enforces it.

Help passes through the authenticator like any request, but the default
posture serves help to everyone, strangers included: it exposes only what the
owner chose to publish as guidance, never data. A node MAY restrict help by
policy, but MUST answer a help request from an admitted peer.

### The plain-text hint

The paragraph above assumes the caller already speaks the envelope. The whole
point of "essential" help is the caller who does not: a person on a raw
connection, or an AI handed a socket with no schema. For them the envelope
itself is the obstacle, so the hint must arrive in plain text.

Two mandatory behaviors give it to them:

- Every `rsntr:Hello` carries `rsntr:hint`, a one-line plain-text pointer
  (`"resonator node; send an rsntr:Query with modulation 'help' for usage, or
  type HELP"`). An AI that can parse the hello RDF reads this and knows its
  next move without being told the protocol out of band.
- If the first bytes on a connection are not a valid length-prefixed envelope
  frame (a human typed something, an AI sent bare text, or the input is a
  lone word like `help`, `HELP`, or `?`), the node MUST answer with a
  plain-text banner, newline-terminated and unframed, then keep the
  connection open for a proper hello. The banner is the same guidance the
  hint points at, spelled out:

```
resonator node (rsntr, envelope 0.1). I speak RDF objects over QUIC.
Hand-driven? Ask me for help in one line:
  [] a rsntr:Query ; rsntr:mod "help" .
and I will reply with usage. Not admitted? Knock:
  [] a rsntr:Knock ; rsntr:message "who you are and what you want" .
```

This is the protocol teaching its own use: a stranger, human or machine, that
connects with no prior knowledge is met with instructions rather than a parse
error. The plain-text banner is a courtesy affordance outside the framed
protocol; once the caller sends a real frame, normal envelope rules resume.

## 4.2 The sparql modulation

SPARQL is a builtin modulation in v3, served by the node's own
SPARQL-over-SQLite engine. Tag `"sparql"`; `rsntr:signal` is the SPARQL text,
verbatim. `rsntr:params` is unused (SPARQL has no positional parameters);
implementations MUST ignore it. SPARQL Update statements (INSERT DATA, DELETE
DATA, DELETE WHERE) ride `rsntr:Execute` with the same tag and answer with a
lone `rsntr:Done` carrying `rsntr:affectedRows`.

The response shape depends on the query form:

- SELECT answers as the ordinary Result/Row/Done sequence (section 6). The
  header's `rsntr:column` list carries the projected variable names in order;
  `rsntr:declType` is omitted (RDF terms are self-typed). Row cells are
  `rsntr:col_<var>` with the bound term: literals stay typed literals, IRIs
  stay IRIs, and an unbound variable is the absence of its predicate.
- ASK answers as a Result header with the single column `"ask"`, one Row with
  `rsntr:col_ask true` or `false`, then Done.
- CONSTRUCT and DESCRIBE answer as a stream of `rsntr:Graph` frames,
  terminated by Done. This makes RDF the default response format: the
  envelope is already Turtle, so a graph result needs no row encoding at all.

A Graph frame is one Turtle document containing exactly one node typed
`rsntr:Graph`, carrying `rsntr:id` (the request ULID) and `rsntr:seq` (chunk
ordinal, from 0). Every remaining triple in the frame, i.e. every triple not
on the header node, is payload: a chunk of the result graph. A frame MAY
declare additional `@prefix` lines for its payload; the implied block still
applies. The result graph is the union of the payload triples of all Graph
frames; `rsntr:seq` orders chunks for streaming consumers, but the union is
order-independent. Blank nodes in the payload are frame-scoped (section 7):
the engine MUST NOT split triples sharing a blank node across frames.

A CONSTRUCT result, on the wire:

```turtle
# frame 1: graph chunk 0
@prefix ex: <http://example.org/notes#> .

[] a rsntr:Graph ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:seq 0 .

ex:groceries a ex:Note ;
   ex:title "groceries" ;
   ex:mtime "2026-07-04T10:11:12" .

# frame 2: trailer
[] a rsntr:Done ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:rowCount 3 ;
   rsntr:truncated false .
```

For graph results `rsntr:rowCount` in the trailer counts payload triples;
`rsntr:rowLimit` on the request caps them and `rsntr:truncated` reports the
clip. A payload chunk that would exceed the frame budget is split into
further Graph frames at triple boundaries.

## 4.3 The audio-duplex modulation

The media modulation streams bytes one way; audio-duplex streams them both
ways on the same request stream, which is what a voice conversation needs.
A duplex source is an ordinary `_media` row whose `accepts` column names
the media type its command reads on stdin; the command's stdout (if any)
is the downstream feed, exactly as media. Policy action: `audio-duplex`
on the source name, separate from `media` (talking into a place is more
privileged than watching it).

The open:

```turtle
[] a rsntr:Query ;
   rsntr:id "01K1D00RTX0000000000000001" ;
   rsntr:mod "audio-duplex" ;
   rsntr:signal "door-talk" .
```

The go-ahead, after which the stream stops being frames in both
directions:

```turtle
[] a rsntr:AudioDuplex ;
   rsntr:id "01K1D00RTX0000000000000001" ;
   rsntr:accepts "audio/L16;rate=8000;channels=1" .
```

`rsntr:contentType` is present only when the source emits downstream
bytes; a pure talk sink (the door panel speaker) omits it. `rsntr:accepts`
is mandatory. The `rsntr:id` matters to web clients: it names the
upstream endpoint the browser POSTs its audio to (web-api.md).

After the header: the caller writes bytes in the `accepts` format on its
half of the stream and reads the source's bytes on the other; closing the
caller's write half (the wire Fin) tells the source's stdin EOF while the
downstream may keep flowing; either side closing everything ends the
session and the node kills the source's process group, exactly as media.
Frame budget rules stop at the header. A source whose `accepts` is NULL
answers `engine-error`: it is not a duplex source.

## 5. Requests

A query, as it crosses the wire (one frame, prefix block implied):

```turtle
[] a rsntr:Query ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "SELECT name, mtime FROM notes WHERE mtime > ?" ;
   rsntr:params ("2026-07-01") ;
   rsntr:rowLimit 1000 .
```

A write:

```turtle
[] a rsntr:Execute ;
   rsntr:id "01J9V3ZS3FQZJ8B1N5D7F9H1K3" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "INSERT INTO notes (title, body) VALUES (?, ?)" ;
   rsntr:params ("hello from bob" "...") .
```

Design notes:

- Params are an RDF list: ordered, compact in Turtle, and short enough that
  list-streaming pain never materializes. Named parameters ride on the
  modulation's own syntax inside `rsntr:signal`; the envelope only guarantees
  positional binding.
- Schema discovery is not a request kind. It is a normal `rsntr:Query`
  against the modulation's catalog (`sqlite_schema`, `pg_catalog`), with the
  serving side's policy treating catalog reads as their own resource.
- There is deliberately no `rsntr:Batch` in this major; when a later version
  adds it, each contained statement gets its own authorization decision
  (settled: N per-statement decisions).
- Requests parse strictly: an object typed with an rsntr: class the server
  does not recognize as a request kind is a `protocol-error`. The Generic
  leniency of section 10 applies to responses only.

## 6. Results

The response stream carries a header object, row objects in batches, and a
trailer, each frame a self-contained Turtle document:

```turtle
# frame 1: header
[] a rsntr:Result ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:column ("name" "mtime") ;
   rsntr:declType ("TEXT" "TEXT") .

# frame 2..n: row batches (many Row objects per frame, up to the frame budget)
[] a rsntr:Row ; rsntr:seq 0 ; rsntr:col_name "groceries" ; rsntr:col_mtime "2026-07-04T10:11:12" .
[] a rsntr:Row ; rsntr:seq 1 ; rsntr:col_name "reading list" ; rsntr:col_mtime "2026-07-19T08:00:00" .

# final frame: trailer
[] a rsntr:Done ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:rowCount 2 ;
   rsntr:truncated false .
```

### The row-mapping decision

Three candidate encodings were considered:

| | fidelity | size | streamable | RDF-queryable |
|---|---|---|---|---|
| (a) predicate-per-column Row objects | full (typed literals) | verbose | yes, row = object | yes |
| (b) RDF list per row | full | medium | painful (list cells) | awkward |
| (c) opaque literal rows (JSON/CSV string) | JSON-typed | compact | yes | no |

(a) is canonical. It is the existing in-house shape (`antenna:Result ;
antenna:var_s "..."` for SPARQL rows), each row is one self-contained object
(streams naturally, one frame boundary can never split a cell), and a result
set dropped into any RDF store is immediately SPARQL-queryable, which makes
audit logs and debugging pleasant. Its verbosity is real and is what the
compact mode (section 8) exists for; (c) is effectively what compact mode
does, done honestly in binary rather than as stringly-typed RDF.

Column-name predicates are minted as `rsntr:col_<name>` with non-IRI-safe
characters percent-encoded; the header's `rsntr:column` list carries the true
names and the order (predicates in RDF are unordered; `rsntr:seq` orders
rows, the header orders columns).

Prior art note: the DAWG result-set vocabulary (`rs:ResultSet`,
`rs:ResultSolution`, `rs:binding`, `rs:variable`/`rs:value`, `rs:index`)
solved this same problem for SPARQL test suites with an extra indirection:
one binding node per cell. That is maximally general (variable names stay
data, not predicates) and roughly doubles the triples per cell; the envelope
borrows its `rs:index` idea as `rsntr:seq` and skips the binding indirection.
If cell-level generality ever matters (modulations with dynamic column sets),
the rs: shape is the known fallback.

## 7. Framing and encodings

A frame is a length-prefixed (u32 LE) UTF-8 chunk containing one complete
Turtle document: one envelope object, or for row batches, a set of Row
objects, or a Graph header plus its payload triples. Rules:

- The standard prefix block is implied, never transmitted (Carrier
  convention). A frame MAY carry additional `@prefix` declarations of its
  own.
- A frame parses standalone: no blank-node references across frames, no
  dangling lists. Frame budget ~256 KiB; a row larger than the budget forces
  `rsntr:BlobRef` or errors.
- Parsing uses oxttl's push-based low-level API (`TurtleParser::low_level()`:
  feed byte chunks, pull triples), so a frame decodes incrementally without
  buffering the stream; serialization is likewise incremental, object at a
  time.

Encodings compared for the wire:

- Turtle (chosen baseline): readable, grep-able, matches Carrier, one parser
  (oxttl) shared with everything else in the Resonator world. Every peer MUST
  accept it.
- N-Triples/N-Quads: line-oriented and prefix-free, marginally simpler to
  frame, but 2-4x the bytes of prefixed Turtle and no readability win over
  it. Not used.
- Jelly-RDF: protobuf-based streaming RDF with exactly this framing model
  (varint-delimited frames, recommended under 1 MB); the credible future
  binary RDF encoding. Its Rust implementation (jelly_rs) is experimental and
  unreleased as of mid-2026, so it is an `rsntr:enc "jelly"` candidate for
  later, not a v3 dependency.
- CBOR-LD: W3C WD but JSON-LD-centric semantic compression with no Rust
  implementation; wrong fit. HDT: archival/query format, not a wire format.
  Both dismissed.
- compact-postcard: not an RDF serialization at all, but a negotiated binary
  encoding of the same envelope objects for sqlite-to-sqlite pairs
  (section 8).

## 8. Hello and negotiation

The first object each side sends on a new connection (on the first opened bi
stream, before any request) is its hello:

```turtle
[] a rsntr:Hello ;
   rsntr:ver "0.1" ;
   rsntr:enc "turtle", "compact-postcard" ;
   rsntr:mods "sql-sqlite", "sparql", "help" .
```

Rules:

- ALPN (`resonator/rdf/0`) carries only the envelope major version.
  Modulations and encodings live in the hello, not the ALPN, or the version
  cross-product explodes.
- Mod tags are plain strings; an engine-backed tag MAY suffix its engine
  version into the tag itself (`sql-sqlite-3.46.0`). A request names the base
  tag (`sql-sqlite`); matching strips a trailing `-<digit...>` suffix from
  the advertised tag, and a request that names a versioned tag exactly is a
  version pin. The builtin v3 node advertises unversioned tags
  (`sql-sqlite`); the versioned form is kept for adapters that must expose
  engine versions. There is no separate engine property: the engine is an
  attribute of the modulation, not the node.
- `turtle` is mandatory; a connection where both hellos list
  `compact-postcard` MAY switch subsequent frames to it (the switch
  mechanics, in-band flag vs per-stream marker, are an open question; that
  RDF-Turtle is the default is not).
- A request in a modulation the server does not serve fails fast with
  `rsntr:Error ; rsntr:code "mod-unsupported"`, before the authenticator
  runs.
- The hello must agree with the node's `_rsntr` table; the table is the
  queryable local mirror of what the node claims on the wire. For extension
  modulations the source of truth is the `_modulations` registry table
  (section 3): the hello advertises exactly the enabled rows plus the
  builtins.

## 9. Everything is an envelope object

The payoff of paying RDF's verbosity tax: the non-query traffic needs no
second protocol.

A stranger knocks (the only thing an unadmitted key may send):

```turtle
[] a rsntr:Knock ;
   rsntr:id "01J9V6QK3M8ZT0R4Y2W6B8N1D5" ;
   rsntr:message "Hi, this is carol, met you at the workshop. Requesting recipe access." .
```

The knock routes through the policy engine like any request; admission is an
INSERT into `_peers`, performed by policy, script, or the owner answering via
`_inbox`. The `rsntr:id` correlates the knock with its `_inbox` row and the
answering `rsntr:Decision`; the server honors a well-formed client ULID and
mints one otherwise.

Presence, gossiped among admitted peers (iroh-gossip; the 4 KiB default
message cap is two orders of magnitude above a beacon):

```turtle
[] a rsntr:Presence ;
   rsntr:at "2026-07-23T09:15:00"^^xsd:dateTime ;
   rsntr:status "around" ;
   rsntr:endpoint "e00c36119fb26262a12a5571f918966caf0b4cefe3c72ed046a1c1bbf1cd63d9" .
```

Decisions, as first-class data (returned to the requester inside
`rsntr:Denied`, stored in `_audit`, shown in `_inbox`):

```turtle
[] a rsntr:Decision ;
   rsntr:id "01J9V3Z8K7Q2M4X6P8R0T2V4W6" ;
   rsntr:decision "deny" ;
   rsntr:decidedBy "script" ;
   rsntr:reason "bulk read exceeds hourly row budget for this peer" .
```

Query, knock, presence, decision, and error are one language, dispatchable by
`rdf:type` exactly as Antenna already dispatches, storable in any RDF store,
and renderable by any client that knows the vocabulary. This is the section
that justifies RDF: none of the alternatives in section 13 can say the same.

## 10. Generic frames and forward compatibility

The vocabulary will outgrow any single decoder: plugins mint new response
kinds, and two nodes on the same envelope major will routinely disagree on
minor version. The envelope absorbs this with one asymmetric rule:

- On the response path (frames answering a request this peer issued, and
  entrainment vibrations), an object typed with an rsntr: class the decoder
  does not recognize MUST decode to an inert Generic object: the class IRI
  plus its property/value bag, preserved verbatim. Decoding MUST NOT fail;
  the consumer may render it generically (the default RDF renderer does),
  log it, store it, or ignore it.
- On the request path, parsing stays strict: a server receiving an object
  whose rsntr: type it does not recognize as a request kind answers
  `rsntr:Error ; rsntr:code "protocol-error"`. A server must never guess at
  the semantics of a request; a client can always afford to carry an opaque
  response object.

This extends the RDF norm one level up. Unknown predicates on known classes
were already ignored (section 15); Generic makes unknown classes on the
response path equally survivable. Together they give the forward-compat
guarantee that makes version skew and plugins safe:

- A plugin-served modulation may answer with frame kinds the caller's
  protocol build has never seen; the caller's client still completes the
  request, correlating by `rsntr:id` and treating unknown frames as opaque
  payload for the modulation-aware layer above it.
- A newer node talking to an older one never has to probe for support before
  responding richly; the older side degrades to Generic instead of erroring
  mid-stream.
- `rsntr:Done`, `rsntr:Error`, and `rsntr:Denied` keep their meaning across
  skew: a stream still terminates on a frame the oldest decoder understands,
  so Generic frames can never wedge a request.

Generic is a decoder posture, not a wire feature: nothing on the wire marks a
frame as generic, and re-serializing a Generic object emits exactly the
triples that arrived.

## 11. Adapter anatomy: how a postgres node joins

An adapter is three shared parts and three engine parts:

Shared (crates from the workspace, engine-agnostic):

- envelope codec (`protocol/`): Turtle in, typed objects out, and back,
- transport (`transport/`): iroh dial/accept, hello exchange, presence
  gossip,
- authenticator chain (`authenticator/`): policy -> script -> ai -> human,
  decision cache, audit shape.

Engine-specific:

- modulation executor: bind `rsntr:params` values to the engine's
  placeholders, execute, iterate rows, map engine types to xsd literals,
- footprint provider (section 11.1),
- state storage: the `_` tables' equivalent (a `rsntr` schema in postgres; a
  collection in mongo), backing `_peers`/`_policy`/`_audit`/`_rsntr`
  semantics.

Full symmetry means the adapter also runs the outbox worker: a postgres node
does not just answer, it issues `rsntr:Query` objects of its own, driven by
rows in its own outbox relation. Nothing in the envelope distinguishes a
"server" from a "client"; there are only peers with different `rsntr:mods`
lists.

### 11.1 Footprint providers and trust classes

The authenticator's ground truth today is sqlite's authorizer callback:
engine-derived, fires during prepare, per column. Each modulation needs its
own `FootprintProvider`:

- sql-sqlite: `sqlite3_set_authorizer` (engine-derived),
- sparql: the engine compiles to a single SQL SELECT over the triple-store
  tables, so the footprint is engine-derived through the same authorizer;
  policy on the rdf tables gates the whole store, and graph-level policy
  follows named-graph support,
- sql-postgres: `pg_query` (the actual postgres parser as a library) or
  server-side EXPLAIN (engine-derived),
- document modulations: the operation descriptor is already structured; the
  footprint is a projection of it,
- anything else: a generic parser (sqlparser) gives a parser-derived
  footprint, which is weaker: it can miss what the engine would actually
  touch.

Policies can therefore discriminate by footprint provenance: a modulation
whose footprint is engine-derived can be auto-allowed by rules; a
parser-derived or absent footprint escalates by default. A node MUST NOT
execute a modulation for which it has no footprint provider unless policy
explicitly accepts footprint-blind requests. The AI-proposes/human-ratifies
rule for rewrites holds across all modulations.

## 12. Costs, stated honestly

- Verbosity. The example query envelope in section 5 is ~230 bytes of Turtle;
  the equivalent postcard struct is ~90. A 100-row, 3-column result runs
  roughly 10-14 KiB as Row objects vs ~3 KiB in postcard. Call it 3-4x,
  before QUIC-level compression arguments (QUIC does not compress; a future
  encoding does). This is the price of self-description, and compact mode
  exists to stop paying it on trusted high-volume pairs.
- Parse cost. Every frame is a Turtle parse. oxttl is a fast streaming
  parser, but a binary decode it is not; the two-parsers problem (every node
  carries an RDF parser plus its SQL engine) is real. Mitigation is
  architectural: the codec is one shared crate, and hot paths can negotiate
  compact mode.
- Literal fidelity. `xsd:double` canonical form round-trips IEEE 754 doubles
  only if serializers are careful (write shortest-round-trip forms, never
  decimal-rounded); base64 inflates blobs 33% (BlobRef is the answer past
  ~1 MB); sqlite's flexible typing means `declType` is advisory, not a
  contract.
- No wire-schema validation. Nothing enforces envelope shape; a malformed
  object is a `protocol-error`, discovered at decode. SHACL shapes for the
  vocabulary would give validating peers a contract, but are deferred.
- Blank-node scope. Frames are parse-isolated, so blank nodes cannot
  reference across frames; row identity across a result set exists only via
  `rsntr:seq` plus `rsntr:id`, and a graph result exists as a single RDF
  graph only after reassembly of its Graph frames.

The compact mode is the escape hatch, and its existence is an admission, not
an embarrassment: universal and efficient are different axes, and the
envelope chooses universal by default.

## 13. Alternatives considered

| Alternative | What it solves | Why it is not the envelope |
|---|---|---|
| Substrait (v0.98, weekly releases; DataFusion/DuckDB/Velox) | engine-neutral relational query plans | queries only; no knock/presence/decision/hello story; a plan IR is also hostile to human-readable audit. Steal instead: a future `rsntr:mod "substrait"` for engines that consume plans. |
| Arrow Flight SQL | columnar result transport, SQL over gRPC | client-server topology, gRPC not p2p QUIC, no policy participant in the protocol. Its columnar-batch idea maps to compact mode, not to the envelope. |
| ADBC | vendor-neutral client API | an API standard, not a wire protocol; nothing to put on the wire. |
| PartiQL | one query language over many data models | the opposite bet: we keep native modulations and make the wrapper universal instead. |
| GraphQL | schema-first client-server queries | resolver-centric, needs a schema authority per node, no symmetric peers, no writes-as-requests model that fits the authenticator. |
| Datalog | elegant recursive queries | another single-language bet, plus no engine ships it as its native interface. |
| multi-modulation SQL in a JSON/postcard envelope | minimal change from draft 1 | handles queries fine; then knock, presence, decision, and hello each need ad hoc message types, and the result is a second, private vocabulary that only this network speaks. RDF is that vocabulary done as a standard, with the parent project already speaking it. |

The argument in one line: the hard heterogeneity is message kinds, not query
modulations, and RDF is the only candidate where every message kind is
uniformly self-describing data.

## 14. SPARQL closes the loop

The convergence the POC left as a future note is realized in v3: `sparql` is
a builtin modulation (section 4.2), so every node's envelope and default
query payload are both RDF, and CONSTRUCT results ride the wire as the same
Turtle the envelope is made of. An Oxigraph-backed node (Antenna itself) can
join with `rsntr:mods "sparql"` and serve the identical modulation from a
different engine; the SPIN heritage closes its loop. SQL-to-SPARQL
translation stays out of scope; a client that speaks both simply queries each
peer in the modulation that peer serves.

## 15. Versioning

- ALPN `resonator/rdf/0` = envelope major version; incompatible changes bump
  it; a Router can register several majors during transitions.
- `rsntr:ver` in the hello = minor version; additive only.
- Vocabulary evolution follows RDF norms (v2 wiki, rdf-protocol): an IRI,
  once minted, never changes referent; new classes and properties are added,
  never repurposed; unknown predicates on known classes are ignored, and
  unknown classes on the response path decode as Generic (section 10). The
  open-world model makes additive evolution the default rather than a
  discipline.
- The v2-to-v3 cutover bumped the vocabulary namespace to
  `http://resonator.network/v3/rsntr#` while keeping ALPN `resonator/rdf/0`;
  wire compatibility with the v2 POC is intentionally broken, and the POC's
  doc examples survive as re-namespaced conformance fixtures rather than as
  wire peers.
