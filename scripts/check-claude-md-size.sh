#!/usr/bin/env bash
# The repository holds exactly one CLAUDE.md, at the root, and it stays under
# LIMIT characters. This script enforces both rules.
#
# Size: Claude Code truncates a project instruction file past 150,000 characters
# and says so only with a one-line warning at session start. The root file
# reached 304k (#553), so the bottom third — Status, known gaps, known bugs —
# was never read.
#
# Location: the subsystem notes used to be a CLAUDE.md beside the code they
# describe, which Claude Code auto-loads when a file in that directory is read.
# That is the property #580 gave up: the notes now live in docs/internal/<area>.md
# and the root file's "Where the notes live" table is what sends a session to
# one. A stray nested CLAUDE.md would silently reintroduce a second instruction
# file that only some sessions see, so the guard fails on it by name.
#
# Run from the repository root. CI runs it on every PR; `npm run check:claude-md`
# runs it locally. Exit 1 names every offending file.
set -euo pipefail

LIMIT=40000
status=0

# Location rule first: a nested file is a rule violation regardless of its size.
while IFS= read -r file; do
  [ -n "$file" ] || continue
  echo "::error file=${file}::${file} is a nested CLAUDE.md; the repository has exactly one, at the root. Move its content to docs/internal/<area>.md and add a row to the root file's 'Where the notes live' table."
  status=1
done < <(git ls-files '*CLAUDE.md' | grep -v '^CLAUDE\.md$' || true)

while IFS= read -r file; do
  size=$(wc -c < "$file")
  if [ "$size" -gt "$LIMIT" ]; then
    echo "::error file=${file}::${file} is ${size} characters; the limit is ${LIMIT}. Move subsystem detail into that area's docs/internal/<area>.md (see the root file's 'Where the notes live')."
    status=1
  else
    printf '%7d  %s\n' "$size" "$file"
  fi
done < <(git ls-files 'CLAUDE.md')

exit $status
