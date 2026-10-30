#!/bin/sh
# usage: tools/pgo.sh <d|b> [dir]   -> writes <dir>/pgo.profdata (dir defaults to track-<t>)
# Builds the project with instrumentation in the pinned build image (rustc 1.99.1), runs it on the full seed-1 data
# from the docker volume `sddata`, and merges the raw profiles with the llvm-profdata that matches that compiler.
# The profile only applies to exactly this source: regenerate it after every edit.
set -e
cd "$(dirname "$0")/.."
t=$1; dir=${2:-track-$t}
tmp=$(mktemp -d); mkdir -p .out/new-$t
MSYS_NO_PATHCONV=1 docker run --rm -e TRACK=$t -v "$(cd "$dir" && pwd -W 2>/dev/null || pwd):/w:ro" -v "$(cd "$tmp" && pwd -W 2>/dev/null || pwd):/prof-out" -v "$PWD/.out/new-$t:/out" \
  --mount type=volume,src=sddata,dst=/data,volume-subpath=$t,readonly \
  rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb sh -c '
    set -e
    rustup component add llvm-tools >/dev/null 2>&1
    cp -r /w /src && cd /src && rm -rf target .cargo
    RUSTFLAGS="-Cprofile-generate=/prof" cargo build --release --offline 2>&1 | tail -1
    ./target/release/solution 2>/dev/null
    $(find $(rustc --print sysroot) -name llvm-profdata | head -1) merge -o /prof-out/pgo.profdata /prof'
cp "$tmp/pgo.profdata" "$dir/pgo.profdata"
rm -rf "$tmp"
