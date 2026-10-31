#!/bin/sh
# usage: tools/cachegrind.sh <d|b> [scale]  -- instruction, branch and cache-miss profile per source line
# (valgrind cachegrind on data/<t>-<scale>-1, default sample; optimised build with debug info; caches modelled as an
# i5-13500 P-core: 48 KB L1d and 2 MB "last level" so L2 misses show up)
cd "$(dirname "$0")/.."
t=$1; sc=${2:-sample}
MSYS_NO_PATHCONV=1 docker run --rm -e TRACK=$t -v "$PWD/track-$t:/w:ro" -v "$PWD/data/$t-$sc-1:/data:ro" -v "$PWD/.out/cg:/out" \
  rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb sh -c '
    set -e
    apt-get update >/dev/null 2>&1 && apt-get install -y valgrind >/dev/null 2>&1
    cp -r /w /src && cd /src && rm -rf target
    CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release --offline 2>&1 | tail -1
    valgrind --tool=cachegrind --cache-sim=yes --branch-sim=yes --D1=49152,12,64 --LL=2097152,16,64 \
      --cachegrind-out-file=/out/cg.out ./target/release/solution 2>&1 | tail -22'
