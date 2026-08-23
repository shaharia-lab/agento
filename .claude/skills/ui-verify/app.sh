#!/usr/bin/env bash
# Bring up (or reuse) the dev app with the WebKit remote inspector open, so
# ui.mjs has something to drive. Idempotent: if the inspector is already
# answering, this exits immediately and costs nothing.
#
#   app.sh            ensure the app is running with the inspector
#   app.sh --x11      ...under XWayland, which is what native input/capture needs
#   app.sh --stop     stop the dev app and free :1420
#   app.sh --status   report without changing anything
#
# The launch is deliberately serialised and reused rather than done per test:
# a cold `tauri dev` links a ~430 MB binary, and doing that once per
# verification is what makes a machine swap.
set -u

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PORT="${INSPECTOR_PORT:-9224}"
LOG="${APP_LOG:-/tmp/agento-app.log}"

up() { ss -ltn 2>/dev/null | grep -q ":${PORT}\b"; }

# `pkill -f "tauri dev"` matches the shell running *this script* when the
# pattern appears in its own argv, killing the caller. The bracket keeps the
# pattern from matching itself.
stop() {
  pkill -9 -f "tauri[ ]dev"           2>/dev/null
  pkill -9 -f "target/debug/agento"   2>/dev/null
  sleep 2
  ss -lntp 2>/dev/null | grep -q ':1420' && { fuser -k 1420/tcp 2>/dev/null; sleep 2; }
  return 0
}

case "${1:-}" in
  --stop)
    stop; echo "stopped"; exit 0 ;;
  --status)
    up && echo "inspector UP on :${PORT}" || echo "inspector DOWN"
    ss -ltn 2>/dev/null | grep -E ':(1420|8991|'"${PORT}"')\b' || true
    exit 0 ;;
esac

X11=""
[ "${1:-}" = "--x11" ] && X11=1

if up && [ -z "$X11" ]; then
  echo "inspector already up on :${PORT} — reusing it"
  exit 0
fi

stop
cd "$ROOT" || exit 1

# A stale node_modules dies mid-launch with "imported but could not be
# resolved", ~20 lines after Vite has already announced itself as ready.
npm install --silent || exit 1

cat > /tmp/agento-launch.sh <<EOF
#!/usr/bin/env bash
cd "$ROOT"
${X11:+export GDK_BACKEND=x11}
export WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:${PORT}
exec npm run app:alongside
EOF
chmod +x /tmp/agento-launch.sh
setsid nohup /tmp/agento-launch.sh > "$LOG" 2>&1 < /dev/null &

echo "launching${X11:+ (XWayland)} — first build links a ~430 MB binary, so a cold start is minutes"
for _ in $(seq 1 120); do
  up && { echo "inspector up on :${PORT}"; exit 0; }
  sleep 5
done
echo "FAIL: inspector never came up. Last lines of $LOG:"
tail -5 "$LOG" | tr '\r' '\n' | tail -5
exit 1
