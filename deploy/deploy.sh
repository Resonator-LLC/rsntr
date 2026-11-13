#!/usr/bin/env bash
# Builds the binaries here and installs them on the host of the official
# servers: tests first, the old binaries kept as .prev, a health check
# after the restart, and the old binaries back if it fails.
#
#   deploy/deploy.sh root@rsntr.net            # resonator and resonator-push
#   SKIP_TESTS=1 deploy/deploy.sh root@host    # when the tests just passed
#
# The host must run the same or a newer glibc than this machine (both are
# Ubuntu 26.04 for the official servers). iroh-relay is installed once:
#   cargo install iroh-relay@1.4.1 --features server --locked --root target/iroh-relay
#   scp target/iroh-relay/bin/iroh-relay root@host:/usr/local/bin/
set -euo pipefail
host=${1:?usage: deploy.sh user@host}
root=$(cd "$(dirname "$0")/.." && pwd)
INSTANCES=${INSTANCES:-"rsntr.net:9898 resonator.network:9897"}
cd "$root"

(cd client && npm ci --no-audit --no-fund >/dev/null && npm run build >/dev/null)
if [[ "${SKIP_TESTS:-0}" != 1 ]]; then
  CARGO_INCREMENTAL=0 cargo test --workspace --quiet
fi
CARGO_INCREMENTAL=0 cargo build --release -p resonator -p resonator-push
scp -q target/release/resonator target/release/resonator-push "$host:/tmp/"

ssh "$host" INSTANCES="'$INSTANCES'" bash -s <<'SH'
set -euo pipefail
units="resonator-push"
for i in $INSTANCES; do units+=" resonator@${i%%:*}"; done
for bin in resonator resonator-push; do
  [ -f /usr/local/bin/$bin ] && cp -p /usr/local/bin/$bin /usr/local/bin/$bin.prev
  install -m 755 /tmp/$bin /usr/local/bin/$bin.new && mv /usr/local/bin/$bin.new /usr/local/bin/$bin
  rm -f /tmp/$bin
done
systemctl restart $units

healthy() {
  for i in $INSTANCES; do
    curl -fs "http://127.0.0.1:${i##*:}/healthz" >/dev/null || return 1
  done
  curl -fs http://127.0.0.1:9900/health >/dev/null
}
for _ in $(seq 20); do
  if healthy; then echo "deployed: $(/usr/local/bin/resonator --version)"; exit 0; fi
  sleep 0.5
done
echo "health check failed; rolling back" >&2
for bin in resonator resonator-push; do
  [ -f /usr/local/bin/$bin.prev ] && mv /usr/local/bin/$bin.prev /usr/local/bin/$bin
done
systemctl restart $units
exit 1
SH
