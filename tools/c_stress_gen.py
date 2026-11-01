# usage: stress_gen.py <out dir> <seed> <records>  -- valid per spec 2.1, aimed at name keying: the same value written
# quoted and unquoted, values with many "" pairs, long names (spill), trailing-space twins, mixed BOM/CRLF/LF, multi-line notes.
import os, random, sys
out, seed, n = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
os.makedirs(out, exist_ok=True)
rnd = random.Random(seed)
BOM = b"\xef\xbb\xbf"
regs = sorted(rnd.sample(range(1, 50000), 3000))
codes = [bytes(rnd.choice(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ") for _ in range(3)) for _ in range(200)]
with open(f"{out}/regions.csv", "wb") as f:
    f.write(b"region_id,country\n")
    for r in regs:
        f.write(b"%d,%s\n" % (r, rnd.choice(codes)))
letters = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
def value():
    k = rnd.random()
    L = rnd.choice([0, 1, 2, 5, 8, 9, 12, 15, 16, 17, 20, 23, 24, 25, 26, 31, 32, 33, 40, 70, 200])
    v = bytearray(rnd.choice(letters + b"  ,") for _ in range(L))
    if k < 0.3 and L:
        for _ in range(rnd.randint(1, 6)):
            v.insert(rnd.randint(0, len(v)), 34)  # '"'
    if k > 0.9:
        v += b" " * rnd.randint(1, 4)
    return bytes(v)
pool = [value() for _ in range(3000)]
pool += [p + b" " for p in pool[:200]]
def write(v):
    if b'"' not in v and b"," not in v and rnd.random() < 0.3:
        return v  # unquoted form of the same value
    return b'"' + v.replace(b'"', b'""') + b'"'
def note():
    words = [bytes(rnd.choice(b"abcxyz") for _ in range(rnd.randint(1, 7))) for _ in range(rnd.randint(0, 12))]
    s = bytearray()
    for i, w in enumerate(words):
        if i: s += rnd.choice([b" ", b" ", b"\n", b", ", b' "q" ', b'"'])
        s += w
    if not s and rnd.random() < 0.5: return b""
    return b'"' + bytes(s).replace(b'"', b'""') + b'"'
rows = [BOM + b"id,name,note,region_id,amount\r\n"]
i = 0
for k in range(n):
    i += rnd.choice([1, 1, 1, 2, 11])
    region = rnd.choice(regs) if rnd.random() < 0.97 else rnd.choice([rnd.randint(1, 60000), 99999999, 123456789012])
    amt = rnd.choice([b"0.00", b"-0.01", b"-0.99", b"12.34", b"99999.99", b"-999.99", b"%d.%02d" % (rnd.randint(-999, 99999), rnd.randint(0, 99))])
    if amt.startswith(b"-0.00"): amt = b"0.00"
    rec = b"%d,%s,%s,%d,%s" % (i, write(rnd.choice(pool)), note(), region, amt)
    bom = BOM if k and rnd.random() < 0.02 else b""
    rows.append(bom + rec + rnd.choice([b"\r\n", b"\n"]))
open(f"{out}/records.csv", "wb").write(b"".join(rows))
