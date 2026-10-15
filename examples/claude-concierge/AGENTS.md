# claude-concierge, for agents

Audience: an AI agent (or human) modifying this example later.
README.md is the user-facing quickstart; this file explains the
machinery, the invariants, and the traps. Verified against the codebase
as of 2026-08.

## One-paragraph model

One Python process, two brains. The resonator daemon (spawned via
`rsntr serve`, detached, idempotent) is the body: it holds the
identity, serves chat/pages/projection/mods, and keeps serving when the
Python process dies. The Python loop is the mind: it wakes on
`rsntr chat wait`, admits knockers from `rsntr inbox list`, and for
each conversation with a new incoming message runs ONE Claude Agent
SDK `query()` whose tools are thin wrappers over the same `rsntr` CLI.
The model's final text is sent back verbatim as the chat reply.

## The loop

    setup: rsntr serve (auto-init) -> policy seed ('*','pages','read','allow')
    cycle: admission_step        rsntr inbox list / inbox answer allow + greeting
           chat wait --timeout N exit 3 = quiet, loop; exit 0 = messages
           handle_message(scope) chat log -> transcript -> think() -> chat send

Everything that blocks runs off the event loop (`arsntr` =
asyncio.to_thread). TRAP: calling the sync `rsntr()` helper directly
inside `cycle`/tools blocks the asyncio loop and the SDK's own turn;
the test caught exactly this (chat wait starved the sender task).

TRAP: `chat wait` is only the wake-up, never the source of truth. Its
cursor starts when the wait starts, so a message landing between waits
(or while the agent is down) is invisible to it. The cycle therefore
scans `chat_messages` past a persistent rowid cursor
(<dir>/agent-cursor.txt) after every wait; this is also what gives the
agent downtime recovery. The live smoke hit exactly this hole.

## The tools (the model's hands)

| tool | wraps | guard |
|---|---|---|
| publish_page | rsntr page publish | name charset |
| list_pages / remove_page | rsntr page list / rm | name charset |
| add_projection | rsntr sql INSERT into _projection | IRI shape |
| grant_access | rsntr sql INSERT into _policy | action allowlist: read, entrain, chat, media, audio-duplex, mod:<name>; never on reserved tables |
| run_sql | rsntr sql | refuses statements mentioning _policy _peers _modulations _hooks _rsntr |
| create_wasm_mod | scaffold + cargo build + rsntr mod add/enable + daemon bounce | name/caps allowlist; compile stderr returned for iteration |
| node_info | rsntr status / page list / mod list | none |
| install_projection_on_peer | rsntr query <peer> INSERT _projection | peer id shape; PENDING relayed |
| publish_page_on_peer | rsntr page publish --peer | peer id + name shape; PENDING relayed |
| push_mod_to_peer | compile + rsntr mod push <peer> | name/caps allowlist; PENDING relayed |

TRAP (injection surface): chat messages are untrusted remote input that
flows into the model's prompt. The guards above are the actual security
boundary, not the system prompt. If you add a tool, add its guard; the
model must not be able to widen policy, touch reserved tables, or run
arbitrary shell, no matter what the transcript says. The SDK's builtin
tools (Bash, Write, ...) are blocked via disallowed_tools; allowed_tools
does NOT restrict builtins (known SDK issue). Custom MCP tools MUST
also be pre-approved via allowed_tools ("mcp__rsntr__<name>") or the
SDK permission layer refuses every call and the model politely reports
it has no permissions (observed live).

The cross-peer build flow (the point of the *_on_peer tools): the
concierge builds on the CHAT PARTNER's node. That node's authenticator
(default chain now includes the human tier) PARKS an unmatched write or
install in its own `_inbox` and answers exit 2 with reason
`pending human decision` (rsntr surfaces `"pending": true` in --json).
The tools detect that and return a PENDING message; the model relays it
so the partner (who IS that node's owner) approves via
`rsntr inbox answer <id> allow [--remember | --grant '<t>=<a>' ...]`,
then the model retries the same tool and the identical request passes
via the peer's decision cache or the granted policy rows. PENDING is
progress, not failure - the system prompt forbids claiming inability.

TRAP (own-node mod install): the mods registry loads at serve start.
The hot-reload path (docs/mod-install.md) is remote-lane only and a
node cannot dial itself, so create_wasm_mod bounces the daemon
(rsntr stop + serve, about 2s). chat wait just restarts on the next
cycle. The generated scaffold uses an ABSOLUTE path to crates/pdk.

## SDK notes

claude-agent-sdk auths via the logged-in claude CLI. One query() per
incoming message with the transcript inlined (no session state); custom
tools via @tool + create_sdk_mcp_server; let the async iterator finish
(breaking early can hang the underlying Query.close). The reply is
taken from ResultMessage.result, falling back to the last
AssistantMessage TextBlock.

## File map

    agent.py       the whole agent (PEP-723 uv script)
    test_agent.py  offline end-to-end test, canned brain, real nodes
    README.md      user quickstart

## Test traps

test_agent.py drives the visitor purely through the rsntr CLI. The
visitor must knock BEFORE the concierge peer-adds it, or the knock
answers "already admitted" and the admission path goes untested. The
concierge peer-adds the visitor after admission only to store dial
addrs so the queued greeting can deliver over loopback.
