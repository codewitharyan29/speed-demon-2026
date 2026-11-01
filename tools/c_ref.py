# Independent reference for Track C, written from spec c-join.md (csv module, excel dialect).
import csv, sys
data, out = sys.argv[1], sys.argv[2]
reg = {}
with open(f"{data}/regions.csv", newline="") as f:
    next(f)
    for line in f:
        a, b = line.rstrip("\n").split(",")
        reg[int(a)] = b
g = {}
with open(f"{data}/records.csv", encoding="utf-8", newline="") as f:
    rd = csv.reader(f)
    first = True
    for row in rd:
        if row and row[0].startswith("﻿"):
            row[0] = row[0][1:]
        if first:
            assert row == ["id", "name", "note", "region_id", "amount"], row
            first = False
            continue
        i, name, note, rid, amt = row
        neg = amt.startswith("-")
        w, fr = amt.lstrip("-").split(".")
        c = int(w) * 100 + int(fr)
        c = -c if neg else c
        key = reg.get(int(rid), "_UNMATCHED")
        s = g.get(key)
        if s is None:
            s = g[key] = [0, 0, 0, None, None, set(), 0, 0]
        s[0] += 1; s[1] += int(i); s[2] += c
        s[3] = c if s[3] is None else min(s[3], c)
        s[4] = c if s[4] is None else max(s[4], c)
        s[5].add(name); s[6] += len(name.encode()); s[7] += len(note.encode())
def m(v):
    return ("-" if v < 0 else "") + f"{abs(v)//100}.{abs(v)%100:02d}"
with open(out, "w", newline="\n") as f:
    for k in sorted(g):
        s = g[k]
        f.write(f"{k} {s[0]} {s[1]} {m(s[2])} {m(s[3])} {m(s[4])} {len(s[5])} {s[6]} {s[7]}\n")
