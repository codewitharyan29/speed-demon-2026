#!/bin/sh
# usage: tools/stress_check.sh <binary name in the sdtarget-a volume> <data dir>...   (data dir: data/a-tiny-1, data/a-stress-7-max, ...)
# Runs the binary on each directory in Docker and compares its output with the Python reference (.out/a_ingest.py), which is run once per
# directory and cached as <dir>/ref.txt. Make the inputs with tools/stress_a.py.
cd "$(dirname "$0")/.."
bin=$1; shift
for d in "$@"; do
  [ -f "$d/ref.txt" ] || { mkdir -p .out/refrun && python .out/a_ingest.py --data "$d" --out .out/refrun && mv .out/refrun/summary.txt "$d/ref.txt"; }
  got=$(MSYS_NO_PATHCONV=1 docker run --rm --network=none -v sdtarget-a:/target:ro -v "$PWD/$d:/data:ro" --tmpfs /out sd-bench sh -c "/target/$bin --data /data --out /out && cat /out/summary.txt" | sha256sum | cut -c1-16)
  want=$(sha256sum < "$d/ref.txt" | cut -c1-16)
  [ "$got" = "$want" ] && echo "OK   $bin $d $got ($(wc -l < "$d/ref.txt") lines)" || echo "FAIL $bin $d got $got want $want"
done
