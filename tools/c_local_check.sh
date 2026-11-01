#!/bin/sh
# Local helper (session 4): fixtures in /home/user/cdata/<name> (+ <name>.ref), binaries in /home/user/bins.
# usage: check.sh <name>...  runs each binary on each fixture, compares to the independent reference
for b in "$@"; do for d in tiny-1 sample-1 full-1 adv hostile $XTRA; do
  ln -sfn /home/user/cdata/$d /data; rm -f /out/aggregates.txt
  /home/user/bins/$b; if cmp -s /out/aggregates.txt /home/user/cdata/$d.ref; then echo "OK   $b $d"; else echo "FAIL $b $d"; fi
done; done
