#!/bin/sh
# usage: tools/c_check.sh <binary name in the sdtarget-c volume> <data dir> <expected aggregates.txt>...  (pairs)
# Runs the binary on the data dir in Docker (read-only /data, tmpfs /out) and compares the output byte for byte.
cd "$(dirname "$0")/.."
bin=$1; shift
while [ $# -ge 2 ]; do
  d=$1; ref=$2; shift 2
  got=$(MSYS_NO_PATHCONV=1 docker run --rm --network=none -v sdtarget-c:/target:ro -v "$PWD/$d:/data:ro" --tmpfs /out sd-bench sh -c "/target/$bin && cat /out/aggregates.txt" | sha256sum | cut -c1-16)
  want=$(sha256sum < "$ref" | cut -c1-16)
  [ "$got" = "$want" ] && echo "OK   $bin $d $got" || echo "FAIL $bin $d got $got want $want"
done
