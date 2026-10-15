# Remote mod install (the `mod-install` modulation)

Status: normative for v3. A policy-gated peer installs, updates, or
removes a wasm mod on a node, taking effect live (no serve restart).
This is how AI agents on the network build capabilities — holograms and
custom query handlers — on nodes they have been granted access to. For
the no-toolchain path (HTML pages), see the `pages` modulation in
[hologram-protocol.md](hologram-protocol.md); this document is the wasm
path.

## 1. Why out of band

A realistic mod is 300-400 KiB; the envelope frame budget is 256 KiB and
an inline blob base64-inflates by 4/3. So the wasm never rides the
request. The installer names a blake3 blob hash, and the serving node
fetches those bytes from the installer over iroh-blobs (bao-verified —
a lying hash cannot yield the wrong bytes). The installer's daemon must
be running to serve the blob.

## 2. The request

An `rsntr:Execute` with `rsntr:mod "mod-install"` whose `rsntr:signal`
is a Turtle document; the typed subject's class is the verb.

```turtle
[] a rsntr:ModInstall ;
   rsntr:name "cameras" ;
   rsntr:hash "blake3:<64hex>" ;   # the blob to fetch
   rsntr:bytes 345530 ;            # optional, cross-checked after fetch
   rsntr:caps "db_read", "clock" ; # requested capabilities
   rsntr:addr "203.0.113.7:41000" ; # installer serving addrs (repeatable)
   rsntr:timeoutMs 5000 ;          # optional, clamped to the node ceiling
   rsntr:memoryMb 64 ;             # optional, clamped
   rsntr:note "..." .

[] a rsntr:ModRemove ; rsntr:name "cameras" .
```

The answer is a one-row result (`name`, `sha256`, `caps`, effective
`timeout_ms`, `memory_mb`) closed by `rsntr:Done`, or `rsntr:Denied` /
`rsntr:Error`. Update is the same `ModInstall` (upsert); the caps
ceiling is re-checked every time.

## 3. What the node does

1. Peer gate, then the policy gate (section 4). One `_audit` row per
   attempt.
2. Fetch the wasm from the requesting peer by hash, using the carried
   `rsntr:addr` addresses so it reaches the installer's blob-serving
   daemon and not a short-lived request endpoint sharing its key. Reject
   over a 4 MiB ceiling.
3. Compute sha256, upsert `_modulations` with `enabled = 1` (the grant
   is the approval, unlike the owner-lane `rsntr mod add` which starts
   disabled), and hot-reload the mods host.
4. The reload applies the registry's load rules — sha match, ABI 1,
   descriptor name == row name, granted caps cover the descriptor's
   declared needs. A mod refused here is disabled and the reason
   returned; a failed *update* therefore takes the mod down until
   re-pushed.
5. On success the transport's advertised modulation set is refreshed, so
   the new name is dialable immediately, even on already-open
   connections.

## 4. The gate

An install whose right is neither granted nor explicitly denied does not
fail: it PARKS in the target's `_inbox` for the human owner and answers
the stable reason `pending human decision` (exit 2, `"pending": true`
in `--json`). The owner approves it with the caps in one stroke:

    rsntr inbox answer <id> allow \
      --grant 'cameras=mod-install' \
      --grant 'cameras=mod-cap:db_read'

and the installer's retry (`rsntr mod push ...` again) then succeeds. An
explicit `deny` row is still a hard deny and never parks. This is what
lets one peer build a mod on another peer's node with the owner in the
loop, from chat.


Direct `_policy` matches (deny wins), scoped by mod name in
`table_name`:

- **Install/remove right**: `(peer|'*', <name>|'*', 'mod-install',
  'allow')`.
- **Caps ceiling**: each requested cap needs `(peer|'*', <name>|'*',
  'mod-cap:<cap>', 'allow')`. The installer chooses which caps to
  request but can never exceed what the owner granted. A cap without its
  grant denies the whole install, naming the missing cap.
- **Limits**: `timeout_ms` is clamped to the node's `max_duration_ms`
  and `memory_mb` to `max_mod_memory_mb` (default 256); the effective
  values are reported.

Grant them at admission with the existing syntax (the action may contain
colons):

```sh
rsntr inbox answer <id> allow \
  --grant 'cameras=mod-install' \
  --grant 'cameras=mod-cap:db_read' --grant 'cameras=mod-cap:clock'
```

To *invoke* the installed mod, the peer also needs the ordinary
`(peer, '*', 'mod:<name>', 'allow')` invocation grant.

## 5. The owner lane

On the control socket the `mod-install` modulation serves only a
blob-import affordance: an Execute with one text parameter (a local file
path) imports the wasm into the serving daemon's blob store and answers
its hash. This is how `rsntr mod push` gets the blob into the daemon
(which holds the store lock) before naming its hash to the peer.
Everything else on the owner lane points at `rsntr mod add`.

## 6. Security

A granted installer can install/update/remove within its `mod-install`
name scope, with caps a subset of its `mod-cap:*` grants and limits
clamped to the node ceilings. It cannot choose caps freely, exceed the
limits, write `_modulations` by direct DML (denied on the remote lane —
plain `INSERT INTO _modulations` under a `'*'` write grant is refused,
forcing this audited verb), or have its mod act as anyone but the
calling peer: every `db_query`/`db_execute` a mod runs is footprint-
collected and chain-decided against the caller, and audited. The trail
is one row per install attempt, one per mod invocation, and one per
statement the mod runs. Revoke by deleting the policy rows and
`rsntr mod rm --peer <peer> <name>`.

## 7. CLI

```sh
rsntr mod push <peer> <name> <file> [--cap ...] [--timeout-ms N] \
    [--memory-mb N] [--note ...]      # import locally, install on the peer
rsntr mod rm --peer <peer> <name>     # remove from the peer
```
