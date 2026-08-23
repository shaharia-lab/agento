#!/usr/bin/env bash
# Populate session_insights for the demo corpus. The desktop build has no
# insight worker (see docs/screenshots/README.md), so this runs the Go server
# from the last desktop commit that still had one (3b54e41) against the demo
# data dir, restarting it until every session has a row (its queue is 100 per
# rescan). Run with the desktop app STOPPED — one writer at a time.
#   backfill.sh <path to agento-go binary> <demo HOME>
set -u
BIN="$1"; DEMO_HOME="$2"; DB="$DEMO_HOME/.agento-desktop-dev/agento.db"
for round in 1 2 3 4 5 6; do
  HOME="$DEMO_HOME" AGENTO_DATA_DIR="$DEMO_HOME/.agento-desktop-dev" AGENTO_SCHEDULER=off AGENTO_INTEGRATIONS=off \
    "$BIN" web --port 8995 --no-browser > /tmp/agento-go-backfill.log 2>&1 &
  pid=$!; sleep 25; kill $pid; wait $pid 2>/dev/null
  n=$(sqlite3 -readonly "$DB" "select count(*) from session_insights")
  want=$(sqlite3 -readonly "$DB" "select count(*) from claude_session_cache")
  echo "round $round: $n / $want"
  [ "$n" -ge "$want" ] && break
done
