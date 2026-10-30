#!/bin/sh
# usage: tools/difftest.sh <d|b> <scale> <seed>
# Builds track-<t> in Docker (Linux), runs it on data/<t>-<scale>-<seed>, and diffs against the Python reference.
set -e
cd "$(dirname "$0")/.."
t=$1; d="data/$t-$2-$3"; mod=$( [ "$t" = d ] && echo d_traverse || echo b_index )
out=$( [ "$t" = d ] && echo distances.txt || echo results.txt )
mkdir -p ".out/ref-$t" ".out/new-$t"
[ -f "$d/ref.txt" ] || { python ../speed-demon-2026-org/reference/$mod.py --data "$d" --out ".out/ref-$t" && mv ".out/ref-$t/$out" "$d/ref.txt"; }
docker build -q --network=none -t sd-$t track-$t >/dev/null
rm -f ".out/new-$t/$out"
MSYS_NO_PATHCONV=1 docker run --rm --network=none -e TRACK=$t -v "$PWD/$d:/data:ro" -v "$PWD/.out/new-$t:/out" sd-$t
cmp "$d/ref.txt" ".out/new-$t/$out" && echo "OK $t $2 seed $3 $(sha256sum < "$d/ref.txt" | cut -c1-16)"
