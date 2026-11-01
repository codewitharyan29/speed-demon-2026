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
  docker exec $C sh -c "mkdir -p /w && cd /repo/track-b && g++ -std=c++23 -O3 -march=native -fno-exceptions -fno-rtti -pthread -static -Wall -Wextra -Wno-unused-parameter ${SD_DEFS} $* src/main.cpp -o /w/solution" ;;
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
