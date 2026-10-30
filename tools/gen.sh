#!/bin/sh
# usage: tools/gen.sh <d|b> <tiny|sample|full> <seed>   -> data/<t>-<scale>-<seed>
# Uses the organisers' generator cloned next to this repo (../speed-demon-2026-org).
set -e
cd "$(dirname "$0")/.."
python ../speed-demon-2026-org/generators/$( [ "$1" = d ] && echo d_traverse || echo b_index ).py --seed "$3" --scale "$2" --out "data/$1-$2-$3"
