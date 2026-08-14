#!/usr/bin/env bash
#
# Start a Go server built from the CURRENT source, on a scratch copy of the
# user's database, for the Rust port to be diffed against.
#
# Why this exists: the "live instance" a developer happens to be running is
# whatever binary they installed, which drifts behind the repo. Diffing a port
# against it compares Rust to an old Go, and the failure mode is the worst kind
# — the diff comes out clean while the port is wrong, or dirty while the port is
# right. The bar is byte-identical to the Go source in *this* checkout, so that
# is what the diff has to ask.
#
# The database is copied rather than shared. The current source may carry
# migrations the installed server has never applied, and applying them to the
# real file would upgrade it underneath a running older instance.
#
#   ./scripts/parity-instance.sh start   # prints AGENTO_LIVE_URL / AGENTO_LIVE_DB
#   ./scripts/parity-instance.sh stop
#
# Typical use:
#   eval "$(./scripts/parity-instance.sh start)"
#   (cd src-tauri && cargo test --test parity_analytics -- --ignored --nocapture)
#   ./scripts/parity-instance.sh stop
#
# CONCURRENCY. Several ports run at once, so the scratch state is per checkout:
# the work dir defaults to a name derived from this script's own repo root, and
# `start` kills only the server recorded in *that* dir. Two agents in separate
# worktrees therefore need no coordination at all. Two agents sharing one
# checkout still would, so set AGENTO_PARITY_DIR (or AGENTO_PARITY_WORKER for
# just a suffix) to separate them. Every invocation echoes the dir it used, and
# `start` exports AGENTO_PARITY_DIR alongside the URL so a later `stop` in
# another shell finds the same instance even if the default changes.
#
# The port is already per instance — it is asked of the OS, not hardcoded — and
# recorded in the work dir, so `url` can report a running instance without
# restarting it.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# A stable fingerprint of the checkout, so one worktree's scratch database can
# never be clobbered by another's. cksum rather than md5sum/shasum: it is POSIX
# and present on both macOS and Linux, and this only has to be distinct, not
# cryptographic.
checkout_id() {
  printf '%s' "$REPO_ROOT" | cksum | cut -d' ' -f1
}

WORKER_SUFFIX="${AGENTO_PARITY_WORKER:+-$AGENTO_PARITY_WORKER}"
WORK_DIR="${AGENTO_PARITY_DIR:-${TMPDIR:-/tmp}/agento-parity-$(checkout_id)$WORKER_SUFFIX}"
DATA_DIR="$WORK_DIR/data"
BIN="$WORK_DIR/agento-parity"
PID_FILE="$WORK_DIR/server.pid"
PORT_FILE="$WORK_DIR/server.port"
LOG_FILE="$WORK_DIR/server.log"
SOURCE_DATA_DIR="${AGENTO_SOURCE_DATA_DIR:-$HOME/.agento}"

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

start() {
  stop >/dev/null 2>&1 || true
  mkdir -p "$DATA_DIR"
  echo "parity work dir: $WORK_DIR" >&2

  # Build from the checkout, not from PATH. The output lives in this instance's
  # own work dir, so a concurrent worker's build cannot replace the binary
  # underneath a running server.
  (cd "$REPO_ROOT" && go build -o "$BIN" .) >&2

  # A copy, so the scratch instance's migrations and scans cannot touch the
  # real database. The -wal and -shm carry writes the main file has not
  # checkpointed yet, so all three travel together or the copy loses data.
  for suffix in "" "-wal" "-shm"; do
    if [ -f "$SOURCE_DATA_DIR/agento.db$suffix" ]; then
      cp "$SOURCE_DATA_DIR/agento.db$suffix" "$DATA_DIR/agento.db$suffix"
    fi
  done

  local port
  port="$(free_port)"
  echo "$port" >"$PORT_FILE"

  AGENTO_DATA_DIR="$DATA_DIR" AGENTO_BIND=127.0.0.1 \
    "$BIN" web --port "$port" --no-browser >"$LOG_FILE" 2>&1 &
  echo $! >"$PID_FILE"

  for _ in $(seq 1 150); do
    if curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done
  if ! curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1; then
    echo "parity server did not become healthy; see $LOG_FILE" >&2
    exit 1
  fi

  # The first read starts a rescan, and rows changing underneath a diff would
  # make it flap. Wait it out before handing the URL over.
  curl -fsS -H 'Content-Type: application/json' \
    "http://127.0.0.1:$port/api/claude-sessions?limit=1" >/dev/null 2>&1 || true
  for _ in $(seq 1 600); do
    if curl -fsS -H 'Content-Type: application/json' \
      "http://127.0.0.1:$port/api/claude-sessions/status" 2>/dev/null |
      grep -q '"scan_in_progress":false'; then
      break
    fi
    sleep 1
  done

  # AGENTO_PARITY_DIR travels with the URL so a `stop` from another shell — or
  # after the default changes — targets this instance rather than guessing.
  echo "export AGENTO_LIVE_URL=http://127.0.0.1:$port"
  echo "export AGENTO_LIVE_DB=$DATA_DIR/agento.db"
  echo "export AGENTO_PARITY_DIR=$WORK_DIR"
}

# Report a running instance without disturbing it, for a shell that lost the
# exports. Silent and non-zero when nothing is running here.
url() {
  [ -f "$PORT_FILE" ] || return 1
  local port
  port="$(cat "$PORT_FILE")"
  curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1 || return 1
  echo "export AGENTO_LIVE_URL=http://127.0.0.1:$port"
  echo "export AGENTO_LIVE_DB=$DATA_DIR/agento.db"
  echo "export AGENTO_PARITY_DIR=$WORK_DIR"
}

# Only ever this work dir's server: a `pkill agento-parity` would take a
# concurrent worker's instance down with it.
stop() {
  if [ -f "$PID_FILE" ]; then
    kill "$(cat "$PID_FILE")" 2>/dev/null || true
    rm -f "$PID_FILE"
  fi
  rm -f "$PORT_FILE"
}

case "${1:-start}" in
start) start ;;
stop) stop ;;
url) url ;;
*)
  echo "usage: $0 [start|stop|url]" >&2
  exit 2
  ;;
esac
