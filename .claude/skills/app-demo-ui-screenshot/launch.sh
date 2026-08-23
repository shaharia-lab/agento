#!/usr/bin/env bash
# Launch the dev app against the demo HOME, isolated from ~/.agento and
# ~/.claude. Under XWayland so the window can be sized with xdotool.
#   launch.sh <demo HOME>
set -u
DEMO_HOME="$1"; REAL_HOME="$HOME"
cd "$(dirname "$0")/../../.."
export HOME="$DEMO_HOME"
export CARGO_HOME="$REAL_HOME/.cargo" RUSTUP_HOME="$REAL_HOME/.rustup"
export XDG_CONFIG_HOME="$REAL_HOME/.config" XDG_DATA_HOME="$REAL_HOME/.local/share" XDG_CACHE_HOME="$REAL_HOME/.cache"
export npm_config_cache="$REAL_HOME/.npm"
export WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224
export GDK_BACKEND=x11
exec npm run app:alongside
