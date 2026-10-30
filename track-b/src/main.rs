use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const ONES: u64 = 0x0101010101010101;
const DOC_BITS: u32 = 14;

/// Maps a file read-only. The mapping is never unmapped, so the slice is 'static.
fn map(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    // SAFETY: PROT_READ (1), MAP_PRIVATE (2) on a file we just opened; the length is the file's.
    unsafe {
        std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len)
    }
}

/// Runs `f(state, job)` for every job in 0..jobs on THREADS threads that pull jobs from one shared counter,
/// so uneven jobs balance themselves. `init` builds each thread's private state. Results come back in job order.
fn par<S, R: Send>(
    jobs: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut all: Vec<(usize, R)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let (mut state, mut got) = (init(), Vec::new());
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return got;
                        }
                        got.push((j, f(&mut state, j)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect()
    });
    all.sort_unstable_by_key(|r| r.0);
    all.into_iter().map(|r| r.1).collect()
}

/// Eight bytes at `at` as a little-endian word; bytes past the end of `b` read as zero.
fn load(b: &[u8], at: usize) -> u64 {
    match b.get(at..at + 8) {
        Some(s) => u64::from_le_bytes(s.try_into().unwrap()),
        None => {
            let (mut t, rest) = ([0; 8], b.get(at..).unwrap_or(&[]));
            t[..rest.len()].copy_from_slice(rest);
            u64::from_le_bytes(t)
        }
    }
}

/// Bit 7 of each byte of `w` is set if that byte is an ASCII letter or digit. Eight bytes at once: a byte `x` is in
/// lo..=hi when adding 0x80 - lo sets its top bit and adding 0x7F - hi does not (no carries, as x < 0x80 here).
fn alnum(w: u64) -> u64 {
    let x = w & ONES * 0x7F;
    let l = (w | ONES * 0x20) & ONES * 0x7F;
    let digit = (x + ONES * 0x50) & !(x + ONES * 0x46);
    let letter = (l + ONES * 0x1F) & !(l + ONES * 0x05);
    (digit | letter) & !w & ONES * 0x80
}

/// Calls `f(start, len)` for each token (maximal run of letters and digits) in b[from..to], until `f` returns false.
/// Token edges are the places where `alnum` flips between neighbouring bytes, and edges alternate start, end, start...
fn tokens(b: &[u8], from: usize, to: usize, mut f: impl FnMut(usize, usize) -> bool) {
    let (mut base, mut prev, mut start, mut open) = (from, 0u64, 0, false);
    while base <= to {
        let mut w = load(b, base);
        if to - base < 8 {
            w &= (1u64 << (8 * (to - base))) - 1;
        }
        let a = alnum(w);
        let mut edges = (a ^ (a << 8 | prev >> 56)) & ONES * 0x80;
        prev = a;
        while edges != 0 {
            let at = base + (edges.trailing_zeros() >> 3) as usize;
            edges &= edges - 1;
            if open && !f(start, at - start) {
                return;
            }
            start = at;
            open = !open;
        }
        base += 8;
    }
}

/// A token of up to 32 bytes, lower-cased, as four little-endian words (zero padded). Equal tokens give equal keys.
fn key(b: &[u8], from: usize, len: usize) -> [u64; 4] {
    let mut k = [0; 4];
    for (i, word) in k.iter_mut().enumerate().take(len.div_ceil(8)) {
        let n = (len - 8 * i).min(8);
        *word = (load(b, from + 8 * i) | ONES * 0x20) & (u64::MAX >> (64 - 8 * n));
    }
    k
}

const M: u64 = 0x9E3779B97F4A7C15;

fn hash(k: &[u64; 4]) -> u64 {
    let mut h = k[0].wrapping_mul(M);
    if k[1] | k[2] | k[3] != 0 {
        for &w in &k[1..] {
            h = (h.rotate_left(29) ^ w).wrapping_mul(M);
        }
    }
    h
}

/// An open-addressing hash table that numbers distinct keys in order of first appearance (slot = id + 1, 0 = empty).
struct Table<K> {
    keys: Vec<K>,
    slots: Vec<u32>,
    bits: u32,
}

impl<K: PartialEq + Copy> Table<K> {
    fn new(max_keys: usize) -> Self {
        let bits = (max_keys * 2).next_power_of_two().trailing_zeros();
        Table {
            keys: Vec::new(),
            slots: vec![0; 1 << bits],
            bits,
        }
    }

    /// The key's id, or the empty slot where it would go. `h` is the key's hash.
    fn find(&self, h: u64, k: &K) -> Result<u32, usize> {
        let mask = self.slots.len() - 1;
        let mut i = (h >> (64 - self.bits)) as usize;
        loop {
            match self.slots[i] {
                0 => return Err(i),
                s if self.keys[s as usize - 1] == *k => return Ok(s - 1),
                _ => i = (i + 1) & mask,
            }
        }
    }

