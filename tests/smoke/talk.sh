#!/usr/bin/env bash
# Phase 1: single-stream rsntr Hello exchange through stock `dumbpipe connect`.
#
#   ./talk.sh <ticket>
#
# Get <ticket> from the node itself:
#   online:            rsntr ticket <dir>
#   offline localhost: ./target/debug/ticketgen <endpoint-id> 127.0.0.1:<serve-port>
#     (rsntr ticket --offline mints a throwaway port, so use ticketgen there.)
set -euo pipefail
TICKET="${1:?usage: talk.sh <ticket>   (rsntr ticket <dir>, or ticketgen <id> <ip:port>)}"
ALPN="utf8:resonator/rdf/0"
here="$(cd "$(dirname "$0")" && pwd)"
proto="$here/target/debug/proto"
[[ -x "$proto" ]] || (cd "$here" && cargo build >&2)

out="$(mktemp)"; trap 'rm -f "$out"' EXIT
# dumbpipe hangs after the exchange (the node holds the connection open), so run
# it detached and stop it once the hello has round-tripped.
"$proto" hello | dumbpipe connect "$TICKET" --custom-alpn "$ALPN" >"$out" 2>/dev/null &
dp=$!; sleep 4; kill "$dp" 2>/dev/null || true; wait "$dp" 2>/dev/null || true
"$proto" decode <"$out"
