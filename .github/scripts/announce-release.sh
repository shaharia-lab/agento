#!/usr/bin/env bash
# Posts one release announcement to the Shaharia Lab Discord release channel.
# `release.yml`'s `announce` job runs it after `promote` has put the update
# manifest live, so the message never leads the release it links to.
#
#   announce-release.sh <tag>              post the announcement
#   announce-release.sh --dry-run <tag>    print the payload, send nothing
#   announce-release.sh --release-json <file> ...
#       read the release from a file holding what
#       `gh release view --json name,body,url,publishedAt` prints, instead of
#       asking GitHub. That is what makes the truncation testable offline; see
#       announce-release.test.sh.
#
# Environment:
#   DISCORD_WEBHOOK    the webhook URL. Empty or unset is not an error: a fork
#                      has no such secret, so the script says so and exits 0.
#   GH_TOKEN           for `gh release view`.
#   GITHUB_REPOSITORY  owner/repo, as Actions sets it.
#
# Three rules this file exists to keep:
#
# The webhook URL is a credential, and it reaches this script through the
# environment only. Nothing here traces (`set -x`, `curl -v`), and what curl and
# Discord say back is printed with the URL replaced, because GitHub's own log
# masking only covers the secret's exact spelling.
#
# Release notes are text somebody typed. They travel `gh` -> `jq --arg` -> a
# file and are never interpolated into shell or into JSON by hand, and the
# payload disables every mention, so an `@everyone` in the notes pings nobody.
#
# A failure exits 1 with a `::warning::`, not an `::error::`. The job is
# `continue-on-error`, the release is already live when this runs, and a red
# annotation on a green run would read as a broken release.
set -euo pipefail

# Discord refuses an embed description past 4096 characters. The notes are cut
# well short of that so the "Read the full notes" link always fits after them.
MAX_DESCRIPTION=3500

usage() {
  echo "usage: announce-release.sh [--dry-run] [--release-json <file>] <tag>" >&2
  exit 2
}

dry_run=0
release_json=""
tag=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --release-json)
      [ $# -ge 2 ] || usage
      release_json="$2"
      shift
      ;;
    -*) usage ;;
    *)
      [ -z "$tag" ] || usage
      tag="$1"
      ;;
  esac
  shift
done
[ -n "$tag" ] || usage

webhook="${DISCORD_WEBHOOK:-}"
if [ "$dry_run" = 0 ] && [ -z "$webhook" ]; then
  echo "::notice::DISCORD_WEBHOOK is not set, so ${tag} is not announced in Discord."
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ -n "$release_json" ]; then
  cp "$release_json" "$work/release.json"
else
  gh release view "$tag" --repo "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}" \
    --json name,body,url,publishedAt >"$work/release.json"
fi

# The cut is made on a line break so a Markdown link or a list item is never
# left half-written. `split`/`join` rather than `rindex`, which answers a byte
# offset for non-ASCII text in the jq the runners carry.
jq --arg tag "$tag" --argjson max "$MAX_DESCRIPTION" '
  def clip($max):
    if length <= $max then {text: ., cut: false}
    else
      (.[0:$max + 1] | split("\n")) as $lines
      | (if ($lines | length) > 1 then $lines[0:-1] | join("\n") else "" end) as $whole
      | ($whole | sub("\\s+$"; "")) as $kept
      | {text: (if $kept == "" then .[0:$max] else $kept end), cut: true}
    end;

  .url as $url
  | ((.body // "") | gsub("\r\n"; "\n") | sub("^\\s+"; "") | sub("\\s+$"; "") | clip($max)) as $notes
  | (if $notes.cut
     then $notes.text + "\n\n[Read the full notes](" + $url + ")"
     else $notes.text
     end) as $description
  | {
      username: "Agento",
      embeds: [
        ({
          title: ("Agento " + $tag),
          url: $url,
          description: $description,
          timestamp: (.publishedAt // (now | todate))
        } | if .description == "" then del(.description) else . end)
      ],
      allowed_mentions: {parse: []}
    }
' "$work/release.json" >"$work/payload.json"

if [ "$dry_run" = 1 ]; then
  cat "$work/payload.json"
  exit 0
fi

# `wait=true` makes Discord answer with the stored message instead of an empty
# 204, so a rejection carries its reason in the body.
case "$webhook" in
  *\?*) target="${webhook}&wait=true" ;;
  *) target="${webhook}?wait=true" ;;
esac

redact() {
  local text
  text="$(cat)"
  printf '%s\n' "${text//"$webhook"/<webhook>}"
}

curl_exit=0
status="$(curl -sS -o "$work/response.txt" -w '%{http_code}' \
  --max-time 20 --retry 2 \
  -H 'Content-Type: application/json' \
  --data @"$work/payload.json" \
  "$target" 2>"$work/curl-error.txt")" || curl_exit=$?

case "$status" in
  2??)
    echo "announced ${tag} in Discord (HTTP ${status})"
    exit 0
    ;;
esac

if [ "$curl_exit" != 0 ]; then
  echo "::warning::Discord could not be reached (curl exit ${curl_exit}, HTTP ${status:-000}); ${tag} was not announced."
  redact <"$work/curl-error.txt"
else
  echo "::warning::Discord answered ${status}; ${tag} was not announced."
fi
[ ! -s "$work/response.txt" ] || redact <"$work/response.txt"
exit 1
