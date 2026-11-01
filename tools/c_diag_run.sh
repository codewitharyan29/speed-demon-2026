#!/bin/sh
# usage: tools/c_diag_run.sh <patch names...>   -- for each patch in docs/track-c-board-diagnostics: apply it to track-c/src/main.rs, commit it
# labelled DIAGNOSTIC, push, wait for the board run, restore the source of the commit before, and log the result to .out/diag.log.
cd "$(dirname "$0")/.."
base=$(git rev-parse HEAD)
for p in "$@"; do
  git checkout -q $base -- track-c/src/main.rs
  patch -p1 < docs/track-c-board-diagnostics/$p.patch >/dev/null || { echo "$p: patch failed" >> .out/diag.log; continue; }
  git add track-c/src/main.rs
  git commit -q -m "DIAGNOSTIC (not a solution): Track C $p, to read a part of the cost from the board

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Hjs26WsNrtCqoeh9uTGGrs"
  git pull -q --rebase origin speed-demon; git push -q origin HEAD:speed-demon
  echo "$p $(python tools/board_c.py | tail -1)" >> .out/diag.log
  git checkout -q $base -- track-c/src/main.rs
  git commit -q -m "Track C: restore candidate B's source after the $p diagnostic

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Hjs26WsNrtCqoeh9uTGGrs"
done
git pull -q --rebase origin speed-demon; git push -q origin HEAD:speed-demon
echo "done $(git rev-parse --short HEAD)" >> .out/diag.log
