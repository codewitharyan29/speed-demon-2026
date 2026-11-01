# usage: python3 bench.py <rounds> <bin>...  interleaved (order rotated each round) wall time and max RSS on /data
import os, sys, time, statistics, subprocess
R, names = int(sys.argv[1]), sys.argv[2:]
if os.path.realpath("/data") != os.path.realpath("/home/user/cdata/" + os.environ.get("DS", "full-1")):
    os.remove("/data"); os.symlink("/home/user/cdata/" + os.environ.get("DS", "full-1"), "/data")
res = {n: [] for n in names}; rss = {n: [] for n in names}; cpu = {n: [] for n in names}
for n in names: subprocess.run([f"/home/user/bins/{n}"])
for r in range(R):
    order = names[r % len(names):] + names[:r % len(names)]
    for n in order:
        t = time.perf_counter(); p = os.fork()
        if p == 0:
            os.execv(f"/home/user/bins/{n}", [n])
        _, st, ru = os.wait4(p, 0); e = time.perf_counter()
        res[n].append((e - t) * 1000); cpu[n].append((ru.ru_utime+ru.ru_stime)*1000); rss[n].append(ru.ru_maxrss / 1024)
for n in names:
    v = sorted(res[n]); q = len(v) // 4
    print(f"{n:12s} median {statistics.median(v):7.2f}  min {v[0]:7.2f}  q1 {v[q]:7.2f} q3 {v[-q-1]:7.2f} ms  maxrss {statistics.median(rss[n]):6.1f} MB  cpu {statistics.median(cpu[n]):7.1f} ms")
