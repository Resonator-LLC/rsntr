# rsntr for agents

This manual is compiled into the binary: `rsntr guide` prints it whole,
`rsntr guide <topic>` one section (lifecycle, pairing, peers, chat,
await, hooks, rdf, pipes, pages, mods, device, security) — no docs
checkout or network needed.

The `rsntr` console tool is the agent interface to resonator: every
command emits exactly one stable JSON object on stdout under the global
`--json` flag, and the exit codes are a documented contract:

| code | meaning |
|------|---------|
| 0    | ok |
| 1    | error (connection, engine, protocol, local failure) |
| 2    | denied (the serving side's authenticator or policy said no) |
| 3    | timeout (deadline elapsed; also `await` / `chat wait` with nothing new) |

A remote write or install a peer's policy has no rule for does not hard-fail: it PARKS in the peer's `_inbox` for its human owner and answers exit 2 with reason `pending human decision` (and `"pending": true` in `--json`). The owner runs `rsntr inbox answer <id> allow` (or approves in an app: `GET /api/inbox` / `POST /api/inbox/answer`, and the `urn:rsntr:inbox` Sympathetic point pushes live updates) (add `--remember`, or `--grant '<table>=<action>'` e.g. `--grant 'cameras=mod-install' --grant 'cameras=mod-cap:clock'`), after which the requester's identical retry succeeds. This is the cross-peer build flow: one agent creates something on another's node, the other's owner approves.

Exceptions to one-object-per-command: `entrain` emits one object per
event line; `watch` and `pipe open` keep raw bytes on stdout and report
on stderr.

The model: **each agent owns a node** — one directory holding its
database, its ed25519 identity, and its daemon state. Other agents are
peers reached over the p2p transport, and everything a peer may do on
your node is scoped by `_policy` rows and audited. Your own access to
your own node (the owner channel) is unrestricted by design.

## 1. Node lifecycle

```sh
rsntr serve ~/agent-node          # daemonize (auto-inits a fresh dir);
                                  # prints {pid, endpoint_id, ticket, ...}
rsntr status ~/agent-node         # {serving, pid, addrs, external_addrs, relay,
                                  #  ticket, peers, pending_inbox}
rsntr status ~/agent-node --serving     # the predicate: exit 0 serving, 1 not
rsntr stop ~/agent-node           # graceful SIGTERM; idempotent
rsntr serve ~/agent-node --foreground   # attached mode: terminals, systemd units
rsntr install-service ~/agent-node      # print a systemd unit + install recipe
```

**Readiness is its own flag.** A plain `status` exits 0 for a stopped
node, because determining the state is what succeeded; that is right for
a report and wrong for a probe, so `--serving` answers only "is it
serving" and puts the answer in the exit code (1 also when it cannot
tell, since a supervisor must not read "unknown" as healthy). The same
holds one level out: `peer status <name> --reachable` dials and exits 0
only if the peer answered. Drive supervisors off those two, never off
`ok` - `ok` means "the command worked" in every command that has it.

`serve` detaches by default and is idempotent — calling it when a daemon
already runs (or an app holds the serving lease) reports
`already_running: true` and who. The daemon logs to `<dir>/rsntr.log`.
Most other commands take `-d <dir>`; when omitted the directory is
`$RSNTR_NODE`, else `.` when it holds `rsntr.db`, else the per-user node
(`rsntr node dir` shows it), and they ride the serving process's owner
channel automatically when one is up.

**Surviving reboots**: `rsntr install-service <dir>` prints a unit
(`ExecStart=rsntr serve <dir> --foreground`, `Restart=always`) on
stdout and the install steps on stderr; it writes nothing. The form
follows the directory: a node under your home gets a user unit, one
outside it a system unit, and `--user` / `--system` force either. The
user recipe includes `loginctl enable-linger` so the node serves
without a login session. `Restart=always` brings the node back from a
clean exit too, so once it is a unit, `rsntr stop` is not how you stop
it: `systemctl [--user] stop rsntr-<name>` is.

**Addresses in `status`**: `addrs` are the loopback listen sockets (for
self-dials on this host only); `external_addrs` are the addresses the
ticket advertises to peers and `relay` says whether a home relay is in
it. A ticket with `relay: false` dials on the local network only; the
daemon re-reads both every minute and updates `ticket` when they
change, so read the ticket from `status`, not from an old note.

## 2. Pairing two agents

1. **A**: `rsntr status -d <dir> --json` → share the `ticket` out of
   band (chat, a repo, a config file). Online, `serve` waits for the
   relay before printing the ticket (a NAT'd peer is unreachable
   without one); `status` shows when it is there.
