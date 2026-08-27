# The Projection: Capability Discovery for the Resonator Network

Status: accepted, 2026-07-29. Supersedes the v2 POC doc (research/rsntr-sqlite3/docs/projection-protocol.md); v3 namespace throughout. Normative companion to [rdf-envelope-protocol.md](rdf-envelope-protocol.md) and [connection-protocol.md](connection-protocol.md). Ontology: [rsntr-projection.ttl](rsntr-projection.ttl).

## 1. Why a projection

The hello tells a peer which modulations a node executes (`rsntr:mods`), and the mandatory `help` modulation teaches a human, in prose, how to use the node. Neither tells a client, in data, what the node offers and how to invoke it. A teletype client cannot render a numbered menu from prose, and an observer cannot build a toolbar from `rsntr:mods "sql-sqlite"`.

The projection fills that gap. A node describes its capability surface as RDF: what can be read, what can be done, what can be watched, and what input each of those needs. The client decides everything about presentation. The same projection renders as a numbered list with prompts on a teletype, as a panel of buttons and forms in the web interface's projection browser, or as any surface a future observer invents. The server sends semantics; the client owns rendering.

Prior art, and what is borrowed from each:

- W3C Web of Things Thing Description (W3C Recommendation): the affordance triad of properties / actions / events, adopted here as Radiant / Excitable / Sympathetic.
- Hydra Core Vocabulary (W3C CG draft): operations attached to resources; the idea that a capability carries its own invocation description.
- SHACL (W3C Recommendation): input described as shapes from which a client generates prompts or forms; adopted here as a deliberately flat SHACL-lite (`rsntr:Coupling`).
- schema.org `potentialAction`, Siren, XForms, Gopher menus: same idea at various depths.

None of these vocabularies is imported. The wire is Turtle-only and the vocabulary stays small and owned (the Turtle-over-JSON-LD decision stands); `owl:equivalentClass` mappings to `td:` and `sh:` are a future interop appendix, not a v1 concern.

Naming: the terms are resonance-native. A node projects a capability surface toward an observer; each capability is a point the observer can resonate with. The metaphor is load-bearing twice: a projection is observer-relative by definition (section 4, policy), and sympathetic resonance is literally "this vibrates when that vibrates", which is a subscription. Class names carry the metaphor; plumbing properties (`rsntr:field`, `rsntr:name`, `rsntr:datatype`) stay plain so a stranger reading raw Turtle can still guess.

## 2. Vocabulary

Same namespace and implied prefix block as the envelope: `rsntr:` is `http://resonator.network/v3/rsntr#`, alongside `xsd:`, `rdf:`, `rdfs:`.

### Classes

| Class | Meaning |
|---|---|
| `rsntr:Projection` | the capability surface a node projects toward one observer; an ordered collection of resonance points |
| `rsntr:ResonancePoint` | one thing the observer can couple with; superclass of the three kinds, and usable bare for pure navigation entries |
| `rsntr:Excitable` | a point the observer drives; invoking it has effects (an action) |
| `rsntr:Radiant` | a point that emits; the observer reads it (a property) |
| `rsntr:Sympathetic` | a point the observer entrains to; it vibrates when something happens (an event) |
| `rsntr:Coupling` | the input contract of a point: what the observer must supply to couple with it |
| `rsntr:Field` | one named input in a Coupling |
| `rsntr:Entrain` | request: begin resonating with a Sympathetic point (section 5) |
| `rsntr:Vibration` | notification: one vibration from an entrained point |
| `rsntr:Damp` | request: end an entrainment early |

### Properties

| Property | Range | On | Meaning |
|---|---|---|---|
| `rsntr:offers` | rdf:List | Projection | ordered ResonancePoints; order is presentation order |
| `rsntr:projects` | `xsd:string` | ResonancePoint | path of a deeper projection, fetched via the `projection` modulation |
| `rsntr:coupling` | `rsntr:Coupling` | Excitable, Radiant | input contract; absent means zero-argument |
| `rsntr:field` | rdf:List | Coupling | ordered Fields |
| `rsntr:name` | `xsd:string` | Field | binding name |
| `rsntr:datatype` | IRI | Field | expected datatype, an xsd IRI |
| `rsntr:required` | `xsd:boolean` | Field | default false |
| `rsntr:default` | literal | Field | prefill value |
| `rsntr:oneOf` | rdf:List | Field | enumerated allowed values |
| `rsntr:icon` | `xsd:string` | ResonancePoint | rendering hint, theme icon catalog name; a client MAY ignore |
| `rsntr:role` | `xsd:string` | ResonancePoint | rendering hint: `default` or `destructive` |
| `rsntr:fires` | `xsd:string` | Excitable | URN template to fire, for pipeline-hosted nodes (section 4) |
| `rsntr:signalTemplate` | `xsd:string` | Excitable, Radiant | statement template with `{name}` placeholders; fallback binding only |
| `rsntr:paramsOrder` | rdf:List | Excitable, Radiant | field names in positional-parameter order |
| `rsntr:point` | IRI | Entrain, Vibration, Damp | the Sympathetic point concerned |

