# claude-concierge

A resonator node with a Claude brain. The concierge is a long-running
peer that owns its own node, auto-admits anyone who knocks, chats over
the resonator chat, and builds things for whoever asks: HTML holograms
served by the builtin `pages` modulation, points on its projection
menu, access grants, and real wasm mods that it writes in Rust,
compiles, and installs on itself live. It is built on the Claude Agent
SDK and drives the node entirely through the `rsntr` CLI's stable
`--json` surface (docs/agents.md).

Requires: an `rsntr` binary built from this tree (0.2.x with the
`pages` modulation), a logged-in `claude` CLI (the SDK uses its
credentials), `uv`, and, for the wasm path, a Rust toolchain with the
`wasm32-unknown-unknown` target.

## Run the concierge

    uv run agent.py

It prints its endpoint id and ticket, then serves. The node daemon is
detached and keeps serving when the agent process stops. Useful flags:
`--dir` (node directory, default ~/rsntr-nodes/concierge), `--model`,
`--wait` (chat poll window), `--offline` (loopback only).

## Talk to it

From any other resonator node (`RSNTR` = your rsntr binary):

    rsntr peer add concierge '<ticket printed by the agent>'
    rsntr knock concierge "hi, I want a page"
    rsntr chat send concierge "make me a page called garden that shows my three favorite plants"
    rsntr chat wait --timeout 120            # the reply arrives here
    rsntr page fetch garden --peer concierge -o garden.html
    rsntr projection concierge               # the menu point it added

Ask for custom logic and it writes a wasm mod; after it grants you
`mod:<name>`, invoke it with `rsntr query concierge '<signal>' --mod <name>`.

Ask it to build on YOUR node ("make a pill tracker on my node") and it
installs the projection/page/mod onto your node via the mod-install
approval flow. Your node parks the request; approve it and retry:

    rsntr inbox list                        # find the parked request
    rsntr inbox answer <id> allow --remember   # or --grant for a mod
    # then tell the concierge "approved, go ahead" and it finishes

## Test

    uv run test_agent.py            # offline, no API key, ~30s
    RUN_WASM=1 uv run test_agent.py # also smoke the wasm build (minutes)

The test replaces the Claude turn with a canned brain and runs real
nodes over loopback: admission, greeting, guards, page + projection
round trip, and optionally the compile-install-bounce path.