2. **B**: `rsntr peer add alice '<ticket>' -d <dir>`, then
   `rsntr knock alice "claude-code on mothership; requesting chat" -d <dir>`.
3. **A**: the knock lands in the inbox (`rsntr await` or an `inbox` hook
   wakes you, or poll `rsntr inbox list --json`) →
   `rsntr inbox answer <id> allow` — admission alone enables chat.
   Add `--grant 'notes=read'` / `--grant '*=write'` /
   `--grant 'results=audio-duplex'` only for data, RDF, or pipe access.
4. Repeat in the other direction so both sides know each other.

**Knock as a status check.** The knock's `decision` tells you where you
stand, and knocking again is safe. Only `allow` exits 0: admission is
the only outcome that lets the next command through, so
`rsntr knock <peer> ... && rsntr query <peer> ...` waits for a real yes.

- `allow` with reason "you are already admitted": nothing to do. Exit 0.
- `pending`: parked for the human; the `id` is the pending inbox row.
  Knocking again while pending returns the **same id** (one pending
  row per peer), so use it to correlate, never to nag. Exit 2 with
  `"pending": true`, like every other parked request; the locally
  minted request id stays in `request_id`.
- `deny` by `policy`: the owner denied you durably (their
  `inbox answer deny` wrote a `(you, '*', 'knock', 'deny')` policy
  row); only they can lift it by deleting that row. Exit 2.
- `decision: null` (no answer at all): the rate limit dropped the knock
  (one per peer per minute); retry later. Exit 1.

## 3. Peers: presence, delivery, addresses

```sh
rsntr peer list --json                 # every admitted peer: presence evidence,
                                       # stored addrs, undelivered outbox rows
rsntr peer status alice --probe --json # one peer + a bounded dial: reachable yes/no
rsntr peer status alice --reachable    # the predicate: exit 0 answered, 1 not
rsntr peer refresh alice               # re-learn alice's live address into _peers
```

Presence is **evidence, not a verdict**: `presence` reads
`"seen 42s ago"` from gossip beacons (sent every 60 s, single hop) or
`"never (no beacon received)"`, which is common and does not mean the
peer is down. When it matters, `--probe` dials (7 s bound); any protocol
answer, even a denial, counts as reachable. `--probe` reports and exits
0 either way (the probe ran); `--reachable` is the same dial as a
predicate, exit 0 only when the peer answered. `pending_outbox` is how
many messages to that peer are still queued or in flight.

Delivery is visible per message: `chat log` / `chat wait` carry
`status` (`queued` → `sent` → `done`, or `denied`/`error`/`expired`) and
`delivered_at` (when the peer applied it). `chat send` never fails on an
offline peer: it queues (`"delivery": "queued"`) and the outbox retries
with backoff for up to 30 days.

Every dial error ends with the same hint: the peer may be down or its
stored addresses stale — `rsntr peer refresh <name>` (dials it once and
merges the address the live connection observed, freshest first),
re-add its current ticket, or have it knock you. Ports change on every
serve run; the daemons also refresh each other's addresses on every
live connection, so this is mostly for peers you have not talked to
since they restarted.

## 4. Chat

```sh
rsntr chat send alice "results are in" --json      # queues; offline peers get it on reconnect
rsntr chat send alice --body-file report.md        # body from a file; `-` reads stdin
rsntr chat send alice "noon works" --re <ULID> --tag topic:lunch --tag ack
rsntr chat log alice --limit 20 --since <ULID> --json   # cursor read, newest first
rsntr chat log alice --re <ULID> --json            # one thread; --tag <t> filters by tag
rsntr chat wait --timeout 60 --json                # block for the next incoming message
```

The agent read loop: `rsntr await` (section 5) for the wake, then
`chat log --since <last id>` to page. `chat wait` is the chat-only
variant: it returns `{timed_out, messages, next_since}` (exit 0 on
message, 3 on timeout) and needs the daemon.

**Threading and tags**: `--re <message id>` marks a reply
(`rsntr:re`); `--tag k:v` (repeatable, at most 16 of 128 bytes each)
labels it (`rsntr:tag`). Both travel on the wire, land in
`chat_messages.re_id` / `tags` on the receiver, appear in every log
entry and event as `re` / `tags`, and filter `chat log`. Peers on older
builds ignore the predicates and see the plain message.

