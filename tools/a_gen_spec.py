# usage: a_gen.py <out dir> <seed> <lines> -- Track A input written from spec a-ingest.md (own code, not the organisers'
# generator): 72 hours, 8 shards with non-decreasing timestamps shipped in chunks (shard 0 hourly, 5-10 min late), Zipf(1)
# popularity over 1,842 templates (the 13 fixed ones plus composed ones), the latency-digit mix measured on seed 1.
import os, random, sys, bisect, itertools
out, seed, N = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rnd = random.Random(seed); os.makedirs(out, exist_ok=True)
fixed = ["/", "/health", "/healthz", "/v2/checkout", "/v2/orders", "/v2/orders-export", "/v2/orders.csv", "/v2/orders/{id}",
 "/v2/orders/{id}/items", "/v2/ordersByDate", "/v2/orders_legacy", "/v2/orderstatus",
 "/internal/v1/tenants/{tenant_id}/projects/{project_id}/pipelines/{pipeline_id}/runs/{run_id}/artifacts/{artifact_id}:download"]
voc = "users orders items carts payments invoices search reports export import admin metrics tenants projects jobs runs files tags v1 v2 v3 api internal public".split()
cat = set(fixed)
while len(cat) < 1842:
    parts = [rnd.choice(["v1", "v2", "v3", "api", "internal"])] + [rnd.choice(voc + ["{id}", "{user_id}", "{sku}", "rank%d" % rnd.randint(1, 999)]) for _ in range(rnd.randint(1, 6))]
    e = "/" + "/".join(parts) + rnd.choice(["", "", ".json", ":batch", "-v2", "_old"])
    if len(e) <= 125: cat.add(e)
cat = sorted(cat); rnd.shuffle(cat)
w = list(itertools.accumulate(1 / (i + 1) for i in range(len(cat))))
statuses = [200]*60 + [201,202,204,301,302,304,400,401,403,404,405,409,410,422,429,499,500,501,502,503,504,599]
def lat():
    r = rnd.random(); d = 4 if r < .476 else 5 if r < .837 else 6 if r < .934 else 3 if r < .985 else 7 if r < .997 else rnd.choice([1, 2, 8, 9, 10])
    if d == 10: return rnd.randint(1000000000, 4294967295)
    return rnd.randint(10 ** (d - 1) if d > 1 else 0, 10 ** d - 1)
methods = ["GET"]*6 + ["POST", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
SPAN = 72 * 3600 * 10**6
def ts(us):
    s, u = divmod(us, 10**6); h = s // 3600 + 18; d = h // 24; h %= 24; m, sec = divmod(s % 3600, 60)
    day = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][d]
    return "%sT%02d:%02d:%02d.%06dZ" % (day, h, m, sec, u)
def line(us, ep=None, st=None, la=None):
    ep = ep or cat[bisect.bisect(w, rnd.random() * w[-1])]
    return "%s %s %s %d %d\n" % (ts(us), rnd.choice(methods), ep, st or rnd.choice(statuses), lat() if la is None else la)
# shards: per shard sorted times, then chunks with an arrival time
chunks = []
per = N // 8
for sh in range(8):
    times = sorted(rnd.randrange(SPAN) for _ in range(per))
    if sh == 0:
        for h in range(72):
            seg = [t for t in times[bisect.bisect_left(times, h*3600*10**6):bisect.bisect_left(times, (h+1)*3600*10**6)]]
            chunks.append(((h + 1) * 3600 * 10**6 + rnd.randint(300, 600) * 10**6, seg))
    else:
        i, t0 = 0, 0
        while i < len(times):
            t1 = t0 + rnd.randint(1, 120) * 10**6
            j = bisect.bisect_left(times, t1, i)
            chunks.append((t1 + rnd.randint(0, 30) * 10**6, times[i:j])); i, t0 = j, t1
chunks.sort(key=lambda c: c[0])
lines = [line(t) for _, seg in chunks for t in seg]
# edge cases
ins = []
for h in range(1, 72):
    b = h * 3600 * 10**6
    ins.append((line(b) + line(b - 1)))                                   # E4/E12
ins += [line(0), line(SPAN - 1)]                                          # E5
for _ in range(31):
    e = rnd.choice(cat); t = rnd.randrange(SPAN)
    ins.append(line(t, e, 200, 0) + line(t, e, 200, 4294967295))          # E2-ish
    ins.append("".join(line(t, e, s) for s in (200, 499, 500, 599)))       # E3
    l = line(rnd.randrange(SPAN)); ins.append(l + l)                      # E11
    ins.append(line(rnd.randrange(SPAN), la=0)); ins.append(line(rnd.randrange(SPAN), la=4294967295))
for e in fixed: ins.append(line(5 * 3600 * 10**6 + 7, e))                 # E14
# each inserted group goes near the place of its own time in the file (by the fraction of the span it is at)
for k in ins:
    t = int(k[11:13]) * 3600 + int(k[8:10]) * 86400  # rough: day and hour of its first line
    frac = min(max(((int(k[8:10]) - 30) % 31 * 24 + int(k[11:13]) - 18 + 0.5) / 72, 0), 1)
    lines.insert(min(len(lines), int(frac * len(lines)) + rnd.randint(0, 2000)), k)
open(f"{out}/requests.log", "w").write("".join(lines))
