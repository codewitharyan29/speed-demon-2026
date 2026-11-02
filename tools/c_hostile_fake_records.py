# hostile inputs whose quoted notes contain complete, correctly escaped fake records (LF + digit), so a cut can land
# in a quoted field and the garbage parse can still look plausible
import os, random, sys
out, seed = sys.argv[1], int(sys.argv[2]); os.makedirs(out, exist_ok=True); rnd = random.Random(seed)
BOM = b"\xef\xbb\xbf"
with open(f"{out}/regions.csv", "wb") as f:
    f.write(b"region_id,country\n")
    for rid in range(1, 2001): f.write(f"{rid},{chr(65 + rid % 5)}{chr(65 + rid % 7)}{chr(65 + rid % 3)}\n".encode())
def fake():
    return b'\n%d,""fake, ""n""\"",""a\nb"",%d,%d.%02d' % (rnd.randint(1, 9999), rnd.randint(1, 2100), rnd.randint(0, 99), rnd.randint(0, 99))
rows = [BOM + b"id,name,note,region_id,amount\r\n"]
for i in range(1, 40001):
    name = rnd.choice([b'"Ann"', b'"Bob, Jr"', b'""', b"", b'"A ""B"" C"'])
    note = rnd.choice([b'"plain"', b'""', b"", b'"x' + fake() + b'"', b'"y' + fake() + fake() + b'"', b'"q\n3 z"'])
    rec = b"%d,%s,%s,%d,%d.%02d" % (i, name, note, rnd.randint(1, 2100), rnd.randint(-99, 999), rnd.randint(0, 99))
    rows.append(rec + rnd.choice([b"\r\n", b"\n"]))
open(f"{out}/records.csv", "wb").write(b"".join(rows))
