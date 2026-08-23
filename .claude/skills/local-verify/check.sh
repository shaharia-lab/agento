#!/usr/bin/env bash
# The pre-push gates, run so they cannot take the machine down with them.
#
#   check.sh                 fmt + clippy + frontend typecheck/build   (no test linking)
#   check.sh --tests         ...and every Rust test binary, ONE AT A TIME
#   check.sh --test <name>   ...and just that one test binary
#
# Why this exists rather than `cargo test`:
#
#   src-tauri/target/debug/libagento_lib.a   1.2 GB
#   src-tauri/target/debug/agento             429 MB
#   src-tauri/tests/*.rs                      8 integration binaries
#
# Each test binary links the whole of that static lib, and `cargo test` links
# them concurrently — on an 8-core, 16 GB machine that is eight multi-gigabyte
# link jobs at once. The machine swaps and stops responding. Nothing about the
# tests is at fault; it is the linker parallelism, so the fix is to bound it
# rather than to skip the tests.
#
# `cargo fmt`, `cargo clippy` and the frontend build do not link, which is why
# the default mode is safe to run as often as you like.
set -u

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT" || exit 1

CORES="$(nproc 2>/dev/null || echo 4)"
JOBS="${CARGO_JOBS:-$(( CORES / 2 ))}"
[ "$JOBS" -lt 2 ] && JOBS=2
[ "$JOBS" -gt 4 ] && JOBS=4

fail=0
step() { echo; echo "==> $*"; }
run()  { "$@" || { echo "FAILED: $*"; fail=1; }; }

step "frontend: typecheck + build"
run npm run build

step "rust: formatting"
( cd src-tauri && run cargo fmt --check )

step "rust: lints (compiles test targets, does not link them)"
( cd src-tauri && run cargo clippy --all-targets -- -D warnings )

case "${1:-}" in
  --tests)
    step "rust: test binaries, one at a time (-j $JOBS)"
    ( cd src-tauri && run cargo test --lib -j "$JOBS" -- --test-threads=4 )
    for t in src-tauri/tests/*.rs; do
      name="$(basename "$t" .rs)"
      step "rust: --test $name"
      ( cd src-tauri && run cargo test --test "$name" -j "$JOBS" -- --test-threads=4 )
    done
    ;;
  --test)
    name="${2:?--test needs a name}"
    step "rust: --test $name (-j $JOBS)"
    ( cd src-tauri && run cargo test --test "$name" -j "$JOBS" -- --test-threads=4 )
    ;;
esac

echo
[ "$fail" -eq 0 ] && echo "ALL GATES PASSED" || echo "SOME GATES FAILED"
exit "$fail"
