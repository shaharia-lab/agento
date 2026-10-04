#!/usr/bin/env bash
# Tests for announce-release.sh. Offline: the release comes from a fixture file
# and `curl` is a stub on PATH that records its arguments and answers whatever
# status the test asks for, so nothing here can reach Discord.
#
# Run from anywhere: `.github/scripts/announce-release.test.sh`. CI runs it on
# every PR, because the script's only other execution is a real release.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/announce-release.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

URL="https://github.com/shaharia-lab/agento/releases/tag/v9.9.9"
WEBHOOK="https://discord.invalid/api/webhooks/123/s3cr3t-token"

failures=0
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}
check() {
  # check <name> <jq filter that must be true> <json file>
  if jq -e "$2" "$3" >/dev/null; then
    echo "ok: $1"
  else
    fail "$1"
  fi
}

fixture() {
  # fixture <file> <body>
  jq -n --arg body "$2" --arg url "$URL" \
    '{name: "v9.9.9", body: $body, url: $url, publishedAt: "2026-10-04T12:00:00Z"}' >"$1"
}

# The stub: logs every argument, copies the payload it was handed, writes a
# response body and prints the status the test chose, exactly as `-w` would.
mkdir -p "$work/bin"
cat >"$work/bin/curl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" >>"$STUB_LOG"
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    --data) cp "${2#@}" "$STUB_PAYLOAD"; shift ;;
  esac
  shift
done
[ -z "$out" ] || printf '%s' "${STUB_BODY:-}" >"$out"
[ -z "${STUB_STDERR:-}" ] || printf '%s\n' "$STUB_STDERR" >&2
printf '%s' "${STUB_STATUS:-204}"
exit "${STUB_EXIT:-0}"
STUB
chmod +x "$work/bin/curl"
export STUB_LOG="$work/curl.log" STUB_PAYLOAD="$work/sent.json"
PATH="$work/bin:$PATH"

# ── a short body is passed through whole ─────────────────────────────────────
fixture "$work/short.json" $'## What changed\r\n\r\n- one\r\n- two @everyone\r\n'
"$script" --dry-run --release-json "$work/short.json" v9.9.9 >"$work/short.out"
check "title is the project and the tag" '.embeds[0].title == "Agento v9.9.9"' "$work/short.out"
check "the embed url is the release url" ".embeds[0].url == \"$URL\"" "$work/short.out"
check "a short body is the whole description, CRLF normalised" \
  '.embeds[0].description == "## What changed\n\n- one\n- two @everyone"' "$work/short.out"
check "mentions are disabled" '.allowed_mentions == {parse: []}' "$work/short.out"
check "the timestamp is the publication time" \
  '.embeds[0].timestamp == "2026-10-04T12:00:00Z"' "$work/short.out"
check "exactly one embed" '.embeds | length == 1' "$work/short.out"

# ── a long body is cut on a line break and ends with the link ────────────────
long="$(for i in $(seq 1 400); do echo "- change number $i touches something — worth a line"; done)"
fixture "$work/long.json" "$long"
"$script" --dry-run --release-json "$work/long.json" v9.9.9 >"$work/long.out"
check "a long description fits Discord's limit" \
  '.embeds[0].description | length <= 4096' "$work/long.out"
check "a long description ends with the full-notes link" \
  ".embeds[0].description | endswith(\"\n\n[Read the full notes]($URL)\")" "$work/long.out"
check "the cut is on a line break: every kept line is a whole line" \
  '.embeds[0].description | split("\n\n[Read the full notes]")[0] | split("\n")
   | all(test("^- change number [0-9]+ touches something — worth a line$"))' "$work/long.out"
check "the kept notes stay within the cut limit" \
  '.embeds[0].description | split("\n\n[Read the full notes]")[0] | length <= 3500' "$work/long.out"
check "the cut keeps as much as fits" \
  '.embeds[0].description | split("\n\n[Read the full notes]")[0] | length > 3400' "$work/long.out"

# ── a body with no line break at all is still cut ────────────────────────────
fixture "$work/oneline.json" "$(printf 'x%.0s' $(seq 1 10000))"
"$script" --dry-run --release-json "$work/oneline.json" v9.9.9 >"$work/oneline.out"
check "a single 10,000-character line fits the limit" \
  '.embeds[0].description | length <= 4096' "$work/oneline.out"
check "and still carries the link" \
  '.embeds[0].description | endswith(")")' "$work/oneline.out"

# ── an empty body sends no description rather than an empty one ──────────────
fixture "$work/empty.json" ""
"$script" --dry-run --release-json "$work/empty.json" v9.9.9 >"$work/empty.out"
check "an empty body omits the description" '.embeds[0] | has("description") | not' "$work/empty.out"