Reused envelope terms, unchanged: `rdfs:label` and `rdfs:comment` for display text, `rsntr:mod` and `rsntr:signal` on points (a point carries the request it invokes, section 4), `rsntr:id` on projection responses and throughout entrainment, `rsntr:seq` and `rsntr:at` on Vibrations, `rsntr:hint` broadened to also carry per-Field prompt text.

Point IRIs are minted by the serving node and MUST be stable across fetches, so clients can bookmark them (a bookmarked point behaves like a toolbar item).

## 3. The projection modulation

`projection` is a modulation, requested with the ordinary choreography. `rsntr:signal` is a projection path; an empty signal is the root.

```turtle
[] a rsntr:Query ; rsntr:id "01K12M8Z4T9Q6W2E8R4T0Y6X3A" ; rsntr:mod "projection" ; rsntr:signal "" .
```

The response is one frame containing one `rsntr:Projection` graph (self-contained, no cross-frame blank nodes, within the ~256 KiB frame budget). Deep surfaces split via `rsntr:projects` rather than growing one giant frame.

```turtle
[] a rsntr:Projection ;
   rsntr:id "01K12M8Z4T9Q6W2E8R4T0Y6X3A" ;
   rsntr:offers ( <urn:notes:browse> <urn:notes:add> <urn:notes:changed> <urn:notes:admin> ) .

<urn:notes:browse> a rsntr:Radiant ;
   rdfs:label "browse notes" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "SELECT title, mtime FROM notes ORDER BY mtime DESC" .

<urn:notes:add> a rsntr:Excitable ;
   rdfs:label "add a note" ;
   rsntr:icon "plus" ;
   rsntr:coupling [ rsntr:field (
       [ rsntr:name "title" ; rsntr:datatype xsd:string ; rsntr:required true ; rsntr:hint "short title for the note" ]
       [ rsntr:name "body" ; rsntr:datatype xsd:string ; rsntr:required false ] ) ] ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "INSERT INTO notes (title, body) VALUES (?, ?)" ;
   rsntr:paramsOrder ( "title" "body" ) .

<urn:notes:changed> a rsntr:Sympathetic ;
   rdfs:label "a note was added" .

<urn:notes:admin> a rsntr:ResonancePoint ;
   rdfs:label "admin" ;
   rsntr:projects "urn:notes:admin" .
```

Rules:

- `projection` is a recommended modulation, listed in `rsntr:mods` next to `help`. Help remains the only mandatory modulation; a node without a projection is merely a black box to generic clients, not broken.
- A projection request passes the authenticator like any request. Default posture mirrors help: strangers may see a (possibly empty) public projection.
- Projected paths are the machine-readable analog of help topics: `rsntr:projects "urn:notes:admin"` is fetched with `rsntr:mod "projection" ; rsntr:signal "urn:notes:admin"`. A point that projects is itself the recursion of the core metaphor: a node projects a Projection, and a point within it projects a deeper one.
- Paths are opaque and discovered-only. A client MUST treat a path as structureless: echo it verbatim, never construct, split, or join one. The empty path (the root) is the only path a client may send without having been handed it. A path the caller was never offered answers `rsntr:Error` with `rsntr:code "point-unknown"`, the same as an unknown point.
- Convention: a node SHOULD use the projecting point's own IRI as the path, so a bookmarked point and a bookmarked submenu are the same thing (point IRIs are already stable). Short tokens (`"admin"`) or several points projecting one shared path are permitted owner aesthetics; either way the string carries no structure a client may interpret.

### The projection never lies

The projection is observer-relative and that is not a caveat, it is the definition. It is computed against the same `_policy` the authenticator enforces, per caller: a point the caller cannot successfully invoke does not appear in the caller's projection. Two peers asking the same node get different projections. This is the same rule that makes help trustworthy ("help never lies about access"), promoted from prose to data.

### Staleness

