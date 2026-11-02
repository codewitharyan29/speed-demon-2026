#!/bin/sh
# usage: acheck.sh <bin>... : exact comparison with the independent reference on all 13 datasets
for b in "$@"; do ok=0; n=0; for d in tiny s2 full-1 hostile a-stress-7 a-stress-8 a-stress-9-max a-stress-10-sent a-stress-11-sentm1 a-stress-12-edge a-stress-13-uniform a-stress-14-zero a-stress-15-half; do rm -rf /tmp/ao; mkdir -p /tmp/ao; /home/user/bins/$b --data /home/user/adata/$d --out /tmp/ao 2>/dev/null; n=$((n+1)); if cmp -s /tmp/ao/summary.txt /home/user/adata/$d.ref; then ok=$((ok+1)); else echo "FAIL $b $d"; fi; done; echo "$b: $ok/$n datasets identical"; done
