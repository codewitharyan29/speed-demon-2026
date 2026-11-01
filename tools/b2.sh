#!/bin/sh
# Track B (C++) dev loop inside the long-lived container sd-b2-dev (gcc:15, data volume at /sd, repo at /repo).
# usage: tools/b2.sh build [extra g++ flags]   -> /w/solution
#        tools/b2.sh check <dataset>           -> run and compare with ref.txt (dataset: b-tiny-1, b-sample-2, b-full-1 ...)
#        tools/b2.sh time <dataset> [runs]     -> wall time of the solution (no Docker start cost)
export MSYS_NO_PATHCONV=1
C=${SD_CONT:-sd-b2-dev}
cmd=$1; shift
case $cmd in
build)
  docker exec $C sh -c "mkdir -p /w && cd /repo/track-b && g++ -std=c++23 -O3 -march=native -fno-exceptions -fno-rtti -pthread -static -Wall -Wextra -Wno-unused-parameter ${SD_DEFS} $* ${SD_SRC:-src/main.cpp} -o /w/${SD_OUT:-solution}" ;;
ab)
  # usage: tools/b2.sh ab <dataset> <binA> <binB> [n]  -- alternating runs (binaries in /w), min and median ms
  d=$1; a=$2; b=$3; n=${4:-11}
  docker exec $C sh -c "
    case $d in b-full-1) src=/sd/b ;; *) src=/repo/data/$d ;; esac
    rm -rf /data /out; mkdir -p /out; ln -sfn \$src /data
    for i in \$(seq $n); do for x in $a $b; do s=\$(date +%s%N); /w/\$x >/dev/null 2>&1; e=\$(date +%s%N); echo \$x \$(( (e - s) / 10000 )); done; done |
      python3 -c \"
import sys,collections
d=collections.defaultdict(list)
for l in sys.stdin:
    k,v=l.split(); d[k].append(int(v)/100)
for k,v in d.items():
    v.sort(); print(k,'min %.1f median %.1f max %.1f ms'%(v[0],v[len(v)//2],v[-1]))\"" ;;
check|time)
  d=$1; n=${2:-1}
  docker exec $C sh -c "
    case $d in b-full-1) src=/sd/b ;; *) src=/repo/data/$d ;; esac
    rm -rf /data /out; mkdir -p /out; ln -sfn \$src /data
    ref=/repo/data/$d/ref.txt
    if [ $cmd = check ]; then
      /w/solution && cmp \$ref /out/results.txt && echo OK $d \$(sha256sum < /out/results.txt | cut -c1-16)
    else
      for i in \$(seq $n); do s=\$(date +%s%N); /w/solution; e=\$(date +%s%N); echo run \$i: \$(( (e - s) / 1000000 )) ms; done
    fi" ;;
esac