A projection is a snapshot; policy edits and node evolution change it. The node announces the change as data, using the vocabulary itself: the well-known Sympathetic point `<urn:rsntr:projection-changed>`, which vibrates whenever the caller's projection changes for any reason (policy change, points added or removed). A node that serves both `projection` and entrainment SHOULD expose it, list it in the root projection's offers, and accept entrainment to it by its well-known IRI without a prior fetch. On a vibration the client re-fetches the paths it cares about; re-fetch on `rsntr:Denied` remains the fallback for clients that do not entrain.

### Open world

The vocabulary evolves additively, like everything on this network. A client that meets an unknown ResonancePoint subclass or an unknown property ignores what it does not understand and still renders the `rdfs:label` as an inert entry (the same degradation rule the envelope's Generic passthrough applies to unknown response classes). Unknown terms are data, never errors.

## 4. Invoking a point

A point carries the request it invokes: `rsntr:mod` and `rsntr:signal` on an Excitable or Radiant are the same properties they are on `rsntr:Query`/`rsntr:Execute`. Invocation is therefore mechanical: copy `modulation` and `text`, add a fresh `rsntr:id`, and bind the coupling.

Bindings, in order of preference:

1. **Positional (preferred).** `rsntr:signal` is a fixed statement with the modulation's native positional placeholders; `rsntr:paramsOrder` lists field names in order. The client builds `rsntr:params` from the field values. An optional field left empty takes an `rsntr:null` slot. No string interpolation, so nothing to escape. Exciting `<urn:notes:add>` with title `groceries` and no body:

```turtle
[] a rsntr:Execute ;
   rsntr:id "01K12M9G7H2J5K8M1N4P7R0S3B" ;
   rsntr:mod "sql-sqlite" ;
   rsntr:signal "INSERT INTO notes (title, body) VALUES (?, ?)" ;
   rsntr:params ("groceries" rsntr:null) .
```

   The response is the ordinary Result/Row/Done (Radiant) or Done (Excitable) stream, with Denied and Error as ever.

2. **Template (fallback).** `rsntr:signalTemplate` with `{name}` placeholders substituted from field values, for modulations without parameter binding. The client is responsible for modulation-appropriate escaping; serving nodes SHOULD prefer the positional binding wherever the modulation allows it, precisely to keep escaping out of the protocol.

3. **Fire (pipeline-hosted nodes).** `rsntr:fires` holds a URN template, `{name}` placeholders bound from the coupling (or from client context such as a selected message id). The client sends the resolved URN into the node's event pipeline. This is the binding for radio-style nodes whose behavior lives in pipelines, not query engines; it is carried over from the superseded v2 antenna vocabulary (section 7) and kept as a defined extension point.

A Radiant with no coupling is a zero-argument read: the client may execute it immediately, poll it, or render its latest result inline. A Sympathetic point names an observable source of vibrations; a peer subscribes by entraining to it (section 5).

## 5. Entrainment

A peer subscribes to a Sympathetic point by entraining to it. The physics term is exact: entrainment (Andronov school: frequency capture) is a weakly coupled oscillator locking onto another's rhythm for as long as the coupling holds, and the coupling here is the connection.

```turtle
[] a rsntr:Entrain ; rsntr:id "01K12MA53C6D9F2G5H8J1K4M7C" ; rsntr:point <urn:notes:changed> .
```

The request passes the authenticator like any other. The answer is a bare `rsntr:Done` (entrained), `rsntr:Denied`, or `rsntr:Error` with `rsntr:code "point-unknown"` for a point outside the caller's projection. After the Done, the node sends one `rsntr:Vibration` frame on the same stream each time the point signals:

```turtle
[] a rsntr:Vibration ;
   rsntr:id "01K12MA53C6D9F2G5H8J1K4M7C" ;
   rsntr:point <urn:notes:changed> ;
   rsntr:seq 0 ;
   rsntr:at "2026-07-29T10:00:00"^^xsd:dateTime .
```

The envelope standardizes only the correlation skeleton: `rsntr:id` (which entrainment), `rsntr:point` (which signal), `rsntr:seq` (delivery order within this entrainment, from 0), `rsntr:at` (when). Everything else in the frame is the node's own: the frame is self-contained Turtle, so the node MAY include domain triples alongside the Vibration object (the new note's URN, the changed row).

Lifetime is the coupling:

- entrainment is connection-scoped: the connection dropping ends it, so there is nothing to lease, renew, or expire;
- closing the entrain stream damps it;
- `[] a rsntr:Damp ; rsntr:point <urn:notes:changed> .` on the stream asks for the same thing in-band; the node confirms with a Done and closes the stream.

