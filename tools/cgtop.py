"""usage: python tools/cgtop.py <cg.out> <source.rs> [n]  -- top source lines from a cachegrind profile
Columns: Ir instructions, D1mr L1 data read misses, Bcm branch mispredicts (all summed over the whole run)."""
import sys, collections
out, src = sys.argv[1], sys.argv[2]; n = int(sys.argv[3]) if len(sys.argv) > 3 else 40
ev = None; fl = None; per = collections.defaultdict(lambda: [0, 0, 0])
for line in open(out):
    if line.startswith("events:"): ev = line.split()[1:]; ix = {e: i for i, e in enumerate(ev)}; continue
    if line.startswith(("fl=", "fi=", "fe=")): fl = line[3:].strip(); continue
    if line[0].isdigit():
        f = line.split()
        if fl and fl.endswith("main.rs"):
            ln = int(f[0]); v = f[1:]
            per[ln][0] += int(v[ix["Ir"]]); per[ln][1] += int(v[ix["D1mr"]]); per[ln][2] += int(v[ix["Bcm"]])
text = open(src).read().split("\n")
tot = [sum(v[i] for v in per.values()) for i in range(3)]
print(f"main.rs totals: Ir {tot[0]:,}  D1mr {tot[1]:,}  Bcm {tot[2]:,}")
for ln, v in sorted(per.items(), key=lambda kv: -(kv[1][0] + 30 * kv[1][1] + 20 * kv[1][2]))[:n]:
    print(f"{ln:4d} Ir {v[0]:>11,} ({100*v[0]/tot[0]:4.1f}%)  D1mr {v[1]:>9,}  Bcm {v[2]:>9,} | {text[ln-1].strip()[:90] if ln-1 < len(text) else ''}")
