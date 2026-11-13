# resonator-agents handoff

Status: all eight items are implemented and tested. This is the second agent's final update.

No real model provider was called. OpenRouter and abliteration.ai were **not** exercised because no keys were available. Every model test runs against `tests/common/fake_provider.rs` on 127.0.0.1.

## Layout
- `src/runtime.rs` provides `Integrations::mount(&mut PreparedApp) -> (Integrations, Router<AppState>)` and `shutdown()`. It sets up:
  - the executor, with the search, attachments and profiles adapters and the capabilities fetcher;
  - the dispatcher on the bus;
  - `AccountRobotDirectory` and `RobotSink`;
  - the agents;
  - a metrics extension.
- `src/adapters.rs` supplies search, attachments and profiles to resonator-robots.
- `src/robots/` contains:
  - the address policy (SSRF guard);
  - `sign`, the webhook HMAC;
  - capabilities;
  - `connector`, which does the webhook POST, the capabilities GET and the `EventHub` for SSE.
- `src/http/` serves `/robot/dataapi[/rpc]`, `/robot/rpc`, `/robot/register…` and `/robot/events`. The module docs list the request and response shapes.
- `src/llm/` holds `LlmClient`, the OpenAI-compatible client. It covers:
  - streaming with tool calls, plus a JSON fallback;
  - retries that honour Retry-After;
  - connect, first-byte, idle and total timeouts;
  - cancellation and usage accounting.
- `src/outline.rs` turns a wave into a threaded outline and truncates it to a budget.
- `src/tools.rs` implements the wave tools as executor operations. Agents and MCP share them.
- `src/agent/` contains:
  - `mod.rs`: `AgentRuntime`;
  - `markdown.rs`: Markdown to Wave formatting;
  - `context.rs`: the prompt;
  - `toolset.rs`: the tools offered to the model.
- `src/mcp.rs` serves `POST /mcp`. It speaks the 2026-07-28 stateless revision and the legacy revisions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05.
- `crates/resonator/src/main.rs`: `resonator-all serve` runs prepare → mount → serve. On SIGINT or SIGTERM it runs `integrations.shutdown()` and then `server.shutdown()`.
- resonator-server change: `/profile` adds `"robotKind": "agent" | "robot"` to the profile objects of robot accounts.

## Agent behaviour
**Triggers.** The agent responds in two cases:
- someone else adds it to a wave (`self_added`);
- a human creates or changes a direct reply to one of its blips.

Replies written by robots or agents are ignored. The dispatcher's hop limit applies on top of that.

**Debounce and answer-once.** A reply is answered only after it has been quiet for `ai.reply_debounce_ms` (default 10000; the same wait applies after the agent is added to a wave, and is extended while anyone keeps writing there). Each trigger is answered once for a given text. The `agent_triggers` table records its state: pending → answering → answered.

**Finished editing.** The quiet period is the fallback. When a person ends an edit session in the web client (Done, Shift+Enter, Escape, focus moving to another blip, starting to edit another blip) after changing the blip, the client submits a delta with one no-op wavelet operation (`OpenWave.signalDoneEditing`; a reply left empty and discarded, an untouched session and a deleted blip send none). The event generator produces no event for it; instead `AgentTriggers::finished_by` lists the authors of no-ops in the delivered deltas who did not change a document after their no-op. The dispatcher keeps that list only for in-process agents (`RobotEndpoint::registry == Registry::Agent`, which `register_agent` sets; external robots from the account store and SSE/webhook sinks are `Registry::Passive`), drops authors that are robots or agents known to the dispatcher, and only then may deliver a bundle without events. External robots never get such a delivery and their bundles are unchanged. The agent ignores its own address and robot accounts again.

