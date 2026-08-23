#!/usr/bin/env bash
# The scanner decodes a project dir name against directories that exist
# (decode_project_path), so the synthetic projects need empty dirs under
# ~/Projects for the shoot. `make_dirs.sh` creates the missing ones and records
# them in $DEMO_HOME/.created_dirs; `make_dirs.sh --clean` removes exactly those.
set -u
LIST="${DEMO_HOME:-$HOME/.agento-demo}/.created_dirs"
P="auth billing handbook gateway mobile-api notifications payments platform search storefront"
if [ "${1:-}" = "--clean" ]; then
  [ -f "$LIST" ] && while read -r d; do rmdir "$HOME/Projects/$d" 2>/dev/null && echo "removed ~/Projects/$d"; done < "$LIST"
  rm -f "$LIST"; exit 0
fi
for d in $P; do
  if [ -e "$HOME/Projects/$d" ]; then echo "exists, leaving alone: ~/Projects/$d"; else mkdir -p "$HOME/Projects/$d" && echo "$d" >> "$LIST"; fi
done
