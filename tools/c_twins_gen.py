# usage: twins_gen.py <out dir> -- names that differ from each other in exactly one byte, at every position 0..40, also with
# "" pairs and trailing spaces, so that any byte a comparison skips merges two names and changes names_distinct.
import os, sys, random
out = sys.argv[1]; os.makedirs(out, exist_ok=True); rnd = random.Random(11)
with open(f"{out}/regions.csv", "w") as f:
    f.write("region_id,country\n" + "".join(f"{r},{'ABCDE'[r % 5]}AA\n" for r in range(1, 101)))
base = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP"
names = set()
for L in range(1, 42):
    for p in range(L):
        for c in "xX\" ":
            n = base[:p] + c + base[p + 1:L]
            names.add(n); names.add(base[:L]); names.add(n + " ")
names = sorted(names)
rows = [b"\xef\xbb\xbfid,name,note,region_id,amount\r\n"]
i = 0
for k in range(60000):
    i += 1; n = rnd.choice(names)
    q = '"' + n.replace('"', '""') + '"'
    rows.append(f'{i},{q},"n",{rnd.randint(1, 105)},{rnd.randint(-99, 999)}.{rnd.randint(0, 99):02d}\n'.encode())
open(f"{out}/records.csv", "wb").write(b"".join(rows))
