# Decision: Browser Client over HTTP/2, Server Terminates iroh

Date: 2026-07-29
Status: accepted

## Decision

The resonator server terminates iroh/QUIC entirely. The HTML/JS browser
client speaks RDF to the server over plain HTTP/2:

- Reads: one fetch() streaming response body per stream.
- Writes: POST per stream.

This replaces the earlier direction (WASM iroh core in the browser plus a
WebRTC/WebTransport UDP gateway); that plan and its research docs were
discarded on 2026-07-28.

## Rationale

- The server becomes an ordinary HTTP/2 service: standard TLS certs,
  reverse-proxyable (nginx/Caddy), no UDP in the browser path, works on
  corporate networks that block QUIC.
- HTTP/2 per-stream flow control gives real end-to-end backpressure on
  reads with zero protocol code: a browser that stops reading fills the
  H2 window and the server stops pulling from the iroh stream.
- No WASM build of the networking stack, no custom tunnel protocol, no
  per-user cert rotation machinery.

## Protocol mapping

Reads (server -> browser):

- Each iroh/RDF stream the browser consumes is one fetch() whose
  response body is read incrementally (response.body.getReader()).
- Stream end maps to response end. Browser abandonment
  (AbortController / reader.cancel()) maps to H2 RST_STREAM, which the
  server translates to resetting the iroh stream.
- Video uses the same shape: a streaming response feeding MSE (fMP4)
  or WebCodecs (raw encoded frames) on the client.

Writes (browser -> server):

- Baseline, all browsers: sequential POSTs per stream carrying a chunk
  or batch of writes; ordering per stream via awaiting the previous
  POST (or sequence numbers).
- Upgrade, Chromium + Safari: one long-lived streaming-upload POST per
  stream (duplex: 'half', ReadableStream body). Feature-detect; fall
  back to sequential POSTs on Firefox.

Stream announcement (control plane):

- HTTP/2 push is dead, so the server cannot open a stream toward the
  browser. The browser opens one long-lived control feed at session
  start (SSE or ndjson streaming response); the server announces
  incoming streams there ("stream {id}, mod X") and the browser
  then fetches them. Session-level RDF notifications ride the same
  feed.

## Accepted tradeoffs

- No end-to-end encryption past the server: the server sees plaintext
  RDF, so it must be user-trusted (one session daemon per user fits).
- One H2 connection per origin: all streams share one TCP pipe, so
  connection-level head-of-line blocking exists under packet loss.
- Bidirectionality of an iroh bi stream is split across two HTTP
  exchanges (response for the read half, POSTs for the write half).

## Operational notes

- Raise SETTINGS_MAX_CONCURRENT_STREAMS above the server default if
  many parallel streams are expected.
- Any reverse proxy in front must not buffer streaming responses
  (X-Accel-Buffering: no for nginx; flush on write) and must allow
  long-lived requests.
- Give heavy video streams their own connection or origin if they
  starve RDF traffic in practice.

## Open items

- Control feed format: SSE vs ndjson.
- URL scheme: /stream/{id} for reads, write endpoint naming.
- In-stream record framing: length-prefixed records recommended, so
  readers never depend on chunk boundaries (proxies may re-chunk).
- Auth/session model between browser and the per-user server daemon.
