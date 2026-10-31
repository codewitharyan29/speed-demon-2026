"""Write a hostile-but-valid Track A input: python tools/stress_a.py <seed> [lines] [sorted|shuffled] -> data/a-stress-<seed>/requests.log
Covers what the generated data may not: endpoints of every length 1..125 bytes (sharing prefixes and suffixes), latencies of 1..10
digits up to 4294967295, every status class, all four days, 1,800 endpoints, one huge group, and shuffled or sorted timestamps."""
import os, random, sys

seed, lines = int(sys.argv[1]), int(sys.argv[2]) if len(sys.argv) > 2 else 400_000
order = sys.argv[3] if len(sys.argv) > 3 else "shuffled"
rnd = random.Random(seed)
alphabet = "abcdefghij/_-{}0123456789"
endpoints = set()
while len(endpoints) < 1800:
    n = rnd.choice([1, 2, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 100, 124, 125, rnd.randint(1, 125), rnd.randint(1, 40)])
    stem = "/" + "".join(rnd.choice(alphabet) for _ in range(n))
    if rnd.random() < 0.3 and endpoints:  # near-duplicates: same head and tail, different middle
        base = rnd.choice(sorted(endpoints))
        if len(base) > 40:
            i = rnd.randint(17, len(base) - 17)
            stem = base[:i] + rnd.choice(alphabet) + base[i + 1:]
    endpoints.add(stem[:125])
endpoints = sorted(endpoints)
weights = [1 / (i + 1) ** 0.8 for i in range(len(endpoints))]
methods = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
digits = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
start = 18 * 3600  # seconds after 2026-10-30T00:00Z
days = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"]
rows = []
for _ in range(lines):
    t = start + rnd.random() * 72 * 3600 - 0.000001
    sec = int(t); us = int((t - sec) * 1e6)
    ts = f"{days[sec // 86400]}T{sec % 86400 // 3600:02d}:{sec % 3600 // 60:02d}:{sec % 60:02d}.{us:06d}Z"
    d = rnd.choice(digits)
    lat = rnd.randint(10 ** (d - 1) if d > 1 else 0, min(10 ** d - 1, 4294967295))
    if rnd.random() < 0.0005:
        lat = 4294967295
    ep = endpoints[0] if rnd.random() < 0.05 else rnd.choices(endpoints, weights)[0]
    rows.append((ts, f"{ts} {rnd.choice(methods)} {ep} {rnd.randint(100, 599)} {lat}"))
if order == "sorted":
    rows.sort()
os.makedirs(f"data/a-stress-{seed}", exist_ok=True)
with open(f"data/a-stress-{seed}/requests.log", "w", newline="\n") as f:
    f.write("\n".join(r[1] for r in rows) + "\n")
print(f"data/a-stress-{seed}: {lines} lines, {len(endpoints)} endpoints, {order}")