    fn intern(&mut self, h: u64, k: K) -> u32 {
        self.find(h, &k).unwrap_or_else(|slot| {
            self.keys.push(k);
            self.slots[slot] = self.keys.len() as u32;
            self.keys.len() as u32 - 1
        })
    }
}

/// An adjacent pair of terms, as one key.
fn pair(a: u32, b: u32) -> u64 {
    (a as u64) << 32 | b as u64
}

/// A parsed query: term ids, and the operator byte (b'A', b'O' or b'N') between each neighbouring pair. A phrase has
/// no operators but the id of each adjacent pair of its terms.
struct Query {
    phrase: bool,
    terms: Vec<u32>,
    ops: Vec<u8>,
    pairs: Vec<u32>,
}

/// Merges sorted `a` with sorted `b` under `op`: AND keeps ids in both, OR in either, NOT in `a` only.
/// When one side is far shorter than the other and the result can only be as long as the short side, it is cheaper
/// to look each short id up in the long list by binary search than to walk the long list.
fn combine(op: u8, a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    let probe = |short: &[u32], long: &[u32], keep: bool, out: &mut Vec<u32>| {
        let mut from = 0;
        for &x in short {
            from += long[from..].partition_point(|&y| y < x);
            if (from < long.len() && long[from] == x) == keep {
                out.push(x);
            }
        }
    };
    if op != b'O' && a.len() * 16 < b.len() {
        return probe(a, b, op == b'A', out);
    }
    if op == b'A' && b.len() * 16 < a.len() {
        return probe(b, a, true, out);
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            if op != b'A' {
                out.push(a[i]);
            }
            i += 1;
        } else if a[i] > b[j] {
            if op == b'O' {
                out.push(b[j]);
            }
            j += 1;
        } else {
            if op != b'N' {
                out.push(a[i]);
            }
            i += 1;
            j += 1;
        }
    }
    if op != b'A' {
        out.extend_from_slice(&a[i..]);
    }
    if op == b'O' {
        out.extend_from_slice(&b[j..]);
    }
}

/// True if the tokens of b[from..to] contain `words` at consecutive positions. Bit-parallel (shift-and) matching:
/// bit j of `state` says "the last tokens match words[0..=j]", so one pass finds the phrase, repeated words included.
fn has_phrase(b: &[u8], from: usize, to: usize, words: &[[u64; 4]]) -> bool {
    let (mut state, mut found) = (0u8, false);
    tokens(b, from, to, |at, len| {
        let k = if len <= 32 {
            key(b, at, len)
        } else {
            [u64::MAX; 4]
        };
        let here = words
            .iter()
            .enumerate()
            .fold(0u8, |m, (j, w)| m | ((*w == k) as u8) << j);
        state = (state << 1 | 1) & here;
        found = state >> (words.len() - 1) & 1 == 1;
        !found
    });
    found
}

/// Appends the ids of the set bits of a bitset, ascending.
fn decode(bits: &[AtomicU64], out: &mut Vec<u32>) {
    for (i, w) in bits.iter().enumerate() {
        let mut w = w.load(Relaxed);
        while w != 0 {
            out.push((i * 64) as u32 + w.trailing_zeros());
            w &= w - 1;
        }
    }
}

