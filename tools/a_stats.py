"""usage: python tools/a_stats.py <data dir> <out groups.bin>
Input distribution of a Track A log (used by track-a/ALGORITHMIC_RESEARCH.md) and, for the percentile benchmark, a binary file
`groups.bin`: for every (hour, endpoint) group in output order, u32 n followed by n u32 latencies, in input order."""
import collections, struct, sys

d, out = sys.argv[1], sys.argv[2]
groups = collections.defaultdict(list)
n = nbytes = 0
lens = collections.Counter()
digits = collections.Counter()
first_hour_lag = 0
prev = None
for line in open(f"{d}/requests.log", "rb"):
    n += 1
    nbytes += len(line)
    ts, method, ep, status, lat = line.rstrip(b"\n").split(b" ")
    hour = ts[:13]
    groups[(hour, ep)].append(int(lat))
    lens[len(ep)] += 1
    digits[len(lat)] += 1
    if prev is not None and hour < prev:
        first_hour_lag += 1
    prev = max(prev, hour) if prev else hour
eps = {k[1] for k in groups}
hours = {k[0] for k in groups}
sizes = sorted(len(v) for v in groups.values())
tot = sum(sizes)
print(f"lines {n}, bytes {nbytes} ({nbytes / n:.1f} per line), endpoints {len(eps)}, hours {len(hours)}, groups {len(groups)}")
print(f"endpoint length: mean {sum(k * v for k, v in lens.items()) / n:.1f}, max {max(lens)}")
print("latency digits:", dict(sorted(digits.items())))
for lo, hi in [(1, 1), (2, 2), (3, 8), (9, 32), (33, 128), (129, 1024), (1025, 10**9)]:
    g = [s for s in sizes if lo <= s <= hi]
    print(f"groups of {lo}..{hi}: {len(g)} ({100 * len(g) / len(sizes):.1f}% of groups), {sum(g)} records ({100 * sum(g) / tot:.1f}%)")
print("largest groups:", sizes[-5:], "median", sizes[len(sizes) // 2])
print(f"lines whose hour is behind the newest seen: {first_hour_lag} ({100 * first_hour_lag / n:.1f}%)")
with open(out, "wb") as f:
    for k in sorted(groups, key=lambda k: (k[0], k[1])):
        v = groups[k]
        f.write(struct.pack("<I", len(v)) + struct.pack(f"<{len(v)}I", *v))