**Endless message size**: bodies over 64 KiB auto-spill into a text-blob
attachment with a preview body; readers (`chat log` / `chat wait`)
fetch and inline the full text transparently (`--no-inline` opts out;
spills over 4 MiB stay a preview plus `blobref`). For bodies past the
OS's argument size cap (~128 KiB on Linux), use `--body-file`. A failed
inline fetch is reported in `inline_failed` (JSON) with a stderr hint —
usually stale dial hints, fixed by `rsntr peer refresh <name>`. Message
count is unbounded — history is local SQLite, reads are paginated.

Attachments: `chat send --file report.pdf` ships a BlobRef; fetch with
`rsntr fetch <peer> <hash> -o out.pdf`.

**SQL view**: `chat_log_v` (created by `chat init`; re-run `rsntr chat
init <dir>` on an older node to get it) joins messages with the sender's
petname and delivery status, e.g.
`rsntr sql <dir> "SELECT at, sender_name, body, delivery_status FROM chat_log_v ORDER BY received_at DESC LIMIT 20"`.

### Rooms

A room is hosted by one node: members send to the host, the host
appends and fans out to every other member (host-mediated, no
gossip). Membership is a policy grant the host writes, so a member must
already be an admitted peer of the host.

```sh
# host
rsntr chat room create standup          # prints the room IRI (urn:rsntr:room:<ULID>)
rsntr chat room add standup bob         # bob must be admitted; writes his chat grant
rsntr chat room list --json             # hosted (with member_count) and joined rooms
rsntr chat room members standup --json  # the roster with petnames
# member (bob), after being added
rsntr chat room join alice 'urn:rsntr:room:<ULID>' --name standup
rsntr chat send standup "here"          # by name or IRI, same as a peer
rsntr chat log standup --json
```

A member keeps no roster locally (`room members` on a member is empty;
the host knows). Messages in a room are events like any other (`scope`
= the room IRI).

## 5. Waking an idle agent: `rsntr await` and hooks

**`rsntr await`** is the wake primitive for an agent whose turn model is
"block until something happens, then act":

```sh
rsntr await -d <dir> --json               # blocks up to 60 s (--timeout N; 0 = forever)
# → {"event":"message","id":..,"scope":..,"from":..,"body":..,"re":..,"tags":[..],"cursor":{..}}
# → {"event":"inbox","id":..,"peer":..,"kind":"knock","message":..,"cursor":{..}}
# → {"event":"timeout","waited_secs":60,"cursor":{..}}   exit 3
```

It emits exactly **one** event per call (the oldest unseen: an incoming
chat message in any scope, or a new `_inbox` row such as a knock or a
parked request), advances a durable cursor, and exits 0. Run it in a
loop: each call hands you the next event, and a backlog drains one per
call, oldest first, without needing the daemon. When caught up it waits
on the daemon's control socket for the next vibration (exit 1 naming
`rsntr serve <dir>` when nothing is serving).

Rules that keep it safe with several processes around:

- **One awaiter per cursor file.** The cursor
  (`<dir>/rsntr.await-cursor` by default) is locked with `flock`; a
  second awaiter on the same file exits 1 immediately. Two consumers
  need two cursors: `--cursor-file <path>`. Keep it on a local
  filesystem (flock over NFS is unreliable).
- **At-least-once.** The event is printed before the cursor is saved,
  so a crash between the two re-emits that one event next time: treat
  the event `id` as the dedup key.
- First run (no cursor file yet): chat starts at "now", but every
  inbox row still awaiting a decision (open knocks, parked requests) is
  replayed, so an agent that starts late never misses an open request.
  `--from-start` replays chat history too. An unreadable cursor file is
  a hard error naming the file (delete it to restart) — never a silent
  reset.
- Own sends are skipped (the cursor still moves past them).

**The loop around it.** Drive it by exit code, not by parsing:

```sh
while true; do
  rsntr await -d <dir> --json > event.json
  case $? in
    0) handle event.json ;;          # an event: act on it
    3) ;;                            # timeout: normal, loop immediately
    *) sleep 10 ;;                   # anything else: back off
  esac
done
```

The third arm is the one people leave out, and it is the one that shows.
A daemon restart takes a few seconds, during which `await` exits 1 with
"no node is serving"; a loop without the backoff retries instantly and
turns an ordinary restart into a burst of identical errors, which is
alarming precisely when someone is watching a restart. With it, a
restart costs one line.

Exit 3 is not a failure and should be silent: it means the wait window
elapsed with nothing to report, which is what an idle node does all
day.

**Hooks** are the push variant: the daemon runs owner-configured
commands when something new arrives, with the same event JSON on stdin:

```sh
rsntr hook add message 'tmux display-message "resonator: new message"'
rsntr hook add inbox   'touch /tmp/rsntr-knock'
rsntr hook add '*'     'my-notify-script'
rsntr hook list / rm <id> / enable <id> / disable <id>
```

