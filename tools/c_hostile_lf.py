# usage: python tools/c_hostile_lf.py <out dir> [seed]
# A file that follows spec 2.1 but breaks the one assumption of Track C's fast path (spec 2.2: inside a quoted note an LF is
# followed by a lowercase letter or a quote): notes here contain LF followed by a digit 1-9 and by a BOM, so a chunk cut can land
# on a position that looks like a record start. The program must still print the reference's output (by noticing and falling back).
import os, random, sys

out, seed = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 3
os.makedirs(out, exist_ok=True)
rnd = random.Random(seed)
BOM = b"\xef\xbb\xbf"
with open(f"{out}/regions.csv", "wb") as f:
    f.write(b"region_id,country\n")
    for rid in range(1, 2001):
        f.write(f"{rid},{chr(65 + rid % 5)}{chr(65 + rid % 7)}{chr(65 + rid % 3)}\n".encode())
notes = [b'"plain"', b'""', b'"line\n7 digit first"', b'"x\n1"', b'"a,\n5,b"', b"\"\n9\"", b'"y\n' + BOM + b'z"', b'"q\n2 ""r"" 3\n4"']
rows = [BOM + b"id,name,note,region_id,amount\r\n"]
for i in range(1, 60001):
    name = rnd.choice([b'"Ann"', b'"Bob, Jr"', b'""', b""])
    rec = b"%d,%s,%s,%d,%d.%02d" % (i, name, rnd.choice(notes), rnd.randint(1, 2100), rnd.randint(-99, 999), rnd.randint(0, 99))
    rows.append(rec + rnd.choice([b"\r\n", b"\n"]))
open(f"{out}/records.csv", "wb").write(b"".join(rows))
print("wrote", out, sum(map(len, rows)), "bytes")
