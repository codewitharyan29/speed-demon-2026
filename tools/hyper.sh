#!/bin/sh
# usage: tools/hyper.sh <track> [runs]   (default track a)
# Builds track-<t> inside the pinned toolchain image (offline, same flags as the board: the track's .cargo/config),
# checks the output hash, then times the binary with hyperfine on the full seed-1 data from the docker volume `sddata`
# and reads the container cgroup's memory.peak for one run. The Docker VM is not the scoring box: compare variants
# against each other, never against the board.
# Env: CPUS (taskset list, default 0-7), THREADS (worker count patched into the benchmarked copy, default 8), BIN (extra binary to time, for A/B), EXPECT (sha256 of summary.txt).
set -e
cd "$(dirname "$0")/.."
t=${1:-a}; runs=${2:-40}; cpus=${CPUS:-0-7}
expect=${EXPECT:-495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3}
docker build -q -f tools/bench.Dockerfile -t sd-bench .out/debs >/dev/null
MSYS_NO_PATHCONV=1 docker run --rm --network=none --memory=8g --cpuset-cpus=$cpus -e CPUS=$cpus -e RUNS=$runs -e EXPECT=$expect -e THREADS=${THREADS:-8} -e BIN=$BIN \
  -v "$PWD/track-$t:/src:ro" -v sdtarget-$t:/target -v sdout:/out \
  --mount type=volume,src=sddata,dst=/data,volume-subpath=$t,readonly sd-bench sh -ec '
    rm -rf /w && cp -r /src /w && cd /w && rm -rf target
    sed -i "s/^const THREADS: usize = .*/const THREADS: usize = $THREADS;/" src/main.rs
    CARGO_TARGET_DIR=/target cargo build --release --offline >/tmp/build.log 2>&1 || { cat /tmp/build.log; exit 1; }
    bin=/target/release/solution
    rm -f /out/summary.txt; taskset -c $CPUS $bin --data /data --out /out
    got=$(sha256sum < /out/summary.txt | cut -d" " -f1)
    [ "$got" = "$EXPECT" ] && echo "HASH OK" || { echo "HASH MISMATCH $got"; exit 1; }
    hyperfine -N --warmup 5 --runs $RUNS "taskset -c $CPUS $bin --data /data --out /out" ${BIN:+"taskset -c $CPUS $BIN --data /data --out /out"}
    echo "cgroup memory.peak after build+runs (upper bound): $(( $(cat /sys/fs/cgroup/memory.peak) / 1048576 )) MB"'
# The real M: a fresh container that runs the final image once and reads memory.peak.
docker build -q --network=none -t sd-$t track-$t >/dev/null
MSYS_NO_PATHCONV=1 docker run --rm --network=none --memory=8g --cpuset-cpus=$cpus --entrypoint sh -v sdout:/out \
  --mount type=volume,src=sddata,dst=/data,volume-subpath=$t,readonly sd-$t \
  -c '/app/solution; echo "memory.peak (fresh container, one run) $(( $(cat /sys/fs/cgroup/memory.peak) )) bytes = $(( $(cat /sys/fs/cgroup/memory.peak) / 1048576 )) MB"'
