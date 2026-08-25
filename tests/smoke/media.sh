#!/usr/bin/env bash
# Open a media feed through a running mux.sh listener and write the raw
# bytes to stdout. One call = one new stream on the shared connection.
#
#   media.sh <local-port> <source>                     # raw bytes to stdout
#   media.sh 9000 nvr/39 | ffplay -f mpegts -fflags nobuffer -framedrop -
#
# The node must have admitted the mux client key (_peers) and allowed the
# source (`rsntr media allow`). Run `stream.sh <port> hello` once first if
# this is the connection's first stream.
set -euo pipefail
PORT="${1:?usage: media.sh <local-port> <source>}"
SOURCE="${2:?need a media source name, e.g. nvr/39}"
here="$(cd "$(dirname "$0")" && pwd)"
proto="$here/target/debug/proto"
[[ -x "$proto" ]] || (cd "$here" && cargo build >&2)
# Keep our send side open for the life of the feed: nc closes the whole
# TCP client when stdin ends, which would tear the stream down.
{ "$proto" media "$SOURCE"; tail -f /dev/null; } \
  | nc 127.0.0.1 "$PORT" | "$proto" media-recv
