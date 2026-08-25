#!/usr/bin/env bash
# Phase 2: the patched-dumbpipe MULTIPLEXER. One iroh connection to a node, one
# new bidi stream per local TCP client on LOCAL_PORT (hello + command + media).
#
#   ./mux.sh <ticket> [local-port] [client-secret-64hex]
#
# Get <ticket> from the node:
#   online:            rsntr ticket <dir>
#   offline localhost: ./target/debug/ticketgen <endpoint-id> 127.0.0.1:<serve-port>
#
# Prints the client endpoint id: admit it into the node's _peers (+ a _policy)
# to run queries. The hello handshake works even while unadmitted.
set -euo pipefail
TICKET="${1:?usage: mux.sh <ticket> [local-port] [secret]}"
LOCAL_PORT="${2:-9000}"
SECRET="${3:-1111111111111111111111111111111111111111111111111111111111111111}"
here="$(cd "$(dirname "$0")" && pwd)"
dp="$here/../dumbpipe/target/debug/dumbpipe"
gen="$here/target/debug/ticketgen"
[[ -x "$dp" ]]  || { echo "build the fork first: (cd ../dumbpipe && cargo build)" >&2; exit 1; }
[[ -x "$gen" ]] || (cd "$here" && cargo build >&2)
echo "client endpoint id (admit for queries): $("$gen" pubkey "$SECRET")" >&2
echo "multiplexer up: 127.0.0.1:$LOCAL_PORT  (each 'nc 127.0.0.1 $LOCAL_PORT' is a new stream; hello first)" >&2
exec env IROH_SECRET="$SECRET" "$dp" connect-tcp \
  --addr "127.0.0.1:$LOCAL_PORT" "$TICKET" \
  --custom-alpn utf8:resonator/rdf/0 --reuse-connection
