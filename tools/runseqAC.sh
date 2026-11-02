#!/bin/sh
# usage: runseqAC.sh name:Afile:Cfile ... -- push Track A and Track C sources together, wait for the board runs of a and c, print one line
cd /home/user/wt-cpp
for spec in "$@"; do
  name=$(echo $spec | cut -d: -f1); af=$(echo $spec | cut -d: -f2); cf=$(echo $spec | cut -d: -f3)
  if [ ! -f "$af" ] || [ ! -f "$cf" ] || [ "$(echo $spec | tr -cd : | wc -c)" != 2 ]; then echo "BAD SPEC (nothing pushed): $spec"; continue; fi
  git fetch -q origin speed-demon; git checkout -q -B docs-c origin/speed-demon
  cp "$af" track-a/src/main.rs; cp "$cf" track-c/src/main.cpp
  sa=$(sha256sum track-a/src/main.rs | cut -c1-16); sc=$(sha256sum track-c/src/main.cpp | cut -c1-16)
  git add track-a/src/main.rs track-c/src/main.cpp
  git commit -q --allow-empty -m "Interleaved board comparison $name: Track A main.rs $sa, Track C main.cpp $sc

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Hjs26Y6kYtMXPENb6wkXvZ"
  c=$(git rev-parse HEAD); git push -q origin HEAD:speed-demon
  python3 /home/user/scripts/boardwait2.py bharat-labs-10030e $c 1500 | python3 -c "
import sys,json
l=sys.stdin.read().strip()
try:
  d=json.loads(l)
  f=lambda r: '%s T=%.3f M=%.2f S=%d K=%.3f %s' % (r['run_id'], r['T_s']*1e3, r['M_bytes']/1e6, r['S'], r['K'], 'ok' if all(r['gates'].values()) else 'GATE-FAIL')
  print('$name', 'A:$sa', 'C:$sc', d['a']['commit'][:7], d['a']['pushed_at'], '| A', f(d['a']), '| C', f(d['c']))
except Exception: print('$name', 'no result', l)
"
done
