#!/bin/sh
# usage: tools/variant.sh <name> '<sed script>' [track]  -- builds the work tree with a sed script applied to src/main.rs (and 8 threads)
# into /target/<name> of the tools/hyper.sh volume: `BIN=/target/<name> tools/hyper.sh a` times it against the unpatched tree.
cd "$(dirname "$0")/.."
name=$1; seds=$2; t=${3:-a}
MSYS_NO_PATHCONV=1 docker run --rm --network=none -e SEDS="$seds" -v "$PWD/track-$t:/src:ro" -v sdtarget-$t:/target sd-bench sh -ec '
  rm -rf /w2 && cp -r /src /w2 && cd /w2 && sed -i "s/^const THREADS: usize = .*/const THREADS: usize = 8;/" src/main.rs
  sed -i "$SEDS" src/main.rs
  CARGO_TARGET_DIR=/target2 cargo build --release --offline >/tmp/b.log 2>&1 || { cat /tmp/b.log; exit 1; }
  cp /target2/release/solution /target/'"$name"
