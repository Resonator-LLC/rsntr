# rsntr: Connection Establishment and Acceptance

Status: v3 normative, 2026-07-29. Supersedes the v2 POC doc
(research/rsntr-sqlite3/docs/connection-protocol.md); rewritten for the v3
namespace and terminology. Normative companion to
[rdf-envelope-protocol.md](rdf-envelope-protocol.md); this doc specifies the
handshake that precedes any request. The Hello exchange lands in `crates/transport`
(milestone M2); the knock/admission path lands in `crates/node` (milestone M4).

The handshake has two layers. The transport layer (iroh QUIC) proves identity and opens a channel; it carries no RDF. The envelope layer, from the first byte of application data onward, is RDF objects like everything else on the network. This doc gives the RDF for every envelope-layer message in establishment and acceptance.

All examples assume the implied prefix block (never transmitted):

```turtle
@prefix rsntr: <http://resonator.network/v3/rsntr#> .
@prefix xsd:   <http://www.w3.org/2001/XMLSchema#> .
@prefix rdf:   <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
```

Two peers throughout: alice dials, bob accepts. Their EndpointIds are written `ed25519:alice...` / `ed25519:bob...` for readability; on the wire and in every table (`_peers`, `_policy`, chat tables) they are the plain 64-hex public key.

## 1. Layer 0: transport establishment (not RDF)

Before any RDF flows, iroh does its work:

- alice dials bob's EndpointId on ALPN `resonator/rdf/0`. iroh resolves a route (direct hole-punched QUIC, or via relay) and completes the QUIC/TLS handshake.
- The handshake authenticates both ed25519 keys. When the connection is up, bob knows alice's EndpointId is proven, not claimed, and vice versa. This is the identity guarantee the envelope layer builds on: `rsntr:from` never has to be asserted in-band because the transport already established it.
- A connection dialed with a different ALPN, or by a key that fails the handshake, never reaches the envelope layer at all.

Everything below happens on QUIC bidirectional streams inside this one authenticated connection.

Liveness and address freshness (2026-08-04): connections carry a 10s QUIC
idle timeout over iroh's 5s keep-alives, so a connection to a peer that
restarted (a zombie: it looks open, accepts streams, never answers) is
closed within seconds rather than hanging requests; one dial is bounded
at 15s. Serving nodes also watch each live connection's network paths
and merge the peer's observed direct addresses into `_peers.addrs`
(fresh first, known peers only, capped) - ports change every serve run,
and this is what keeps dials direct across peer restarts without
re-running `peer add`. The web relay retries a request once on a fresh
dial after a stream error (safe: request ids are idempotent via
`_applied`); a slow-but-alive peer never errors and is never resent.

## 2. Layer 1: the Hello exchange

The first bi stream opened on a new connection is the hello stream. The dialer (alice) sends its `rsntr:Hello` first; the acceptor (bob) answers with its own `rsntr:Hello` on the same stream. This ordering is deterministic because QUIC delivers stream data in order and the acceptor reads before it writes.

alice -> bob:

```turtle
[] a rsntr:Hello ;
   rsntr:ver "0.1" ;
   rsntr:enc "turtle" ;
   rsntr:mods "sql-sqlite", "sparql", "help" ;
   rsntr:hint "resonator node; send an rsntr:Query with modulation 'help' for usage, or type HELP" .
```

bob -> alice:

```turtle
[] a rsntr:Hello ;
   rsntr:ver "0.1" ;
   rsntr:enc "turtle", "compact-postcard" ;
   rsntr:mods "sql-sqlite", "sparql", "help" ;
   rsntr:hint "resonator node; send an rsntr:Query with modulation 'help' for usage, or type HELP" .
```

Every `mods` list includes `help` (mandatory, envelope doc sec 4.1), and every hello carries `rsntr:hint`: a one-line plain-text pointer so a human or an AI reading the hello knows how to proceed without out-of-band knowledge of the protocol. A node that offers a browsable capability surface additionally advertises `mods "projection"` (recommended, [projection-protocol.md](projection-protocol.md)).

What each side learns and checks:

