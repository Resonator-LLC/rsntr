#!/bin/sh
# Runs the screenshot tests against the browsers of the official Playwright
# image (the version of @playwright/test), so pixels match on every machine.
# The tests and the servers run here; only the browsers run in the image.
#   e2e/visual/run.sh [playwright args, e.g. --update-snapshots]
# DOCKER="sudo -n docker" when docker needs sudo.
set -eu
cd "$(dirname "$0")/../.."
VERSION=$(npx playwright --version | awk '{print $2}')
IMAGE="mcr.microsoft.com/playwright:v$VERSION-noble"
DOCKER=${DOCKER:-docker}
PORT=${PW_VISUAL_PORT:-3555}
NAME="resonator-visual-$$"
$DOCKER run -d --rm --name "$NAME" --network host --init --user pwuser --workdir /home/pwuser \
  "$IMAGE" npx -y "playwright@$VERSION" run-server --port "$PORT" --host 127.0.0.1 >/dev/null
trap '$DOCKER stop "$NAME" >/dev/null 2>&1 || true' EXIT INT TERM
i=0
until curl -s -o /dev/null "http://127.0.0.1:$PORT/"; do
  i=$((i + 1))
  if [ "$i" -gt 120 ]; then
    echo "the Playwright server in $IMAGE did not start" >&2
    exit 1
  fi
  sleep 1
done
PW_VISUAL_WS="ws://127.0.0.1:$PORT/" npx playwright test -c playwright.visual.config.ts "$@"