Vibrations are ticks, not a log. A node MAY coalesce a burst into fewer vibrations (silently in v1; a coalesced-count would be an additive extension), `rsntr:seq` counts delivered vibrations only, and nothing is replayed on reconnect: a peer that reconnects re-entrains and reads a Radiant to catch up. This keeps event history out of the protocol by construction; a point whose history matters exposes that history as a Radiant.

Backpressure resolves the same way. If coalescing still cannot keep the stream ahead of a slow consumer, the node damps the entrainment itself: an `rsntr:Error` with `rsntr:code "limit-exceeded"` ends the stream. The client may re-entrain and catch up through a Radiant; an overdriven resonance is stopped, not buffered without bound.

Delivery is per-connection only. A vibration reaches a peer on that peer's own authenticated stream, never over gossip, so a Vibration's payload can be policy-filtered per observer exactly like the projection itself. Fan-out cost is therefore linear in entrained peers, and that is accepted: a node's fan-out is bounded by its connection count anyway. If fan-out pressure ever demands more, the recorded direction is announce-and-pull (a tiny gossip ping "point X vibrated", data pulled per-connection), which scales the signal without ever moving data off the policy-checked path; it is not part of the protocol.

Owning the reversal: the network is otherwise exclusively pull-based, with anything subscription-like pushed to business logic. Entrainment partially reverses that, for live connections only, and the record shows it. Durable cross-connection subscriptions, managed refresh, and delivery guarantees remain out of the protocol. The natural v2-of-this-spec extension is physics-shaped but unspecified: a subscription that survives reconnection would decay unless refreshed by re-entraining.

## 6. Rendering (non-normative)

The same projection above, on a teletype (the `rsntr projection` CLI):

```
notes node : projection ""

  [1] browse notes
  [2] add a note        (title, body?)
  [3] a note was added  (vibrates)
  [4] admin >

choose: 2
title? groceries
body? <enter>
done, 1 row affected.
```

In the web interface's projection browser (the observer's seed), a natural mapping:

| projection term | observer rendering |
|---|---|
| `rsntr:Projection` | a panel or menu of numbered points |
| `rsntr:Excitable` | a button or menu entry (icon and role hints applied) |
| `rsntr:Radiant` | an inline value, a result table, or a live tile |
| `rsntr:Sympathetic` | a badge / notification source with an entrain toggle |
| `rsntr:Coupling` | a form, one input per Field, hints as placeholders |
| `rsntr:projects` | zoom target: a deeper level, portal-style |

`rsntr:projects` is deliberately the fractal move: drilling into a point projects a deeper surface, which matches the observer's planned zoom and depth-of-view model. A menu tree and a zoomable space are the same data at different renderings.

A point's `kind` MAY also be the `rsntr:Hologram` IRI: a hint that invoking it yields a mod-served web view rather than frames to render. Hologram rendering is response-driven regardless of the hint; see [hologram-protocol.md](hologram-protocol.md).

## 7. Mapping to the antenna vocabulary (historical)

The v2 antenna vocabulary (`antenna:ContextMenu`/`antenna:MenuItem`, the widget-level vocabulary inside a radio) is superseded in v3; this mapping is kept as the record of where the projection terms came from, and for any bridge that still meets a v2 radio:

| rsntr (network) | antenna (v2 widget) |
|---|---|
| `rsntr:Excitable` | `antenna:MenuItem` |
| `rdfs:label` | `antenna:label` |
| `rsntr:icon` | `antenna:icon` |
| `rsntr:role` | `antenna:role` |
| `rsntr:fires` (`{VAR}`) | `antenna:onActivate` (`%VAR%`) |
| `rsntr:Projection` + `rsntr:offers` | `antenna:contextMenuItems` list on `<urn:radio:self>` |

No `antenna:` term changes; the mapping is documentation, not migration.

## 8. Settled questions

No open questions remain in this doc. The settlement record:

- Invalidation: the well-known point `<urn:rsntr:projection-changed>` (section 3).
- Path formation: opaque, discovered-only strings; a node SHOULD use the projecting point's IRI as the path; no path grammar exists or will (section 3).
- Backpressure: coalesce, then damp with `limit-exceeded` (section 5).
- Fan-out: per-connection streams only; vibrations never ride gossip, preserving per-observer policy filtering (section 5). Announce-and-pull is the recorded direction if fan-out pressure ever appears, out of protocol for now.
- Decayed-lease entrainment and intra-projection pagination: confirmed future extensions, not v1. Until then, a projection that outgrows the frame budget splits into zoom levels.

Vocabulary-adjacent questions that live elsewhere: the modulation tag scheme (strings vs IRIs, now covering `help` and `projection` too) and SHACL shapes for wire validation remain open items tracked in the v3 plan (PLAN.md).
