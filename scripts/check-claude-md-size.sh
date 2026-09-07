#!/usr/bin/env bash
# Every CLAUDE.md in the repository must stay under LIMIT characters.
#
# Claude Code truncates a project instruction file past 150,000 characters and
# says so only with a one-line warning at session start. The root file reached
# 304k (#553), so the bottom third — Status, known gaps, known bugs — was never
# read. The notes are now split into one CLAUDE.md per subsystem, loaded only
# when a file in that directory is read, and this guard is what keeps any one of
# them from growing back into the state it recovered from.
#
# Run from the repository root. CI runs it on every PR; `npm run check:claude-md`
# runs it locally. Exit 1 names every file over the limit.
set -euo pipefail

LIMIT=40000
status=0

while IFS= read -r file; do
  size=$(wc -c < "$file")
  if [ "$size" -gt "$LIMIT" ]; then
    echo "::error file=${file}::${file} is ${size} characters; the limit is ${LIMIT}. Move subsystem detail into the CLAUDE.md beside that code (see the root file's 'Where the notes live')."
    status=1
  else
    printf '%7d  %s\n' "$size" "$file"
  fi
done < <(git ls-files '*CLAUDE.md' 'CLAUDE.md')

exit $status
