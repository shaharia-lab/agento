#!/usr/bin/env bash
# Clear the three things that make `npm run app:alongside` fail with an error
# that does not name its own cause. Run from anywhere. Idempotent.
set -u

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "==> killing stale dev processes"
pkill -9 -f "tauri dev"              2>/dev/null
pkill -9 -f "node_modules/.bin/vite" 2>/dev/null
pkill -9 -f "target/debug/agento"    2>/dev/null
sleep 2

# `pkill -f vite` misses the node process that actually holds :1420 often
# enough that checking the port is the only reliable test.
if ss -lntp 2>/dev/null | grep -q ':1420'; then
  echo "==> :1420 still held, killing by port"
  fuser -k 1420/tcp 2>/dev/null
  sleep 2
fi
ss -lntp 2>/dev/null | grep -q ':1420' \
  && { echo "FAIL: :1420 still in use — tauri dev will die with"; \
       echo '      "The beforeDevCommand terminated with a non-zero status code."'; exit 1; } \
  || echo "    :1420 free"

echo "==> syncing node_modules (branches add deps; a stale tree dies"
echo "    mid-launch with 'imported but could not be resolved')"
( cd "$ROOT" && npm install --silent ) || exit 1

echo "==> preflight OK. Launch with:"
echo "    WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224 \\"
echo "      setsid nohup npm run app:alongside > /tmp/app.log 2>&1 < /dev/null &"
