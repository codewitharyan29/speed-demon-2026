#!/bin/sh
# usage: tools/bench.sh <d|b> [runs]  -- wall time of the container on full seed-1 data, pinned to 12 CPUs.
# Data is read from the docker volume `sddata` (subpath d or b): a Windows bind mount makes mmap I/O 5x slower
# and hides the CPU work. Fill the volume once: docker run --rm -v sddata:/v -v <datadir>:/src:ro alpine sh -c "mkdir -p /v/d && cp /src/* /v/d/"
# Docker Desktop on Windows is a VM, so treat numbers as relative only; the board is the truth.
cd "$(dirname "$0")/.."
t=$1; n=${2:-5}; mkdir -p ".out/new-$t"
docker build -q --network=none -t sd-$t track-$t >/dev/null || exit 1
for i in $(seq $n); do
  s=$(date +%s%N)
  MSYS_NO_PATHCONV=1 docker run --rm --network=none --cpuset-cpus=0-11 --memory=8g -e TRACK=$t \
    --mount type=volume,src=sddata,dst=/data,volume-subpath=$t,readonly -v "$PWD/.out/new-$t:/out" sd-$t
  e=$(date +%s%N); echo "run $i: $(( (e - s) / 1000000 )) ms (includes container start)"
done
# Peak memory as the scorer sees it: the container cgroup's memory.peak after one run.
MSYS_NO_PATHCONV=1 docker run --rm --network=none --cpuset-cpus=0-11 --memory=8g -e TRACK=$t --entrypoint sh \
  --mount type=volume,src=sddata,dst=/data,volume-subpath=$t,readonly -v "$PWD/.out/new-$t:/out" sd-$t \
  -c '/app/solution; echo "memory.peak $(( $(cat /sys/fs/cgroup/memory.peak) / 1048576 )) MB"'
