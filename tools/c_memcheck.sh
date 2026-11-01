#!/bin/sh
# usage: tools/c_memcheck.sh <chunk shift> <data dir> <expected aggregates.txt>...  (pairs)
# Rust's sanitizers need a nightly compiler (not in the offline image), so this is the substitute: track-c built with debug
# assertions and overflow checks, run under valgrind memcheck (invalid reads/writes, uninitialised reads, leaks of control flow)
# with the chunk size set to 1 << <chunk shift>, then the output is compared with the reference. valgrind: see tools/cg_c.sh.
cd "$(dirname "$0")/.."
sh_=$1; shift
while [ $# -ge 2 ]; do
  d=$1; ref=$2; shift 2
  MSYS_NO_PATHCONV=1 docker run --rm --network=none -v "$PWD/track-c:/w:ro" -v "$PWD/$d:/data:ro" -v "$PWD/$ref:/ref.txt:ro" -v "$PWD/.out/valg:/debs:ro" --tmpfs /out sd-bench sh -c '
    dpkg -i /debs/valgrind_3.24.0-3_amd64.deb /debs/libc6-dbg_2.41-12+deb13u4_amd64.deb >/dev/null 2>&1
    cp -r /w /src && cd /src && sed -i "s/len, 18, 1 << 20)/len, 18, 1 << '"$sh_"')/" src/main.rs
    grep -q "1 << '"$sh_"')" src/main.rs || { echo "sed failed"; exit 1; }
    CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true \
      CARGO_TARGET_DIR=/tmp/t cargo build --release --offline >/tmp/b.log 2>&1 || { cat /tmp/b.log; exit 1; }
    valgrind --error-exitcode=9 --errors-for-leak-kinds=none -q /tmp/t/release/solution 2>&1 | head -30; rc=$?
    cmp /out/aggregates.txt /ref.txt && echo "MEMCHECK OK '"$d"' chunk 2^'"$sh_"'"'
done
