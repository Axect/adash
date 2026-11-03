#!/usr/bin/env bash
# Release gate: no personal data and no secrets in the tree or its history.
#
# 1. Deny-list grep. Words come from files that never enter the repository:
#    - $ADASH_SCRUB_DENYLIST (default: ~/.config/adash/scrub-denylist.txt), one string per line
#    - project and agent names of your own adash install (meta.yaml projects, registry names),
#      read at check time
#    Without any of them (e.g. in CI) this step is skipped.
# 2. gitleaks over the git history (and the working tree when there is no git repository).
#
# Exit 0 only when both report nothing.
set -euo pipefail
cd "$(dirname "$0")/.."

deny=$(mktemp)
trap 'rm -f "$deny"' EXIT

list=${ADASH_SCRUB_DENYLIST:-$HOME/.config/adash/scrub-denylist.txt}
[ -f "$list" ] && grep -v '^\s*#' "$list" | sed '/^\s*$/d' >>"$deny"

# names from a local adash install (never committed)
cfg=${XDG_CONFIG_HOME:-$HOME/.config}/adash/meta.yaml
state=${XDG_STATE_HOME:-$HOME/.local/state}/adash/registry.json
if command -v python3 >/dev/null; then
  python3 - "$cfg" "$state" >>"$deny" <<'PY' || true
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
if [ -s "$deny" ]; then
  n=$(sort -u "$deny" | wc -l)
  hits=$(git ls-files -z 2>/dev/null | xargs -0 grep -I -n -F -i -w -f <(sort -u "$deny") -- 2>/dev/null || true)
  if [ -z "$(git ls-files 2>/dev/null | head -1)" ]; then
    hits=$(grep -rIn -F -i -w -f <(sort -u "$deny") --exclude-dir=target --exclude-dir=.git . || true)
  fi
  if [ -n "$hits" ]; then
    echo "deny-list: $(printf '%s\n' "$hits" | wc -l) hit(s) ($n words)"
    printf '%s\n' "$hits" | cut -c1-200
    fail=1
  else
    echo "deny-list: 0 hits ($n words)"
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
