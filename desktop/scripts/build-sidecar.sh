#!/usr/bin/env bash
#
# Build the Agento Go server as a Tauri sidecar.
#
# Tauri resolves sidecars by filename: `<name>-<rust-target-triple>` (plus .exe
# on Windows). Getting that name wrong fails at bundle time with a confusing
# "binary not found", so the mapping lives here rather than in six CI steps.
#
# The Go server is pure Go — modernc.org/sqlite, not the CGO sqlite3 driver —
# so CGO_ENABLED=0 cross-compiles every target from any host. That is what
# makes a single build machine able to produce sidecars for all platforms,
# even though Tauri itself still has to bundle on the target OS.
#
# Usage:
#   scripts/build-sidecar.sh                        # host target
#   scripts/build-sidecar.sh aarch64-apple-darwin   # explicit target
#   AGENTO_SRC=/path/to/agento scripts/build-sidecar.sh

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$HERE")"
OUT="$ROOT/src-tauri/binaries"
NAME="agento-server"

# The desktop app lives at desktop/ inside the agento repo, so the Go module is
# the parent. The sibling layout is still accepted for a standalone checkout.
find_src() {
  if [[ -n "${AGENTO_SRC:-}" ]]; then echo "$AGENTO_SRC"; return; fi
  for candidate in "$ROOT/.." "$ROOT/../agento"; do
    if [[ -f "$candidate/go.mod" ]]; then (cd "$candidate" && pwd); return; fi
  done
}

SRC="$(find_src)"

if [[ -z "$SRC" || ! -f "$SRC/go.mod" ]]; then
  echo "error: Agento Go source not found." >&2
  echo "       Expected go.mod in the parent directory, or set AGENTO_SRC." >&2
  exit 1
fi

TARGET="${1:-$(rustc -vV | sed -n 's/^host: //p')}"

case "$TARGET" in
  x86_64-unknown-linux-gnu)   GOOS=linux   GOARCH=amd64 ;;
  aarch64-unknown-linux-gnu)  GOOS=linux   GOARCH=arm64 ;;
  x86_64-apple-darwin)        GOOS=darwin  GOARCH=amd64 ;;
  aarch64-apple-darwin)       GOOS=darwin  GOARCH=arm64 ;;
  x86_64-pc-windows-msvc)     GOOS=windows GOARCH=amd64 ;;
  aarch64-pc-windows-msvc)    GOOS=windows GOARCH=arm64 ;;
  *) echo "error: unsupported target triple '$TARGET'" >&2; exit 1 ;;
esac

EXT=""
[[ "$GOOS" == "windows" ]] && EXT=".exe"

DEST="$OUT/${NAME}-${TARGET}${EXT}"
mkdir -p "$OUT"

echo "building $GOOS/$GOARCH -> $(basename "$DEST")"

# -s -w strips the symbol table and DWARF; the sidecar is shipped, not debugged,
# and it is the single largest thing in the bundle.
(
  cd "$SRC"
  CGO_ENABLED=0 GOOS="$GOOS" GOARCH="$GOARCH" \
    go build -trimpath -ldflags "-s -w" -o "$DEST" .
)

chmod +x "$DEST" 2>/dev/null || true
echo "done: $DEST ($(du -h "$DEST" | cut -f1))"
