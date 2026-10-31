#!/bin/sh
# usage: tools/ab.sh <pairs> <name> <name> [<name>...]   -- interleaved A/B/... timing of binaries in the sdtarget-a volume
# (build them with tools/abbuild.sh <git-ref> <name> or tools/variant.sh <name> '<sed script>'). Every round runs each binary once,
# in order, on a fresh output file (rm first: overwriting an existing file costs a flush at close). Prints min / median / mean of
# wall, user and sys ms per binary. Env: CPUS (default 0-7), OUT (output dir; default tmpfs /dev/shm/o, `OUT=/out` for the docker volume).
# The Docker VM inflates kernel time (pread, faults, madvise, writes) far more than CPU time: rank by user, confirm on the board.
cd "$(dirname "$0")/.."
t=${T:-a}; pairs=$1; shift; names="$*"; cpus=${CPUS:-0-7}; outdir=${OUT:-/dev/shm/o}
MSYS_NO_PATHCONV=1 docker run --rm --network=none --memory=8g --cpuset-cpus=$cpus -e PAIRS=$pairs -e NAMES="$names" -e CPUS=$cpus -e OUTDIR=$outdir \
  -v sdtarget-$t:/target -v sdout:/out --mount type=volume,src=sddata,dst=/data,volume-subpath=$t,readonly sd-bench bash -ec '
  mkdir -p $OUTDIR; TIMEFORMAT="%R %U %S"; : > /tmp/res
  for n in $NAMES; do rm -f $OUTDIR/summary.txt; taskset -c $CPUS /target/$n 2>/dev/null; done   # warm-up
  for i in $(seq $PAIRS); do for n in $NAMES; do
    rm -f $OUTDIR/summary.txt
    t=$( { time taskset -c $CPUS /target/$n >/dev/null 2>&1; } 2>&1 )
    echo "$n $t" >> /tmp/res
  done; done
  for n in $NAMES; do
    echo "$n $(sha256sum < $OUTDIR/summary.txt | cut -c1-12)"
    for col in 2 3 4; do
      grep "^$n " /tmp/res | awk -v c=$col "{printf \"%d\n\", \$c*1000}" | sort -n | awk -v c=$col -v n=$n "{v[NR]=\$1; s+=\$1} END{m=(NR%2)?v[(NR+1)/2]:(v[NR/2]+v[NR/2+1])/2; split(\"x wall user sys\",nm,\" \"); printf \"  %-5s min %7.1f  median %7.1f  mean %7.1f ms\n\", nm[c], v[1], m, s/NR}"
    done
  done'