# ── a dry run never calls curl ───────────────────────────────────────────────
if [ -e "$STUB_LOG" ]; then fail "a dry run called curl"; else echo "ok: a dry run never calls curl"; fi

# ── no webhook: a notice, exit 0, no curl ────────────────────────────────────
rc=0
out="$(env -u DISCORD_WEBHOOK "$script" --release-json "$work/short.json" v9.9.9 2>&1)" || rc=$?
if [ "$rc" = 0 ] && [[ "$out" == ::notice::* ]] && [ ! -e "$STUB_LOG" ]; then
  echo "ok: an unset webhook is a notice, exit 0, and no request"
else
  fail "an unset webhook must be a notice, exit 0, no request (rc=$rc, out=$out)"
fi
rc=0
out="$(DISCORD_WEBHOOK="" "$script" --release-json "$work/short.json" v9.9.9 2>&1)" || rc=$?
if [ "$rc" = 0 ] && [[ "$out" == ::notice::* ]] && [ ! -e "$STUB_LOG" ]; then
  echo "ok: an empty webhook is a notice, exit 0, and no request"
else
  fail "an empty webhook must be a notice, exit 0, no request (rc=$rc, out=$out)"
fi

# ── a 2xx answer: exit 0, one request, the payload sent is the dry-run one ───
rc=0
out="$(DISCORD_WEBHOOK="$WEBHOOK" STUB_STATUS=200 STUB_BODY='{"id":"1"}' \
  "$script" --release-json "$work/short.json" v9.9.9 2>&1)" || rc=$?
if [ "$rc" = 0 ] && [ "$(grep -c -x -- '--data' "$STUB_LOG")" = 1 ]; then
  echo "ok: a 200 exits 0 after exactly one request"
else
  fail "a 200 must exit 0 after one request (rc=$rc, out=$out)"
fi
if grep -q -x -- "${WEBHOOK}?wait=true" "$STUB_LOG"; then
  echo "ok: the request goes to the webhook with wait=true"
else
  fail "the request must go to the webhook with wait=true"
fi
if diff <(jq -S 'del(.embeds[0].timestamp)' "$work/sent.json") \
        <(jq -S 'del(.embeds[0].timestamp)' "$work/short.out") >/dev/null; then
  echo "ok: what is sent is what the dry run printed"
else
  fail "the sent payload differs from the dry run's"
fi
case "$out" in
  *"$WEBHOOK"*) fail "the webhook URL was printed on success" ;;
  *) echo "ok: the webhook URL is not printed on success" ;;
esac

# ── a refusal: exit 1, a warning naming the status, the URL never printed ────
rc=0
out="$(DISCORD_WEBHOOK="$WEBHOOK" STUB_STATUS=404 \
  STUB_BODY="{\"message\":\"Unknown Webhook\",\"echo\":\"$WEBHOOK\"}" \
  "$script" --release-json "$work/short.json" v9.9.9 2>&1)" || rc=$?
if [ "$rc" = 1 ] && [[ "$out" == "::warning::Discord answered 404"* ]]; then
  echo "ok: a 404 exits 1 with a warning naming the status"
else
  fail "a 404 must exit 1 with a warning naming the status (rc=$rc, out=$out)"
fi
case "$out" in
  *"Unknown Webhook"*) echo "ok: Discord's reason is printed" ;;
  *) fail "Discord's response body must be printed" ;;
esac
case "$out" in
  *"$WEBHOOK"* | *s3cr3t-token*) fail "the webhook URL leaked into the output of a refusal" ;;
  *) echo "ok: the webhook URL is redacted from a refusal" ;;
esac

# ── a network failure: exit 1, a warning, curl's message redacted ────────────
rc=0
out="$(DISCORD_WEBHOOK="$WEBHOOK" STUB_STATUS=000 STUB_EXIT=6 \
  STUB_STDERR="curl: (6) Could not resolve host for $WEBHOOK" \
  "$script" --release-json "$work/short.json" v9.9.9 2>&1)" || rc=$?
if [ "$rc" = 1 ] && [[ "$out" == "::warning::Discord could not be reached (curl exit 6"* ]]; then
  echo "ok: a network failure exits 1 with a warning naming curl's exit"
else
  fail "a network failure must exit 1 with a warning (rc=$rc, out=$out)"
fi
case "$out" in
  *s3cr3t-token*) fail "the webhook URL leaked into the output of a network failure" ;;
  *) echo "ok: the webhook URL is redacted from a network failure" ;;
esac

# ── no tracing flags, ever ───────────────────────────────────────────────────
if grep -n -E '^[^#]*(set -[a-z]*x|curl[^#]* (-v|--verbose|--trace))' "$script"; then
  fail "the script traces, which would print the webhook URL"
else
  echo "ok: no set -x and no curl -v"
fi

if [ "$failures" != 0 ]; then
  echo "${failures} failed"
  exit 1
fi
echo "all passed"
