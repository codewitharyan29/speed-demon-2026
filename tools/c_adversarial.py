# usage: python tools/c_adversarial.py <out dir>
# Writes records.csv and regions.csv that follow spec 2.1 but not only what the generator writes: an unterminated last record,
# unquoted non-empty text, long names with doubled quotes, fields made of quotes only, a 3 KB note, regions at the edges of the
# table, one-record groups, BOMs after LF and CRLF, and ids that make id_sum exceed 2**32.
import os, sys, random

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
rnd = random.Random(7)
BOM = b"\xef\xbb\xbf"
regions = [(1, "AAA"), (2, "AAB"), (999999, "ZZZ"), (500000, "MMM"), (1000000, "QQQ")]
with open(f"{out}/regions.csv", "wb") as f:
    f.write(b"region_id,country\n")
    for rid, c in sorted(regions):
        f.write(f"{rid},{c}\n".encode())

names = [b"", b'""', b'"Betts"', b'"Betts "', b'"Ann ""Annie"" Lee"', b'"' + b"x" * 40 + b'"', b'"' + b'a ""q"" ' * 12 + b'"',
         b'""""', b'"""x"', b'"x"""', b"plain", b'"Kaphar, Titus"']
notes = [b"", b'""', b'"two\nlines"', b'"a, ""b""\nc"', b'"""x"', b'"x"""', b'""""', b'"' + b"word " * 600 + b'"', b"unquoted note", b'"lone "" quote"']
amounts = [b"0.00", b"-0.01", b"-0.99", b"99999.99", b"-999.99", b"12.50", b"100000.00"]
rows = [BOM + b"id,name,note,region_id,amount\r\n"]
rid_pool = [1, 2, 999999, 500000, 1000000, 7, 9999999, 123456]
id_ = 4_000_000_000  # ids are not bounded by the grammar, only by the magnitude remark; keep sums inside i64
for i in range(4000):
    id_ += rnd.choice([1, 1, 2, 11])
    rec = b"%d,%s,%s,%d,%s" % (id_, rnd.choice(names), rnd.choice(notes), rnd.choice(rid_pool), rnd.choice(amounts))
    term = rnd.choice([b"\r\n", b"\n"])
    bom = BOM if (i > 0 and rnd.random() < 0.05) else b""
    rows.append(bom + rec + (term if i < 3999 else b""))  # the last record has no terminator
open(f"{out}/records.csv", "wb").write(b"".join(rows))
print("wrote", out)
