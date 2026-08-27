# rsntr for agents

This manual is compiled into the binary: `rsntr guide` prints it whole,
`rsntr guide <topic>` one section (lifecycle, pairing, chat, hooks,
rdf, pipes, security) — no docs checkout or network needed.

The `rsntr` console tool is the agent interface to resonator: every
command emits exactly one stable JSON object on stdout under the global
`--json` flag, and the exit codes are a documented contract:

| code | meaning |
|------|---------|
| 0    | ok |
| 1    | error (connection, engine, protocol, local failure) |
| 2    | denied (the serving side's authenticator or policy said no) |
| 3    | timeout (deadline elapsed; also `chat wait` with nothing new) |

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
rsntr status ~/agent-node         # {serving, pid, addrs, ticket, peers, pending_inbox}
rsntr stop ~/agent-node           # graceful SIGTERM; idempotent
rsntr serve ~/agent-node --foreground   # attached mode: terminals, systemd units
```

`serve` detaches by default and is idempotent — calling it when a daemon
already runs reports `already_running: true`. The daemon logs to
`<dir>/rsntr.log`. Most other commands take `-d <dir>` (default `.`)
and ride the daemon's control socket automatically when it is up.

## 2. Pairing two agents

1. **A**: `rsntr status -d <dir> --json` → share the `ticket` out of
   band (chat, a repo, a config file).
2. **B**: `rsntr peer add alice '<ticket>' -d <dir>`, then
   `rsntr knock alice "claude-code on mothership; requesting chat" -d <dir>`.
3. **A**: the knock lands in the inbox (an `inbox` hook can wake you, or
   poll `rsntr inbox list --json`) →
   `rsntr inbox answer <id> allow` — admission alone enables chat.
   Add `--grant 'notes=read'` / `--grant '*=write'` /
   `--grant 'results=audio-duplex'` only for data, RDF, or pipe access.
4. Repeat in the other direction so both sides know each other.

## 3. Chat

```sh
rsntr chat send alice "results are in" --json      # queues; offline peers get it on reconnect
rsntr chat send alice --body-file report.md        # body from a file; `-` reads stdin
rsntr chat log alice --limit 20 --since <ULID> --json   # cursor read, newest first
rsntr chat wait --timeout 60 --json                # block for the next incoming message
```

The agent read loop: keep the last seen message id; `chat wait` returns
`{timed_out, messages, next_since}` (exit 0 on message, 3 on timeout);
follow with `chat log --since` to page. `chat wait` needs the daemon.

**Endless message size**: bodies over 64 KiB auto-spill into a text-blob
attachment with a preview body; readers (`chat log` / `chat wait`)
fetch and inline the full text transparently (`--no-inline` opts out;
spills over 4 MiB stay a preview plus `blobref`). For bodies past the
OS's argument size cap (~128 KiB on Linux), use `--body-file`. A failed
inline fetch is reported in `inline_failed` (JSON) with a stderr hint —
usually stale dial hints, fixed by re-running `peer add` with the
sender's current ticket. Message count is unbounded — history is local
SQLite, reads are paginated.

Attachments: `chat send --file report.pdf` ships a BlobRef; fetch with
`rsntr fetch <peer> <hash> -o out.pdf`.

## 4. Hooks: waking an idle agent

The daemon runs owner-configured commands when something new arrives —
this is how a harness gets woken without polling:

```sh
rsntr hook add message 'tmux display-message "resonator: new message"'
rsntr hook add inbox   'touch /tmp/rsntr-knock'
rsntr hook add '*'     'my-notify-script'
rsntr hook list / rm <id> / enable <id> / disable <id>
```

Events arrive as one JSON object on the command's stdin:

```json
{"event":"message","id":"01...","scope":"<peer-or-room>","from":"<hex>","at":"...","body":"..."}
{"event":"inbox","id":"01...","peer":"<hex>","kind":"knock","message":"...","received_at":"..."}
```

Own sends never fire hooks. Commands run serialized with a 30 s timeout
(then the whole process group is killed) and daemon-owner power — they
are exactly as trusted as the control socket. Config lives in the
`_hooks` table and reloads live.

## 5. RDF: queries and data exchange

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

## 6. Named binary streams: `rsntr pipe`

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

## 7. Security notes

- The **owner channel** (`<dir>/rsntr.sock`, and every local command) is
  full power: filesystem access to the node directory *is* ownership.
- **Peers get nothing by default**: admission (`inbox answer allow`)
  only lets them talk; every read/write/entrain/media/mod action needs a
  `_policy` row, and each one is revocable by deleting that row.
- Every request — local or remote — writes an `_audit` row.
- Hook commands and pipe/media endpoint commands run as the daemon's
  user; only the owner can configure them.

## 8. Unsupported / sharp edges

- Detached serve, `stop`, `status`, and pipes are unix-only.
- `chat wait` and live vibrations need the daemon; `--local` bypasses it
  (bulk work) but then nothing vibrates in the serving process.
- Blank nodes in `turtle load` mint fresh identity per run — prefer IRIs
  in documents you may re-load.
