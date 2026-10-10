#!/bin/bash
# Nightly safety net (fork only): push local main to origin before the 02:00 cloud routine, which
# only sees what is on GitHub. Fast-forward only: never forces, never pushes other branches
# (unfinished work stays local). Run by ~/Library/LaunchAgents/com.eliasx45.effectcraft.pushmain.plist.
set -u
cd "$(dirname "$0")/../.." || exit 1
echo "== $(date '+%F %T')"
git fetch -q origin main || { echo "fetch failed"; exit 1; }
ahead=$(git rev-list --count origin/main..main)
if [ "$ahead" = "0" ]; then echo "main already on GitHub"; exit 0; fi
if ! git merge-base --is-ancestor origin/main main; then
  echo "main and origin/main have diverged; not pushing (merge origin/main into main by hand)"; exit 1
fi
git push -q origin main && echo "pushed $ahead commit(s)"
