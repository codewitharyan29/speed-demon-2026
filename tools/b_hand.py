"""Hand-made Track B test: tokens of 17, 21, 32 and 40 bytes (and others) in docs, BOOL and PHRASE queries.
usage: python tools/b_hand.py <outdir> [seed]   (then run the Python reference on it and compare)"""
import random, sys, os
out, seed = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 1
r = random.Random(seed)
al = 'abcdefghijklmnopqrstuvwxyz0123456789'
short = [''.join(r.choice(al) for _ in range(r.randint(1, 9))) for _ in range(60)]
longs = [''.join(r.choice(al) for _ in range(n)) for n in (16, 17, 18, 21, 24, 31, 32, 17, 21, 32, 20, 25)]
toolong = [''.join(r.choice(al) for _ in range(40)), longs[6] + 'x', longs[6] + 'yz']  # >32 bytes, share a 32-byte prefix
vocab = short + longs + toolong + ['and', 'or', 'not']
docs = []
for i in range(3000):
    n = r.choice([0, 1, 3, 10, 40, 120])
    ws = [r.choice(vocab) for _ in range(n)]
    seps = [' ', ' ', ', ', '-', ' — ', '. ', ' ']
    t = ''
    for w in ws:
        w = w.upper() if r.random() < 0.2 else w.capitalize() if r.random() < 0.2 else w
        t += w + r.choice(seps)
    docs.append(t.rstrip(' ') if r.random() < 0.5 else t)
os.makedirs(out, exist_ok=True)
with open(out + '/docs.txt', 'w', encoding='utf8', newline='\n') as f:
    for i, t in enumerate(docs):
        f.write(f'{i}\t{t}\n')
qs = []
cands = [w for w in vocab if len(w) <= 32] + ['absent', 'x' * 32, 'y' * 17]
for _ in range(1500):
    k = r.randint(1, 6)
    ts = [r.choice(longs + short[:10] + cands[:20]) if r.random() < .6 else r.choice(cands) for _ in range(k)]
    q = 'BOOL ' + ts[0]
    for t in ts[1:]:
        q += ' ' + r.choice(['AND', 'OR', 'NOT']) + ' ' + t
    qs.append(q)
for _ in range(1500):
    d = r.choice([x for x in docs if len(x) > 30])
    import re
    toks = re.findall(r'[a-z0-9]+', d.lower())
    if len(toks) < 2: continue
    k = r.randint(2, min(8, len(toks))); a = r.randint(0, len(toks) - k)
    ts = toks[a:a + k]
    if any(len(t) > 32 for t in ts): continue
    qs.append('PHRASE "' + ' '.join(ts) + '"')
r.shuffle(qs)
open(out + '/queries.txt', 'w', newline='\n').write('\n'.join(qs) + '\n')
