#!/bin/sh
# usage: tools/c_multiseed.sh <binary in sdtarget-c> <scale> <seed>...
# Generates each dataset with the official generator (.out/official), computes the official reference output and compares.
cd "$(dirname "$0")/.."
bin=$1; scale=$2; shift 2
for seed in "$@"; do
  d=.out/cdata/$scale-$seed
  if [ ! -f $d/ref.txt ]; then
    mkdir -p $d && python .out/official/generators/c_join.py --seed $seed --scale $scale --out $d >/dev/null 2>&1 || { echo "gen failed $seed"; continue; }
    mkdir -p $d/refout && python .out/official/reference/c_join.py --data $d --out $d/refout >/dev/null 2>&1 && mv $d/refout/aggregates.txt $d/ref.txt
  fi
  sh tools/c_check.sh $bin $d $d/ref.txt
done
