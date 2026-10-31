#!/bin/sh
# usage: tools/abbuild.sh <git-ref> <name> [track]  -- builds track-<t> as of <git-ref> (8 worker threads, like tools/hyper.sh)
# into the volume used by tools/hyper.sh as /target/<name>, so `BIN=/target/<name> tools/hyper.sh a` can time it against the work tree.
cd "$(dirname "$0")/.."
ref=$1; name=$2; t=${3:-a}
rm -rf ".out/ab-$name" && mkdir -p ".out/ab-$name" && git archive "$ref" "track-$t" | tar -x -C ".out/ab-$name"
MSYS_NO_PATHCONV=1 docker run --rm --network=none -v "$PWD/.out/ab-$name/track-$t:/src:ro" -v sdtarget-$t:/target sd-bench sh -ec '
  rm -rf /w2 && cp -r /src /w2 && cd /w2 && sed -i "s/^const THREADS: usize = .*/const THREADS: usize = 8;/" src/main.rs
  CARGO_TARGET_DIR=/target2 cargo build --release --offline >/tmp/b.log 2>&1 || { cat /tmp/b.log; exit 1; }
  cp /target2/release/solution /target/'"$name"' && ls -la /target/'"$name"
