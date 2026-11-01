#!/bin/sh
# Local helper (session 4): fixtures in /home/user/cdata/<name> (+ <name>.ref), binaries in /home/user/bins.
# usage: icount.sh <src.rs>... : haswell build under valgrind on the sample; prints instructions and mispredicts
for f in "$@"; do n=$(basename $f .rs)_h; CPU=haswell sh "$(dirname "$0")/c_local_build.sh" $n $f >/dev/null; ln -sfn /home/user/cdata/${DS:-sample-1} /data
(cd /tmp && echo "$n $(valgrind --tool=cachegrind --cache-sim=no --branch-sim=yes --cachegrind-out-file=/dev/null /home/user/bins/$n 2>&1 | grep -E 'I *refs|Mispredicts:' | awk '{print $2, $3, $4}' | tr '\n' ' ')"); done