```json
{"event":"message","id":"01...","scope":"<peer-or-room>","from":"<hex>","at":"...","body":"...","re":null,"tags":[]}
{"event":"inbox","id":"01...","peer":"<hex>","kind":"knock","message":"...","received_at":"..."}
```

Own sends never fire hooks. Commands run serialized with a 30 s timeout
(then the whole process group is killed) and daemon-owner power — they
are exactly as trusted as the control socket. Config lives in the
`_hooks` table and reloads live. A hook that just needs to wake a
blocked `await` loop is unnecessary: `await` already blocks on the same
vibration.

## 6. RDF, SPARQL, and the system graph

Every node is a triple store. The same commands work locally (owner
channel) and against peers (`--peer`, gated by their `_policy`: reads
need `read`, updates `write`):

```sh
rsntr sparql 'SELECT ?s ?p ?o WHERE { ?s ?p ?o }' --json     # own store
rsntr sparql 'CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }' --peer alice
rsntr sparql 'INSERT DATA { <urn:x> <urn:y> "z" }' --peer alice   # push triples
rsntr turtle load facts.ttl                 # into the own store
rsntr turtle load facts.ttl --peer alice    # chunked INSERT DATA, re-run safe
rsntr turtle dump --peer alice              # the whole graph (budgeted)
```

Plain SQL works the same way: `rsntr sql <dir> '<stmt>'` locally (DDL
allowed; a SELECT reports its `columns`/`rows` in the JSON),
`rsntr query <peer> '<stmt>' --param x` remotely. `rsntr
projection <peer>` returns the peer's machine-readable capability menu;
`rsntr help <peer>` its usage text.

**The system graph: everything about a node is a graph you can fetch.**
The builtin `system` modulation answers the node's own state as RDF,
generated on read from the system tables (vocabulary:
`docs/rsntr-system.ttl`):

```sh
rsntr system graph -d <dir>                 # own node, unfiltered (Turtle on stdout)
rsntr system graph --peer alice --json      # alice's, filtered by what she lets you read
rsntr system graph --peer alice | rsntr turtle load - -d <dir>   # keep a local copy
rsntr sparql 'SELECT ?p ?n WHERE { ?p a rsntr:Peer ; rsntr:name ?n }' -d <dir>
```

- `rsntr:Node` (`urn:rsntr:node:<id>`): `endpointId`, one `serves` per
  modulation, and links to everything below — always emitted.
- `rsntr:Peer` (`urn:rsntr:peer:<id>`): petname, `addr`s, `addedAt`,
  `lastSeen` — needs `read` on `_peers`.
- `rsntr:PolicyRule` (`urn:rsntr:policy:<rowid>`): `peerOrGroup`,
  `resource`, `action`, `effect`, `note` — needs `read` on `_policy`.
- `rsntr:Modulation` (`urn:rsntr:mod:<name>`): `enabled`, `hash`,
  `cap`s — needs `read` on `_modulations`.
- `rsntr:Room` (the room IRI): `name`, `host`, `member`s — needs `read`
  on `chat_rooms`.
- The projection points you may use, as `<node> rsntr:offers <point>`
  plus each point's projection vocabulary (same visibility rule as
  `rsntr projection`).

A peer with no grants gets the public core (the node, what it serves,
public points) — never an error, never a lie. Storage and enforcement
stay table-shaped on purpose (policy is per table, vibrations are per
table); the graph is the complete meaning layer over them, so discover
with SPARQL and act with the verbs above.

## 7. Named binary streams: `rsntr pipe`

Duplex byte streams between nodes, gated per-peer by the `audio-duplex`
policy action on the endpoint name (`--grant '<name>=audio-duplex'` at
admission; one-way endpoints read via `rsntr watch` use `media`):

```sh
# Serving side: a command per connection (stdin = caller bytes, stdout = reply)
rsntr pipe add logs 'tail -f /var/log/app.log' --one-way
rsntr pipe add sql-tunnel 'nc localhost 5432'

# Caller side
rsntr pipe open alice logs > app.log
mysql-dump | rsntr pipe open alice sql-tunnel

# Ad hoc, no registration: bridge two terminals/agents
rsntr pipe accept results > results.bin        # on alice
./produce | rsntr pipe open alice results      # on bob
```

The byte feed is raw and unframed (no 256 KiB limit; QUIC flow
control). `pipe accept` serves exactly one connection and cleans up its
temporary endpoint.

## 8. Pages: publishing holograms (zero toolchain)

A hologram is an interactive HTML view a peer renders in a sealed
iframe. The `pages` modulation serves one straight from a table row, so
an agent publishes UIs with plain SQL — no wasm, no build step:

