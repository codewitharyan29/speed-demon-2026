#!/bin/sh
for b in "$@"; do for d in tiny s2 full-1 $XTRA; do rm -rf /tmp/aout; mkdir -p /tmp/aout
  /home/user/bins/$b --data /home/user/adata/$d --out /tmp/aout
  if cmp -s /tmp/aout/summary.txt /home/user/adata/$d.ref; then echo "OK   $b $d"; else echo "FAIL $b $d"; fi; done; done