- Envelope major version is fixed by the ALPN (`resonator/rdf/0`); `rsntr:ver` carries the minor. A peer that cannot satisfy the minor answers `rsntr:Refused` (section 5) instead of a Hello and closes.
- `rsntr:enc` lists accepted frame encodings. `turtle` is mandatory for every peer. The intersection here is `{turtle}`, so the connection stays Turtle; had both offered `compact-postcard`, either side could switch subsequent request streams to it (switch mechanics are open question 1).
- `rsntr:mods` is the set of modulations this peer works: it executes them for the other side and can issue them itself. alice can send bob `sql-sqlite` or `sparql`; a request in a modulation absent from the responder's `mods` is rejected `mod-unsupported` before the authenticator runs. An adapter-backed tag may carry an engine version suffix (`sql-sqlite-3.46.0`); matching strips it (envelope doc sec 8) - the builtin v3 node advertises plain tags. (A finer transmit/receive split can return additively if asymmetric peers ever need it; unknown predicates are ignored.)
- Both hellos must agree with each node's own `_rsntr` table (its queryable self-description). A node whose hello and `_rsntr` disagree is buggy.

The hello exchange is pure capability negotiation. It does not decide whether the peers may talk to each other about data; that is acceptance (sections 3 and 4).

## 3. Acceptance path A: a known peer

A peer is "known" (admitted) if its EndpointId has a row in the local `_peers` table. Admission is the trust boundary, established out of band (section 4 is how a stranger crosses it).

A node additionally treats its own proven EndpointId as admitted, without a `_peers` row: local surfaces (the web interface, an embedded caller) act as the owner and pass the peer gate under the node's own identity. Over iroh this path is never reachable from the network (iroh refuses to dial its own id), so the rule grants nothing to remote peers; it exists for local surfaces and future transports.

If, after the hello exchange, bob finds alice's proven EndpointId in his `_peers`, the connection is accepted for requests. There is no extra acceptance object: presence in `_peers` plus a completed hello is acceptance. alice may now open request streams, each `rsntr:Query` or `rsntr:Execute` passing through bob's authenticator chain per request.

Optionally, on acceptance bob updates liveness and either side may begin gossiping presence to the peer-set (both are `rsntr:Presence` objects, see the envelope doc):

```turtle
[] a rsntr:Presence ;
   rsntr:at "2026-07-23T09:15:00"^^xsd:dateTime ;
   rsntr:status "around" .
```

## 4. Acceptance path B: a stranger knocks

If bob does not find alice in `_peers`, alice is a stranger. A stranger's connection completes the hello exchange (so both know each other's capabilities) but is otherwise inert: the only envelope object bob will accept from an unadmitted key is exactly one `rsntr:Knock`. Any `rsntr:Query`/`rsntr:Execute` from a stranger is refused at the peer gate, before parsing SQL.

alice -> bob, on a fresh bi stream:

```turtle
[] a rsntr:Knock ;
   rsntr:id "01J9V6QK3M8ZT0R4Y2W6B8N1D5" ;
   rsntr:message "Hi, this is alice, we met at the workshop. Requesting read access to recipes." .
```

Rules on the knock:

- One knock per connection. A second `rsntr:Knock` in the same window is dropped silently and does not extend the rate-limit budget.
- Knocks are rate-limited per-key and globally (token bucket, persisted), because the knock is the network's spam surface.
- The knock routes into the authenticator chain as an ordinary decision with `rsntr:action "knock"` and no SQL footprint. Policy or a script may auto-answer it; otherwise it parks for the human.

### 4.1 The knock parks for a human

