#!/usr/bin/env bash
# Release gate: no personal data and no secrets in the tree or its history.
#
# 1. Deny-list grep. Words come from files that never enter the repository:
#    - $ADASH_SCRUB_DENYLIST (default: ~/.config/adash/scrub-denylist.txt), one string per line,
#      matched as whole words in any case
#    - project and agent names of your own adash install (meta.yaml projects, registry names),
#      read at check time, matched as whole words in their exact case (an agent named BUILD does
#      not flag the word "build")
#    Without any of them (e.g. in CI) this step is skipped.
#    Scope: only what is not public yet, i.e. lines added since the upstream branch (commits not
#    pushed, staged and unstaged edits) and untracked files. Lines already on the remote cannot
#    be taken back by blocking a push, so they are not checked again. `--full` (or no upstream)
#    checks every tracked file instead.
# 2. gitleaks over the git history (and the working tree when there is no git repository).
#
# Exit 0 only when both report nothing.
set -euo pipefail
cd "$(dirname "$0")/.."

full=0
[ "${1:-}" = "--full" ] && full=1
deny=$(mktemp)
names=$(mktemp)
scope=$(mktemp)
trap 'rm -f "$deny" "$names" "$scope"' EXIT

list=${ADASH_SCRUB_DENYLIST:-$HOME/.config/adash/scrub-denylist.txt}
[ -f "$list" ] && grep -v '^\s*#' "$list" | sed '/^\s*$/d' >>"$deny"

# names from a local adash install (never committed)
cfg=${XDG_CONFIG_HOME:-$HOME/.config}/adash/meta.yaml
state=${XDG_STATE_HOME:-$HOME/.local/state}/adash/registry.json
if command -v python3 >/dev/null; then
  python3 - "$cfg" "$state" >>"$names" <<'PY' || true
import json, sys, os
names = set()
cfg, state = sys.argv[1], sys.argv[2]
if os.path.exists(cfg):
    try:
        import yaml
        m = yaml.safe_load(open(cfg)) or {}
        names |= set((m.get("projects") or {}).keys())
    except Exception:
        pass
if os.path.exists(state):
    r = json.load(open(state))
    for k in ("overrides",):
        names |= {v for v in (r.get(k) or {}).values() if isinstance(v, str)}
    for k in ("self_names", "selfNames"):
        names |= {v.get("name") for v in (r.get(k) or {}).values() if isinstance(v, dict) and v.get("name")}
# short or generic names would match ordinary words (and the demo uses some on purpose)
generic = {"draft", "mail", "proof", "proposer", "planner", "critic", "smoke", "signature", "skills", "review", "reviewer", "test"}
for n in sorted(names):
    if len(n) >= 4 and not n.isdigit() and n.lower() not in generic:
        print(n)
PY
fi

fail=0
# what to look at: "path:line:text" rows
base=""
if [ $full -eq 0 ] && git rev-parse --git-dir >/dev/null 2>&1; then
  base=$(git merge-base HEAD '@{upstream}' 2>/dev/null || true)
fi
if [ -n "$base" ]; then
  what="changes since ${base:0:7}"
  # added lines of committed + staged + unstaged changes, with their new line numbers
  git diff -U0 --no-color "$base" -- . | awk '
    /^\+\+\+ b\// { f = substr($0, 7); next }
    /^@@/ { split($3, a, ","); n = substr(a[1], 2) + 0; next }
    /^\+/ && f != "" { print f ":" n ":" substr($0, 2); n++ }' >"$scope"
  git ls-files -z --others --exclude-standard | xargs -0 -r grep -I -n -H "" -- >>"$scope" 2>/dev/null || true
elif git rev-parse --git-dir >/dev/null 2>&1; then
  what="all tracked files"
  git ls-files -z | xargs -0 grep -I -n -H "" -- >"$scope" 2>/dev/null || true
else
  what="all files"
  grep -rIn -H "" --exclude-dir=target --exclude-dir=.git . >"$scope" || true
fi
if [ -s "$deny" ] || [ -s "$names" ]; then
  n=$(cat "$deny" "$names" | sort -u | wc -l)
  hits=""
  # match the text after "path:line:" only
  text() { cut -d: -f3- "$scope"; }
  if [ -s "$deny" ]; then hits=$(grep -n -F -i -w -f <(sort -u "$deny") <(text) || true); fi
  if [ -s "$names" ]; then hits=$(printf '%s\n%s' "$hits" "$(grep -n -F -w -f <(sort -u "$names") <(text) || true)" | sed '/^$/d'); fi
  hits=$(printf '%s\n' "$hits" | sed '/^$/d' | cut -d: -f1 | sort -un | while read -r i; do sed -n "${i}p" "$scope"; done)
  if [ -n "$hits" ]; then
    echo "deny-list: $(printf '%s\n' "$hits" | wc -l) hit(s) ($n words, $what)"
    printf '%s\n' "$hits" | cut -c1-200
    fail=1
  else
    echo "deny-list: 0 hits ($n words, $what)"
  fi
else
  echo "deny-list: skipped (no list and no local adash install)"
fi

gl=$(command -v gitleaks || true)
if [ -z "$gl" ]; then
  echo "gitleaks: not installed" >&2
  exit 2
fi
if git rev-parse --git-dir >/dev/null 2>&1; then
  "$gl" git --no-banner --redact . || fail=1
else
  "$gl" dir --no-banner --redact . || fail=1
fi
exit $fail
