# usage: a_ref.py <data dir> <out file> -- independent Track A reference written from spec a-ingest.md
import sys, collections
g = collections.defaultdict(list); e = collections.Counter()
with open(sys.argv[1] + "/requests.log", "rb") as f:
    for l in f:
        ts, _, ep, st, la = l.split(b" ")
        k = (ts[:13], ep); g[k].append(int(la))
        if 500 <= int(st) <= 599: e[k] += 1
with open(sys.argv[2], "wb") as o:
    for k in sorted(g):
        v = sorted(g[k]); n = len(v); r = lambda p: v[(p * n + 99) // 100 - 1]
        o.write(b"%s:00:00Z %s %d %d %d %d %d %d\n" % (k[0], k[1], n, r(50), r(95), r(99), e[k], e[k] * 1000000 // n))
