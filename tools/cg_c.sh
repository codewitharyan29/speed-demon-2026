#!/bin/sh
# usage: tools/cg_c.sh [scale]   -- per-source-line instruction, branch-miss and cache-miss counts of track-c (valgrind cachegrind)
# on data/c-<scale>-1 (default sample), caches modelled as an i5-13500 P-core (48 KB L1d, 2 MB last level). The valgrind packages are
# installed from .out/valg (download valgrind_3.24.0-3 and libc6-dbg_2.41-12+deb13u4 from deb.debian.org; apt cannot verify
# signatures in this VM, dpkg does not need to). Then: python tools/cgtop.py .out/cg/cg.out track-c/src/main.rs 40
cd "$(dirname "$0")/.."
sc=${1:-sample}; mkdir -p .out/cg
MSYS_NO_PATHCONV=1 docker run --rm --network=none -v "$PWD/track-c:/w:ro" -v "$PWD/data/c-$sc-1:/data:ro" -v "$PWD/.out/cg:/out" -v "$PWD/.out/valg:/debs:ro" sd-bench sh -c '
  set -e
  dpkg -i /debs/valgrind_3.24.0-3_amd64.deb /debs/libc6-dbg_2.41-12+deb13u4_amd64.deb >/dev/null 2>&1
  cp -r /w /src && cd /src && rm -rf target
  CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release --offline 2>&1 | tail -1
  mkdir -p /tmp/o && sed -i "s|/out/aggregates|/tmp/o/aggregates|" src/main.rs 2>/dev/null || true
  valgrind --tool=cachegrind --cache-sim=yes --branch-sim=yes --D1=49152,12,64 --LL=2097152,16,64 --cachegrind-out-file=/out/cg.out ./target/release/solution 2>&1 | tail -12'