On such a delivery, each armed quiet timer of that wave that waits on a person in the list has its deadline set to now, and its `wait_quiet` task is woken through a per-timer `Notify` (no polling). Which timers wait on whom: every armed timer stores an `editor` next to its deadline.
- A reply trigger: the last human who created or changed that reply blip in the bundle that armed or last extended the timer (else the blip's creator).
- `ADDED` (first answer or redo): the person who added the agent, replaced by the last human who changed any blip in a bundle that armed or extended the timer. Edits only by robots or agents extend the deadline but leave the editor.

A no-op from anyone else leaves the timer alone, so someone still typing is not cut short. With no timer waiting on that person nothing happens: no fetch, no model call. Everything after the timer is unchanged (fingerprint check, `max_reanswers`, cancelling a streaming answer, limits), so a no-op after a whitespace-only edit calls no model. The arming in a bundle is done before the "finished" handling, so a last edit and the no-op in the same bundle arm the timer and then fire it. An edit after the no-op (a later bundle, or later in the same deltas) arms the normal quiet period again. Measured effect of a bare no-op (tests `a_no_op_is_accepted_and_makes_nothing_unread` in resonator-server and `doneEditing.test.ts` in the client): it changes no blip (version, time, contributors) and makes nothing unread. The digest keeps its time, which comes from the blips. But it advances the wavelet's version and last-modified time, and the server's search index sorts by that time, so the wave moves to the top of the other participants' inboxes, as any edit would.

**Redo after an edit.** For each trigger, `agent_triggers` also stores the response blip, a `fingerprint` and a `reanswers` count. The last two columns were added in migration 6. The fingerprint is a SHA-256 of the text that was answered, with whitespace runs collapsed and the text trimmed per blip, so spacing-only edits are not changes. What it covers depends on the trigger:
- A reply trigger: the reply's text.
- Being added: the blips written by humans that come before the response blip in reading order. Blips by this agent and by other robots or agents do not count, and neither do replies to the answer or anything after it. If the response blip is gone, all human-written blips count.

When a human edit arrives for an answered trigger, the same quiet-period debounce is armed. For a reply trigger, that edit is a `reply_blips_changed`. For being added, it is any human edit in the wave. Edits made only by robots or agents never arm the debounce. When the debounce fires, the agent re-reads the wave and recomputes the fingerprint.
- If the fingerprint is unchanged, nothing happens and no model is called.
- If the trigger blip was deleted or emptied, or there is no human text left, the old answer stays.
- Otherwise the answer is redone. If a response to that trigger is still streaming, it is cancelled first: only the latest text matters.

A redo goes through the normal admission path: rate limits, daily budgets and the concurrency semaphore all apply. `ai.max_reanswers` (default 5; 0 restores the old answer-once behaviour) caps redos per trigger. The cap is enforced atomically by `RobotStore::reopen_trigger`.

The redo rewrites the existing response blip in place. The agent deletes the blip's text and streams the new answer into it, followed by the usual Markdown conversion. The old answer is left out of the prompt, and the task tells the model that the message was edited after it answered. If the response blip was deleted, or someone else has replied to it, a new blip is posted the normal way and recorded as the response blip. A reply here means the first blip of a reply thread on it, or the next blip in its thread: the same notion of "reply" that triggers an agent. The old answer then stays as the context of those replies.

Restarts: fingerprints persist, so nothing is re-answered merely because of a restart. Edits made while the server was down are only noticed at the next edit event. Triggers answered before migration 6 have no fingerprint. For a reply trigger, the next edit counts as a change. For being added, the current text is adopted without answering.

**Restart.** On start, pending triggers are answered. Triggers left in the answering state get the note "[The answer was interrupted by a server restart.]".

**Empty wave.** When the agent is added to a wave with no human text, it posts only the `greeting`, and only if one is configured.

**Queueing.** One response runs at a time per agent and wave. Triggers that arrive meanwhile are coalesced: the latest is answered and the others are marked answered. A queued redo that is coalesced away this way is dropped. Its trigger keeps its old fingerprint, so the next edit redoes it.

**Output.**
- The reply blip is created immediately, placed by `WaveView::answer_placement` / `added_placement` (`src/outline.rs`):
  - It answers a message with an indented reply under it (`createChild`), unless that message is the last blip of a reply thread. In that case it continues the thread (`continueThread`), so a side conversation stays together instead of indenting further each turn.
  - When added, it answers the last message by whoever added it. If that person wrote nothing, it goes at the end of the root thread.
  - If the wave cannot be read, it falls back to `continueThread`, or to appending when it was added.
- Text is streamed in `stream_batch_ms` batches.
- At the end, Markdown is converted to Wave formatting, unless someone else has touched the blip.

**Errors.** A provider error, timeout or cancellation ends the blip with the visible note "[The answer could not be finished: …]".

**Cancellation.** The provider request is cancelled in three cases:
- the agent is removed from the wave;
- the response blip is deleted;
- an append to the blip fails.

**Limits.**
- Global and per-user limits per minute are kept in memory.
- Global and per-user daily limits use the `agent_usage` table.
- A semaphore enforces `max_concurrent_responses`, and `max_queued_responses` caps the queue.
- Over a limit, the agent posts a short note if `limit_notice` is set.

## Tests (all passing)
| Crate | Unit | Integration |
| --- | --- | --- |
| resonator-agents | 48 | 50 (agents 23, llm_client 18, mcp 3, robots_api 6) |
| resonator-server | 125 | 135, including the `robot_store` tests (4) and the attachment quota test (1) |
| resonator-robots | 40 | 47 |

Clippy `--all-targets` and `cargo fmt --check` are clean for resonator-agents, resonator, resonator-server and resonator-robots. `cargo build --release -p resonator` succeeds.

## Not verified / residual risks
- Real providers (OpenRouter, abliteration.ai, local servers) were never called.
- The WebSocket test checks that the client receives the agent's deltas while the reply streams. It does not apply them to a client-side document model.
- The per-minute rate windows live in memory and reset on restart. The daily limits persist.
- A missing API key environment variable stops startup (fail fast). It does not disable just that one agent.
- MCP is incomplete in three ways:
  - responses are JSON only, with no SSE;
  - `subscriptions/listen` is not implemented;
  - there is no OAuth discovery, only bearer PATs and robot tokens.

## For assembly
Rename `resonator-all` to `resonator` once resonator-server's own binary goes. Do not do it before then: the UI agent builds `-p resonator-server` and runs `target/release/resonator`.
