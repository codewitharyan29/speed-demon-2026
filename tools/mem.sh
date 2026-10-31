#!/bin/sh
# usage: tools/mem.sh <name> [runs]   -- memory.peak (what the scorer reads) of a binary in the sdtarget-a volume, one fresh container per run.
# The Docker VM's THP setting changes M a lot (the board has `always`): `cat /sys/kernel/mm/transparent_hugepage/enabled` in the VM.
cd "$(dirname "$0")/.."
for i in $(seq ${2:-3}); do
  MSYS_NO_PATHCONV=1 docker run --rm --network=none --memory=8g --cpuset-cpus=0-7 -v sdtarget-a:/target:ro --mount type=volume,src=sddata,dst=/data,volume-subpath=a,readonly \
    --tmpfs /out sd-bench sh -c "taskset -c 0-7 /target/$1 --data /data --out /out; echo \"$1 memory.peak \$(( \$(cat /sys/fs/cgroup/memory.peak) )) bytes = \$(( \$(cat /sys/fs/cgroup/memory.peak) / 10485 / 100 )).\$(( \$(cat /sys/fs/cgroup/memory.peak) / 10485 % 100 )) MB\""
done
