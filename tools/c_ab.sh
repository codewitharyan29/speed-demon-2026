#!/bin/sh
# usage: tools/c_ab.sh <rounds> <binary in sdtarget-c>...   -- interleaved wall-time (median/min ms) on data/c-full-1 with all the VM's CPUs, plus
# memory.peak (fresh container each) of each binary. Local numbers only rank ideas: the VM has 4 MB of L3 and the board 24 MB.
cd "$(dirname "$0")/.."
rounds=$1; shift; names="$*"
MSYS_NO_PATHCONV=1 docker run --rm --network=none -e R=$rounds -e NAMES="$names" -v sdtarget-c:/target:ro -v sdcdata:/data:ro --tmpfs /out sd-bench bash -ec '
  : > /tmp/res
  for n in $NAMES; do /target/$n; done
  for i in $(seq $R); do for n in $NAMES; do
    s=$(date +%s%N); /target/$n; e=$(date +%s%N); echo "$n $(( (e-s)/10000 ))" >> /tmp/res
  done; done
  for n in $NAMES; do grep "^$n " /tmp/res | awk "{print \$2}" | sort -n | awk -v n=$n "{v[NR]=\$1} END{printf \"%-8s min %6.2f median %6.2f ms\n\", n, v[1]/100, v[int((NR+1)/2)]/100}"; done'
for n in $names; do
  MSYS_NO_PATHCONV=1 docker run --rm --network=none --memory=8g -v sdtarget-c:/target:ro -v sdcdata:/data:ro --tmpfs /out sd-bench sh -c "/target/$n; echo $n M=\$(( \$(cat /sys/fs/cgroup/memory.peak) / 1000 )) KB"
done
