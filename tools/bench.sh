#!/bin/sh
# usage: tools/bench.sh <d|b> <scale> <seed> [runs]  -- wall time + peak RSS of the container, pinned to 12 CPUs
# (Docker Desktop on Windows is a VM, so treat numbers as relative only; the board is the truth.)
cd "$(dirname "$0")/.."
t=$1; d="data/$t-$2-$3"; n=${4:-5}; mkdir -p ".out/new-$t"
for i in $(seq $n); do
  s=$(date +%s%N)
  MSYS_NO_PATHCONV=1 docker run --rm --network=none --cpuset-cpus=0-11 --memory=8g -e TRACK=$t -v "$PWD/$d:/data:ro" -v "$PWD/.out/new-$t:/out" sd-$t
  e=$(date +%s%N); echo "run $i: $(( (e - s) / 1000000 )) ms (includes container start)"
done
