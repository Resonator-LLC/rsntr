#!/usr/bin/env bash
# Drive ONE bidi stream through a running mux.sh listener. Each call is a new
# stream on the shared connection.
#   stream.sh <local-port> hello
#   stream.sh <local-port> query "SELECT id, body FROM notes ORDER BY id"
set -euo pipefail
PORT="${1:?usage: stream.sh <local-port> hello|query [SQL]}"
MODE="${2:?need mode: hello|query}"
here="$(cd "$(dirname "$0")" && pwd)"
proto="$here/target/debug/proto"
if [[ "$MODE" == "query" ]]; then
  "$proto" query "${3:?need SQL}" | nc 127.0.0.1 "$PORT" | "$proto" decode
else
  "$proto" "$MODE" | nc 127.0.0.1 "$PORT" | "$proto" decode
fi
