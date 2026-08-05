#!/usr/bin/env bash
# One worktree per working session (human or agent). Idempotent: if the
# worktree already exists it just tells you where.
#
# WHY: several sessions on ONE working tree collide in two ways, and both are
# expensive:
#
#   1. Edits. Two agents writing the same files without coordination lose work
#      silently. This is the serious risk.
#   2. Compilation. Cargo takes an EXCLUSIVE lock on `target/`, so sessions
#      serialize waiting for each other — and if they invoke cargo with
#      different flags they rebuild artifacts that were already good, in a
#      loop, in both directions.
#
# A worktree fixes both: its own tree (git isolates the edits) and its own
# `target/` for free — cargo resolves it from the CWD, and `/target` is already
# in .gitignore — with no CARGO_TARGET_DIR to set.
#
# COST, and it is not small: that own `target/` is another full artifact
# universe, ~30 GB once warm, plus one cold build (~250s). Cargo never
# garbage-collects, so a worktree nobody has opened in a month is still 30 GB.
# `just disk` lists the targets of every worktree; `git worktree remove` gives
# one back whole. Reach for this when sessions genuinely overlap — not by
# default.
#
# Usage:  scripts/wt.sh <name>          # e.g.  scripts/wt.sh m3-5
#         scripts/wt.sh <name> <base>   # base defaults to main
set -euo pipefail

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok() { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }

name="${1:-}"
base="${2:-main}"
if [[ -z "$name" ]]; then
  echo "usage: scripts/wt.sh <name> [base-branch]" >&2
  echo "       e.g.  scripts/wt.sh m3-5     # worktree ../norte-wt-m3-5, branch wt/m3-5" >&2
  exit 1
fi

# The name becomes both a branch and a path: no shell or git surprises.
if [[ ! "$name" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "ERROR: invalid name '$name' (allowed: [A-Za-z0-9._-])" >&2
  exit 1
fi

root="$(git rev-parse --show-toplevel)"
# Inside a worktree, --show-toplevel returns THAT worktree; the main repo is
# the one that decides where the siblings hang from.
main_root="$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')"
dest="$(dirname "$main_root")/norte-wt-$name"
branch="wt/$name"

if [[ -e "$dest" ]]; then
  ok "already there: $dest"
  echo "   cd $dest"
  exit 0
fi

info "worktree '$name' from '$base'"
git -C "$main_root" worktree add -b "$branch" "$dest" "$base"

ok "ready: $dest (branch $branch)"
cat <<EOF

   cd $dest

   Its target/ is its own: it no longer competes with the other sessions.
   The first build is cold; every one after that is yours alone.

   When you are done — do this, do not leave it lying around:
     git -C "$main_root" worktree remove $dest
     git -C "$main_root" branch -d $branch
EOF