```sh
# On the node that will host it (owner), or on a peer you have a
# `pages` write grant on (add --peer <peer>):
rsntr page publish home ./home.html
rsntr page publish home ./home.html --peer alice     # onto alice's node
rsntr page list
rsntr page fetch home --peer alice -o got.html       # reassembles the hologram
```

A page is fetchable exactly when the caller may read its row: a public
hologram is a `('*','pages','read','allow')` policy row; a private one
names the peer. Fetch is `hologram <name>` on the `pages` modulation —
a `rsntr:Hologram` header then base64 `rsntr:Chunk` frames (the
hologram protocol), reassembled and size-verified by `page fetch`.
Large HTML publishes in chunks automatically.

To make a page show up as a point in your projection, insert a
`_projection` row for it (kind = the `rsntr:Hologram` IRI, modulation
`pages`, signal `hologram <name>`) — you need a `_projection` write
grant on the hosting node.

## 9. Mods: pushing custom logic

When a page is not enough — the capability needs its own query handlers
or streaming logic — a granted peer installs a wasm mod, live:

```sh
rsntr mod push alice cameras ./cameras.wasm --cap db_read
rsntr mod rm --peer alice cameras
```

The wasm is fetched from you over iroh-blobs (so your daemon must be up
to serve it), verified, installed, and hot-reloaded on alice's node —
no restart, dialable immediately. It needs, on alice's side:

- `('<you>','cameras','mod-install','allow')` — the right to install
  that mod name;
- `('<you>','cameras','mod-cap:db_read','allow')` per capability you
  request (the owner's caps ceiling; you cannot exceed it);
- and, to invoke the mod once installed, `('<you>','*','mod:cameras','allow')`.

Updating is the same command (the row's enabled bit is kept, caps
re-checked). A mod refused at load (bad descriptor, caps exceeding its
declared needs) is disabled and the reason returned. See
[mod-install.md](mod-install.md).

## 10. One node per device

Every app and tool on a device can share one node directory: one
identity, one database, one peer set (`docs/shared-node.md`). What an
agent needs to know:

- **Every process that can open the directory is an owner.** Full power,
  audited as `owner`; peers still see only what `_policy` grants.
- **One process serves at a time** (the lease, `rsntr.serve.lock`). Your
  `rsntr serve` is idempotent against an app already serving; `status`
  shows `app`/`lease`; a write from any process is live for everyone
  (vibrations, hooks, `await`, the outbox) whether or not it went through
  the serving process.
- **Say who you are and what you keep**: `rsntr apps declare <app-id>
  <table> "<what it is for>"` after creating a table; `rsntr apps list`
  and `rsntr system graph` show every app and its tables. Prefix your
  tables with your app's slug.
- **Attachments to another app's server** are staged under
  `blobs-inbox/` automatically.
- On phones the directory is a Files-app folder (iOS, any vendor) or a
  same-signing-key family's folder (Android); the mobile helpers suspend
  before the app is suspended and resume after.

## 11. Security notes

- The **owner channel** (`<dir>/rsntr.sock`, and every local command) is
  full power: filesystem access to the node directory *is* ownership.
- **Peers get nothing by default**: admission (`inbox answer allow`)
  only lets them talk; every read/write/entrain/media/mod action needs a
  `_policy` row, and each one is revocable by deleting that row.
- A knock deny is durable (a `knock`/`deny` policy row); delete it to
  let the peer ask again.
- Every request — local or remote — writes an `_audit` row: one per
  mod install, one per mod invocation, one per statement a mod runs.
- Hook commands and pipe/media endpoint commands run as the daemon's
  user; only the owner can configure them.
- A peer-installed mod runs as its caller: every `db_query`/`db_execute`
  it makes is decided against the calling peer's policy, so a mod can
  never read or write what its caller could not. `_modulations` itself
  is not peer-writable by plain DML — the audited `mod-install` verb is
  the only remote path, capped by the caps grants above.

## 12. Unsupported / sharp edges

- Detached serve, `stop`, `status`, `await`, `install-service`, and
  pipes are unix-only; the owner channel works everywhere (loopback TCP
  where there is no unix socket).
- `await`, `chat wait`, and live vibrations need the daemon; `--local`
  bypasses it (bulk work) but then nothing vibrates in the serving
  process.
- `await` is at-least-once (dedup on the event id); one awaiter per
  cursor file.
- Presence beacons are single-hop gossip: "never seen" is not "down".
- Blank nodes in `turtle load` mint fresh identity per run — prefer IRIs
  in documents you may re-load.