fn put(out: &mut Vec<u8>, mut x: u64) {
    let mut buf = [0u8; 20];
    let mut i = 20;
    loop {
        i -= 1;
        buf[i] = b'0' + (x % 10) as u8;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

fn main() {
    // Intern every term of every query, and every adjacent pair of terms in a phrase. `used` says how a term is
    // used: bit 0 in a BOOL query (needs a posting list), bit 1 in a phrase (needs pair tracking in the scan).
    let q = map("/data/queries.txt");
    let words = q.iter().filter(|&&c| c == b' ' || c == b'\n').count();
    let (mut terms, mut grams, mut used) = (Table::new(words), Table::new(words), Vec::new());
    let queries: Vec<Query> = q
        .split(|&c| c == b'\n')
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.split(|&c| c == b' ');
            let phrase = parts.next().unwrap() == b"PHRASE";
            let (mut ids, mut ops) = (Vec::new(), Vec::new());
            for (k, w) in parts.enumerate() {
                if !phrase && k % 2 == 1 {
                    ops.push(w[0]);
                } else {
                    let w = w.strip_prefix(b"\"").unwrap_or(w);
                    let w = w.strip_suffix(b"\"").unwrap_or(w);
                    let k = key(w, 0, w.len());
                    let id = terms.intern(hash(&k), k);
                    used.resize(terms.keys.len(), 0u8);
                    used[id as usize] |= 1 + phrase as u8;
                    ids.push(id);
                }
            }
            let pairs = ids
                .windows(2)
                .filter(|_| phrase)
                .map(|w| grams.intern(pair(w[0], w[1]).wrapping_mul(M), pair(w[0], w[1])))
                .collect();
            Query {
                phrase,
                terms: ids,
                ops,
                pairs,
            }
        })
        .collect();
    let (nterms, nkeys) = (terms.keys.len(), terms.keys.len() + grams.keys.len());
    assert!(nkeys < 1 << (32 - DOC_BITS));

    // Scan the corpus in newline-aligned chunks. A chunk yields (term, doc) pairs for the query terms only,
    // each once per document, in document order, plus the byte offset of each of its documents. The first token
    // of a line is the document id.
    let docs = map("/data/docs.txt");
    // Boundary i of n equal parts of the file, moved to the start of the next line.
    let cut = |i: usize, n: usize| match i {
        0 => 0,
        _ if i == n => docs.len(),
        _ => {
            let at = (docs.len() * i / n).saturating_sub(1);
            at + docs[at..].iter().position(|&c| c == b'\n').unwrap() + 1
        }
    };
    // The id of the last line gives the document count.
    let tail = &docs[..docs.len() - 1];
    let last_line = tail.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
    let digits = tail[last_line..].iter().take_while(|c| c.is_ascii_digit());
    let n_docs = digits.fold(0, |x, &d| x * 10 + (d - b'0') as usize) + 1;

    // Terms in over 1/32 of the documents are kept as bitsets (a list that long would not be smaller), the rest as
    // lists. The first 12/576 of the corpus is a good enough sample to choose; a wrong guess only costs speed.
    const SAMPLE: usize = 12;
    let sampled = par(
        SAMPLE,
        || vec![0u32; nterms],
        |stamp, j| {
            let (mut line, mut seen, mut df) = (0, 0, vec![0u32; nterms]);
            tokens(
                docs,
                cut(j, 48 * SAMPLE),
                cut(j + 1, 48 * SAMPLE),
                |at, len| {
                    if at == 0 || docs[at - 1] == b'\n' {
                        (line, seen) = (at as u32 + 1, seen + 1);
                    } else if len <= 32 {
                        let k = key(docs, at, len);
                        if let Ok(t) = terms.find(hash(&k), &k) {
                            if used[t as usize] & 1 != 0 && stamp[t as usize] != line {
                                stamp[t as usize] = line;
                                df[t as usize] += 1;
                            }
                        }
                    }
                    true
                },
            );
            (df, seen)
        },
    );
    let seen: u32 = sampled.iter().map(|s| s.1).sum();
    let (mut dense_of, mut ndense) = (vec![u32::MAX; nterms], 0);
    for t in 0..nterms {
        if used[t] & 1 != 0 && sampled.iter().map(|s| s.0[t]).sum::<u32>() * 32 >= seen {
            (dense_of[t], ndense) = (ndense, ndense + 1);
        }
    }
    let wp = n_docs.div_ceil(64);
    let bits: Vec<AtomicU64> = (0..ndense as usize * wp)
        .map(|_| AtomicU64::new(0))
        .collect();

    let scanned = par(
        CHUNKS,
        || vec![0u32; nkeys],
        |last, c| {
            let (mut doc, mut base, mut prev) = (0, 0, u32::MAX);
            let (mut pairs, mut offs) = (Vec::new(), Vec::new());
            // A pair is stored in four bytes: the key id above DOC_BITS bits, the document's place in the chunk below.
            let mut note = |id: u32, doc: u32, base: u32, pairs: &mut Vec<u32>| {
                if last[id as usize] != doc + 1 {
                    last[id as usize] = doc + 1;
                    assert!(doc - base < 1 << DOC_BITS);
                    pairs.push(id << DOC_BITS | (doc - base));
                }
            };
            tokens(docs, cut(c, CHUNKS), cut(c + 1, CHUNKS), |at, len| {
                let hit = if len > 32 {
                    Err(0)
                } else {
                    let k = key(docs, at, len);
                    terms.find(hash(&k), &k)
                };
                if at == 0 || docs[at - 1] == b'\n' {
                    doc = docs[at..at + len]
                        .iter()
                        .fold(0, |x, &d| x * 10 + (d - b'0') as u32);
                    if offs.is_empty() {
                        base = doc;
                    }
                    offs.push(at as u32);
                    prev = u32::MAX;
                } else if let Ok(t) = hit {
                    if used[t as usize] & 1 != 0 {
                        match dense_of[t as usize] {
                            u32::MAX => note(t, doc, base, &mut pairs),
                            d => {
                                let word = d as usize * wp + doc as usize / 64;
                                bits[word].fetch_or(1 << (doc % 64), Relaxed);
                            }
                        }
                    }
                    if used[t as usize] & 2 != 0 {
                        if let (true, Ok(g)) = (
                            prev != u32::MAX,
                            grams.find(pair(prev, t).wrapping_mul(M), &pair(prev, t)),
                        ) {
                            note(nterms as u32 + g, doc, base, &mut pairs);
                        }
                        prev = t;
                    } else {
                        prev = u32::MAX;
                    }
                } else {
                    prev = u32::MAX;
                }
                true
            });
            (pairs, offs, base)
        },
    );

    // Posting lists in one array. Per chunk and term we know the count, so each chunk scatters into its own
    // disjoint slots and every list comes out sorted without a sort.
    let mut place = par(
        CHUNKS,
        || (),
        |_, c| {
            let mut row = vec![0u32; nkeys];
            for &p in &scanned[c].0 {
                row[(p >> DOC_BITS) as usize] += 1;
            }
            row
        },
    );
    let mut start = vec![0u32; nkeys + 1];
    for t in 0..nkeys {
        let mut at = start[t];
        for row in place.iter_mut() {
            (row[t], at) = (at, at + row[t]);
        }
        start[t + 1] = at;
    }
    let post: Vec<AtomicU32> = (0..start[nkeys]).map(|_| AtomicU32::new(0)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            let mut at = place[c].clone();
            for &p in &scanned[c].0 {
                let t = (p >> DOC_BITS) as usize;
                post[at[t] as usize].store(scanned[c].2 + (p & (1 << DOC_BITS) - 1), Relaxed);
                at[t] += 1;
            }
        },
    );
    // SAFETY: AtomicU32 has the layout of u32, and the threads that wrote it have all been joined.
    let post: &[u32] =
        unsafe { std::slice::from_raw_parts(post.as_ptr() as *const u32, post.len()) };
    let doc_off: Vec<u32> = scanned.iter().flat_map(|s| s.1.iter().copied()).collect();
    let list = |t: u32| &post[start[t as usize] as usize..start[t as usize + 1] as usize];

    // Answer the queries, 64 per job.
    let lines = par(
        queries.len().div_ceil(64),
        || (Vec::new(), Vec::new(), Vec::new()),
        |(cur, tmp, dense), j| {
            let mut out = Vec::new();
            for query in &queries[j * 64..((j + 1) * 64).min(queries.len())] {
                cur.clear();
                if query.phrase {
                    // Documents holding every adjacent pair of the phrase; two terms need no more than that.
                    let mut order: Vec<_> =
                        query.pairs.iter().map(|&g| nterms as u32 + g).collect();
                    order.sort_by_key(|&t| list(t).len());
                    cur.extend_from_slice(list(order[0]));
                    for &t in &order[1..] {
                        tmp.clear();
                        combine(b'A', cur, list(t), tmp);
                        std::mem::swap(cur, tmp);
                    }
                    let words: Vec<_> = query
                        .terms
                        .iter()
                        .map(|&t| terms.keys[t as usize])
                        .collect();
                    if words.len() > 2 {
                        cur.retain(|&d| {
                            let from = doc_off[d as usize] as usize;
                            let end = doc_off
                                .get(d as usize + 1)
                                .map_or(docs.len(), |&next| next as usize)
                                - 1;
                            let tab = docs[from..].iter().position(|&c| c == b'\t').unwrap();
                            has_phrase(docs, from + tab + 1, end, &words)
                        });
                    }
                } else {
                    let bits_of = |t: u32| &bits[dense_of[t as usize] as usize * wp..][..wp];
                    let first = query.terms[0];
                    match dense_of[first as usize] {
                        u32::MAX => cur.extend_from_slice(list(first)),
                        _ => decode(bits_of(first), cur),
                    }
                    for (k, &op) in query.ops.iter().enumerate() {
                        let t = query.terms[k + 1];
                        if dense_of[t as usize] == u32::MAX || op == b'O' {
                            let other = if dense_of[t as usize] == u32::MAX {
                                list(t)
                            } else {
                                dense.clear();
                                decode(bits_of(t), dense);
                                &dense[..]
                            };
                            tmp.clear();
                            combine(op, cur, other, tmp);
                            std::mem::swap(cur, tmp);
                        } else {
                            let b = bits_of(t);
                            cur.retain(|&x| {
                                (b[x as usize / 64].load(Relaxed) >> (x % 64) & 1 == 1)
                                    == (op == b'A')
                            });
                        }
                    }
                }
                put(&mut out, cur.len() as u64);
                out.push(b' ');
                put(&mut out, cur.iter().map(|&d| d as u64).sum());
                for &d in cur.iter().take(10) {
                    out.push(b' ');
                    put(&mut out, d as u64);
                }
                out.push(b'\n');
            }
            out
        },
    );
    std::fs::write("/out/results.txt", lines.concat()).expect("write output");
}