If no automated tier decides, bob inserts the knock into `_inbox` and the connection waits (or alice's stream closes and she retries later; the knock is remembered by request id):

```sql
INSERT INTO _inbox (request_id, peer, sql, params, received_at)
VALUES ('01J9V6QK3M8ZT0R4Y2W6B8N1D5',
        'ed25519:alice...',
        '',              -- a knock carries no SQL
        'knock: Hi, this is alice ...',
        datetime('now'));
```

bob's owner answers by setting the decision, exactly as for any escalated request:

```sql
UPDATE _inbox SET decision = 'allow', decided_by = 'human'
WHERE request_id = '01J9V6QK3M8ZT0R4Y2W6B8N1D5';
```

### 4.2 Admission is a _peers row

Whichever tier decides, "allow" means one thing: alice gets a `_peers` row. That INSERT is the admission; from the next connection on, alice takes acceptance path A.

```sql
INSERT INTO _peers (endpoint_id, name, added_at, notes)
VALUES ('ed25519:alice...', 'alice', datetime('now'), 'admitted via knock 01J9V6QK...');
```

### 4.3 The acceptance answer, as RDF

bob answers the knock with an `rsntr:Decision` correlated by `rsntr:id`. On acceptance:

```turtle
[] a rsntr:Decision ;
   rsntr:id "01J9V6QK3M8ZT0R4Y2W6B8N1D5" ;
   rsntr:decision "allow" ;
   rsntr:decidedBy "human" ;
   rsntr:reason "welcome, recipes are readable" ;
   rsntr:at "2026-07-23T09:20:00"^^xsd:dateTime .
```

On refusal, the same object with `rsntr:decision "deny"`. A denied stranger is told once and then falls back to the plain refusal for any further non-knock traffic; the connection carries nothing more.

Every knock and its outcome is written to `_audit` (`direction = 'in'`, `decided_by` = the deciding tier), like all decisions.

## 4bis. The plain-text probe (a caller who does not speak the protocol)

Everything above assumes the dialer opens with a valid `rsntr:Hello` frame. A human on a raw connection, or an AI handed a socket with no schema, will not. Establishment must not meet them with a silent parse failure; the protocol teaches its own use.

If the first bytes on an accepted connection are not a valid length-prefixed envelope frame (unframed text, or a lone word like `help`, `HELP`, `?`), bob MUST reply with a plain-text banner, newline-terminated and unframed, and keep the connection open for a real hello:

```
resonator node (rsntr, envelope 0.1). I speak RDF objects over QUIC.
Hand-driven? Ask me for help in one line:
  [] a rsntr:Query ; rsntr:mod "help" .
and I will reply with usage. Not admitted? Knock:
  [] a rsntr:Knock ; rsntr:message "who you are and what you want" .
```

This is a courtesy affordance outside the framed protocol (envelope doc sec 4.1). Once the caller sends a proper frame, normal establishment resumes at section 2. A well-behaved client never triggers it; it exists entirely for the uninitiated human or AI, which is exactly who most needs it.

## 5. Refusal at the handshake

Before acceptance is even in question, a hello can be refused for capability reasons. Instead of answering with its own Hello, the responder sends `rsntr:Refused` and closes the connection:

```turtle
[] a rsntr:Refused ;
   rsntr:code "envelope-version" ;
   rsntr:reason "this node speaks envelope 0.x only" .
```

`rsntr:code` values at the handshake: `envelope-version` (minor version unsatisfiable), `encoding` (no common mandatory encoding, which should never happen since Turtle is mandatory), `protocol-error` (malformed hello). Refusal is distinct from a stranger's inert connection: refusal ends the connection at layer 1; a stranger's connection survives to carry exactly one knock.

## 6. State machine

Per connection, from bob's (acceptor's) point of view:

```
                 QUIC + ALPN handshake ok (alice's key proven)
                                |
                                v
                         [AwaitHello]  --- bad/unsatisfiable hello ---> send Refused, close
                                |
                        both Hellos exchanged
                                |
                                v
                       alice in _peers ?
                        /               \
                     yes                 no
                      |                   |
                      v                   v
                 [Accepted]           [Stranger]
                      |                   |
             open request streams   one Knock only ---> chain decides
                      |                   |                 /        \
                 authenticator      (else refused)     allow          deny
                 per request             |               |              |
                                         |          INSERT _peers    Decision deny
                                         |          Decision allow    (stays inert)
                                         |               |
                                         |               v
                                         |          next connection -> [Accepted]
                                         v
                                    audited throughout
```

## 7. Vocabulary added by this doc

Additive to the envelope vocabulary (existing classes `rsntr:Hello`, `rsntr:Knock`, `rsntr:Presence`, `rsntr:Decision` are unchanged):

| Term | Kind | Meaning |
|---|---|---|
| `rsntr:Refused` | class | handshake-level refusal, closes the connection |
| `rsntr:action` | property | decision action for non-SQL requests; value `"knock"` here |

`rsntr:Decision` is reused verbatim as the knock answer (no new class needed): a knock is a request, its Decision is the response, correlated by `rsntr:id`, exactly like a query's `rsntr:Denied`/`rsntr:Result`. Both additions follow the versioning rule (new terms, never repurposed; unknown predicates ignored), so they need no envelope major bump.

## 8. Summary sequence

```
alice                                   bob
  |   --- QUIC/ALPN resonator/rdf/0 --->  |   (keys proven both ways)
  |                                       |
  |   --- rsntr:Hello ------------------> |
  |   <-- rsntr:Hello -------------------  |   (or rsntr:Refused, close)
  |                                       |
  |            bob checks _peers          |
  |                                       |
  |   known:  --- rsntr:Query ----------> |   --> authenticator --> Result/Denied
  |                                       |
  |   stranger: --- rsntr:Knock --------> |   --> chain --> _inbox / _peers
  |             <-- rsntr:Decision ------  |
```

This is the whole of establishment and acceptance: iroh proves who, Hello settles what-language, `_peers` settles whether-at-all, and a knock is how a stranger asks to be written into `_peers`. All of it above the transport is RDF objects, dispatched by `rdf:type` like every other message on the network.
