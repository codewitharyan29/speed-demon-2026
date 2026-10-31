use std::arch::x86_64::*;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 64;
const ONES: u64 = 0x0101010101010101;
const DOC_BITS: u32 = 14;
const BLOOM_BITS: u32 = 20;
const NO_BITS: u32 = 0xFFF;
/// Tokens handed over at a time: enough independent table lookups to keep many cache misses in flight.
const BATCH: usize = 48;

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
/// so uneven jobs balance themselves. `init` builds each thread's private state. Results come in no set order.
fn par<S, R: Send>(
    jobs: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let (mut state, mut got) = (init(), Vec::new());
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return got;
                        }
                        got.push(f(&mut state, j));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect()
    })
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

/// A token of the corpus: the document it is in, where it starts and how long it is.
#[derive(Clone, Copy, Default)]
struct Tok {
    doc: u32,
    at: u32,
    len: u32,
}

/// Bit j of the first result is set if byte j of b[at..] is an ASCII letter or digit, bit j of the second if it is a
/// newline, for 64 bytes at a time. Bytes at or after `to` are neither.
fn masks(b: &[u8], at: usize, to: usize) -> (u64, u64) {
    let mut tail = [0u8; 64];
    let block = match b.get(at..at + 64) {
        Some(s) if at + 64 <= to => s,
        _ => {
            tail[..to - at].copy_from_slice(&b[at..to]);
            &tail[..]
        }
    };
    // SAFETY: `block` is 64 bytes, and AVX2 is enabled for this build (target-cpu=native in .cargo/config.toml).
    // Bytes of 0x80 and above are negative as i8, so every signed range test below rejects them.
    unsafe {
        let k = |c: i8| _mm256_set1_epi8(c);
        let half = |p: *const u8| {
            let v = _mm256_loadu_si256(p as *const __m256i);
            let lower = _mm256_or_si256(v, k(0x20));
            let letter = _mm256_and_si256(
                _mm256_cmpgt_epi8(lower, k(0x60)),
                _mm256_cmpgt_epi8(k(0x7B), lower),
            );
            let digit =
                _mm256_and_si256(_mm256_cmpgt_epi8(v, k(0x2F)), _mm256_cmpgt_epi8(k(0x3A), v));
            let alnum = _mm256_movemask_epi8(_mm256_or_si256(letter, digit)) as u32;
            (
                alnum,
                _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, k(10))) as u32,
            )
        };
        let (a0, n0) = half(block.as_ptr());
        let (a1, n1) = half(block.as_ptr().add(32));
        (a0 as u64 | (a1 as u64) << 32, n0 as u64 | (n1 as u64) << 32)
    }
}

/// Splits b[from..to] into tokens (maximal runs of letters and digits), 64 bytes at a time, and hands them to `f`
/// in batches until `f` returns false. If `line_start`, `from` begins a line, whose first token (the document id)
/// is skipped, as is the first token of every later line; `doc` numbers the lines from `first_doc`, and the offset
/// of every line start goes to `lines`. Token edges are where the letter-or-digit mask flips between neighbouring
/// bytes; they alternate start, end, start, ...
fn scan(
    b: &[u8],
    (from, to): (usize, usize),
    first_doc: u32,
    line_start: bool,
    lines: &mut Vec<u32>,
    mut f: impl FnMut(&[Tok]) -> bool,
) {
    let (mut batch, mut n) = ([Tok::default(); BATCH + 33], 0);
    let (mut doc, mut prev_alnum, mut prev_nl) = (first_doc, 0u64, line_start as u64);
    let (mut start, mut start_doc, mut skip, mut open) = (0, 0, false, false);
    if line_start && from < to {
        lines.push(from as u32);
    }
    for at in (from..to).step_by(64) {
        let (m, nl) = masks(b, at, to);
        let after_nl = nl << 1 | prev_nl;
        let mut edges = m ^ (m << 1 | prev_alnum);
        (prev_alnum, prev_nl) = (m >> 63, nl >> 63);
        while edges != 0 {
            let p = edges.trailing_zeros();
            edges &= edges - 1;
            if open {
                let len = at + p as usize - start;
                batch[n] = Tok {
                    doc: start_doc,
                    at: start as u32,
                    len: len as u32,
                };
                n += !skip as usize;
            } else {
                start = at + p as usize;
                start_doc = doc + (nl & ((1u64 << p) - 1)).count_ones();
                skip = after_nl >> p & 1 == 1;
            }
            open = !open;
        }
        let mut newlines = nl;
        while newlines != 0 {
            let next = at + newlines.trailing_zeros() as usize + 1;
            newlines &= newlines - 1;
            if next < to {
                lines.push(next as u32);
            }
        }
        doc += nl.count_ones();
        if n >= BATCH {
            if !f(&batch[..n]) {
                return;
            }
            n = 0;
        }
    }
    if open && !skip {
        batch[n] = Tok {
            doc: start_doc,
            at: start as u32,
            len: (to - start) as u32,
        };
        n += 1;
    }
    if n > 0 {
        f(&batch[..n]);
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

/// A token's first sixteen bytes, lower-cased, as two words, with no branch on its length: bytes past the token are
/// masked off.
fn short_key(b: &[u8], at: usize, len: usize) -> (u64, u64) {
    let both = match b.get(at..at + 16) {
        Some(s) => u128::from_le_bytes(s.try_into().unwrap()),
        None => load(b, at) as u128 | (load(b, at + 8) as u128) << 64,
    };
    let bits = 8 * len.min(31) as u32;
    // SAFETY: BMI2 is enabled for this build (target-cpu=native). `bzhi` keeps the low `n` bits, all of them if n >= 64.
    let cut = |w: u128, n: u32| unsafe { _bzhi_u64(w as u64 | ONES * 0x20, n) };
    (cut(both, bits), cut(both >> 64, bits.saturating_sub(64)))
}

/// One cache line of the scan's table: the two key words and the metadata of three terms, so a lookup reads one line.
#[repr(C, align(64))]
#[derive(Clone, Copy, Default)]
struct Bucket {
    k0: [u64; 3],
    k1: [u64; 3],
    meta: [u32; 4],
}

/// The table the scan probes for every term of up to 16 bytes, which is all of them in practice (the longest token
/// in a generated corpus has 17). A lookup compares a whole bucket at once; a full bucket spills into the next one,
/// and only then, as `spilled` records, does a lookup go on to the next bucket.
struct Probe {
    buckets: Vec<Bucket>,
    spilled: Vec<u64>,
    shift: u32,
}

impl Probe {
    fn new(keys: &[[u64; 4]], meta: impl Fn(usize) -> u32) -> Probe {
        let bits = (keys.len() / 2).next_power_of_two().trailing_zeros().max(1);
        let n = (1 << bits) + 64;
        let mut p = Probe {
            buckets: vec![Bucket::default(); n],
            spilled: vec![0; n / 64 + 1],
            shift: 64 - bits,
        };
        for (id, k) in keys.iter().enumerate().filter(|(_, k)| k[2] == 0) {
            let mut i = p.bucket((k[0], k[1]));
            while let None = p.buckets[i].k0.iter().position(|&x| x == 0) {
                p.spilled[i / 64] |= 1 << (i % 64);
                i += 1;
            }
            let b = &mut p.buckets[i];
            let lane = b.k0.iter().position(|&x| x == 0).unwrap();
            (b.k0[lane], b.k1[lane], b.meta[lane]) = (k[0], k[1], meta(id));
        }
        p
    }

    fn bucket(&self, (k0, k1): (u64, u64)) -> usize {
        ((k0.wrapping_mul(M) ^ k1.wrapping_mul(0xC2B2AE3D27D4EB4F)) >> self.shift) as usize
    }

    /// The metadata of the term whose key words are `k`, or `miss`.
    fn find(&self, k: (u64, u64), miss: u32) -> u32 {
        let mut i = self.bucket(k);
        loop {
            let b = &self.buckets[i];
            // SAFETY: AVX2 is enabled for this build. Each load reads 32 bytes inside the 64-byte bucket: three key
            // words and one word after them, whose lane the mask drops.
            let hit = unsafe {
                let words = |p: *const u64, w: u64| {
                    let v = _mm256_loadu_si256(p as *const __m256i);
                    _mm256_cmpeq_epi64(v, _mm256_set1_epi64x(w as i64))
                };
                let both = _mm256_and_si256(words(b.k0.as_ptr(), k.0), words(b.k1.as_ptr(), k.1));
                _mm256_movemask_pd(_mm256_castsi256_pd(both)) as usize & 7
            };
            let found = b.meta[hit.trailing_zeros() as usize & 3];
            if hit != 0 || self.spilled[i / 64] >> (i % 64) & 1 == 0 {
                return if hit != 0 { found } else { miss };
            }
            i += 1;
        }
    }
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

    /// Rebuilds the table at three times the number of keys it holds, so lookups touch less memory.
    fn shrink(&mut self, hash: impl Fn(&K) -> u64) {
        self.bits = (self.keys.len() * 3).next_power_of_two().trailing_zeros();
        self.slots = vec![0; 1 << self.bits];
        for id in 0..self.keys.len() {
            let slot = self.find(hash(&self.keys[id]), &self.keys[id]).unwrap_err();
            self.slots[slot] = id as u32 + 1;
        }
    }

    /// Asks for the cache line of the slot where a key with hash `h` would be looked for.
    fn prefetch(&self, h: u64) {
        let slot = self
            .slots
            .as_ptr()
            .wrapping_add((h >> (64 - self.bits)) as usize);
        // SAFETY: a prefetch is only a hint and never faults; the address is inside `slots`.
        unsafe { _mm_prefetch(slot as *const i8, _MM_HINT_T0) };
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
/// Everything is stored inline (a query has at most 8 terms), so the per-chunk pass over all queries reads memory in
/// order instead of chasing a pointer per query.
struct Query {
    phrase: bool,
    terms: Few<u32, 8>,
    ops: Few<u8, 8>,
    pairs: Few<u32, 7>,
}

/// A list of at most N items, stored inline.
#[derive(Clone, Copy)]
struct Few<T, const N: usize> {
    len: u8,
    items: [T; N],
}

impl<T: Copy + Default, const N: usize> Few<T, N> {
    fn new(s: &[T]) -> Self {
        let mut items = [T::default(); N];
        items[..s.len()].copy_from_slice(s);
        Few {
            len: s.len() as u8,
            items,
        }
    }
}

impl<T, const N: usize> std::ops::Deref for Few<T, N> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items[..self.len as usize]
    }
}

/// Merges sorted `a` with sorted `b` under `op`: AND keeps ids in both, OR in either, NOT in `a` only.
fn combine<T: Copy + Ord>(op: u8, a: &[T], b: &[T], out: &mut Vec<T>) {
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
fn has_phrase(b: &[u8], range: (usize, usize), words: &[[u64; 4]]) -> bool {
    let (mut state, mut found) = (0u8, false);
    scan(b, range, 0, false, &mut Vec::new(), |batch| {
        for t in batch {
            let k = if t.len <= 32 {
                key(b, t.at as usize, t.len as usize)
            } else {
                [u64::MAX; 4]
            };
            let here = words
                .iter()
                .enumerate()
                .fold(0u8, |m, (j, w)| m | ((*w == k) as u8) << j);
            state = (state << 1 | 1) & here;
            found = state >> (words.len() - 1) & 1 == 1;
            if found {
                return false;
            }
        }
        true
    });
    found
}

/// What a query operator combines the running set with, within one chunk: a term's list of chunk-local ids, or its
/// bitset over the chunk's documents.
enum Operand<'a> {
    List(&'a [u16]),
    Bits(&'a [u64]),
}

fn has(bits: &[u64], d: u16) -> bool {
    bits[d as usize / 64] >> (d % 64) & 1 == 1
}

/// The running set of a query within one chunk: a sorted list of chunk-local ids, or a bitset over the chunk's
/// documents once an OR has brought in a dense term. The buffers are reused from query to query.
#[derive(Default)]
struct Run {
    dense: bool,
    list: Vec<u16>,
    tmp: Vec<u16>,
    bits: Vec<u64>,
}

impl Run {
    fn start(&mut self, x: &Operand) {
        (self.list.clear(), self.bits.clear());
        self.dense = matches!(x, Operand::Bits(_));
        match x {
            Operand::List(l) => self.list.extend_from_slice(l),
            Operand::Bits(b) => self.bits.extend_from_slice(b),
        }
    }

    /// `self op x`, where `op` is b'A' (AND), b'O' (OR) or b'N' (NOT).
    fn apply(&mut self, op: u8, x: &Operand) {
        let Run {
            dense,
            list,
            tmp,
            bits,
        } = self;
        match (*dense, x) {
            (false, _) if list.is_empty() && op != b'O' => {}
            (false, Operand::List(b)) => {
                tmp.clear();
                combine(op, list, b, tmp);
                std::mem::swap(list, tmp);
            }
            // A list against a bitset: test each id, unless the union needs the bitset anyway.
            (false, Operand::Bits(b)) if op != b'O' => list.retain(|&d| has(b, d) == (op == b'A')),
            (false, Operand::Bits(b)) => {
                (bits.clear(), bits.extend_from_slice(b));
                list.iter()
                    .for_each(|&d| bits[d as usize / 64] |= 1 << (d % 64));
                *dense = true;
            }
            // A bitset against a list: the intersection is as small as the list.
            (true, Operand::List(b)) if op == b'A' => {
                list.clear();
                list.extend(b.iter().filter(|&&d| has(bits, d)));
                *dense = false;
            }
            (true, Operand::List(b)) => {
                for &d in *b {
                    let (w, bit) = (d as usize / 64, 1u64 << (d % 64));
                    bits[w] = if op == b'O' {
                        bits[w] | bit
                    } else {
                        bits[w] & !bit
                    };
                }
            }
            (true, Operand::Bits(b)) => {
                for (x, y) in bits.iter_mut().zip(*b) {
                    *x = match op {
                        b'A' => *x & y,
                        b'O' => *x | y,
                        _ => *x & !y,
                    };
                }
            }
        }
    }

    /// The set's size and the sum of its ids.
    fn size_sum(&self) -> (u64, u64) {
        if self.dense {
            // The positions of a word's set bits add up to the sum, over each bit k of a position, of 2^k times the
            // number of set bits whose position has bit k set: six popcounts instead of one step per set bit.
            const HAS: [u64; 6] = [
                0xAAAA_AAAA_AAAA_AAAA,
                0xCCCC_CCCC_CCCC_CCCC,
                0xF0F0_F0F0_F0F0_F0F0,
                0xFF00_FF00_FF00_FF00,
                0xFFFF_0000_FFFF_0000,
                0xFFFF_FFFF_0000_0000,
            ];
            let (mut count, mut sum) = (0, 0);
            for (i, &w) in self.bits.iter().enumerate() {
                let n = w.count_ones() as u64;
                let within: u64 = (0..6)
                    .map(|k| ((w & HAS[k]).count_ones() as u64) << k)
                    .sum();
                (count, sum) = (count + n, sum + (i * 64) as u64 * n + within);
            }
            (count, sum)
        } else {
            (
                self.list.len() as u64,
                self.list.iter().map(|&d| d as u64).sum(),
            )
        }
    }

    /// Appends the set's (up to) ten smallest ids, plus `base`.
    fn smallest(&self, base: u32, out: &mut Vec<u32>) {
        if !self.dense {
            return out.extend(self.list.iter().take(10).map(|&d| base + d as u32));
        }
        for (i, &word) in self.bits.iter().enumerate() {
            let mut w = word;
            if out.len() >= 20 {
                break;
            }
            while w != 0 && out.len() < 20 {
                out.push(base + (i * 64) as u32 + w.trailing_zeros());
                w &= w - 1;
            }
        }
    }
}

/// One thread's buffers for a chunk, reused from chunk to chunk.
#[derive(Default)]
struct Chunk {
    pairs: Vec<u32>,
    offs: Vec<u32>,
    start: Vec<u32>,
    post: Vec<u16>,
    rows: Vec<u64>,
    order: Vec<u32>,
    run: Run,
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
    // Parse every query; the keys of all terms go into one list. They are read from the whole mapped file, so that
    // the eight-byte loads never run off the end of a short slice.
    let q = map("/data/queries.txt");
    let mut keys: Vec<[u64; 4]> = Vec::new();
    let shapes: Vec<(bool, usize, Few<u8, 8>)> = q
        .split(|&c| c == b'\n')
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.split(|&c| c == b' ');
            let phrase = parts.next().unwrap() == b"PHRASE";
            let (mut ops, before) = (Vec::new(), keys.len());
            for (k, w) in parts.enumerate() {
                if !phrase && k % 2 == 1 {
                    ops.push(w[0]);
                } else {
                    let w = w.strip_prefix(b"\"").unwrap_or(w);
                    let w = w.strip_suffix(b"\"").unwrap_or(w);
                    keys.push(key(q, w.as_ptr() as usize - q.as_ptr() as usize, w.len()));
                }
            }
            (phrase, keys.len() - before, Few::new(&ops))
        })
        .collect();

    // Number the terms, sixteen at a time: the table slots of a batch are fetched together, so their cache misses
    // overlap. `used` says how a term is used: bit 0 in a BOOL query (needs a posting list), bit 2 in a phrase; bit 1,
    // set later, if the scan tracks pairs it starts or ends.
    let (mut terms, mut grams, mut ids) =
        (Table::new(keys.len()), Table::new(keys.len()), Vec::new());
    for batch in keys.chunks(16) {
        batch.iter().for_each(|k| terms.prefetch(hash(k)));
        ids.extend(batch.iter().map(|&k| terms.intern(hash(&k), k)));
    }
    let (mut used, mut at) = (vec![0u8; terms.keys.len()], 0);
    let mut queries: Vec<Query> = (shapes.into_iter())
        .map(|(phrase, n, ops)| {
            let t = &ids[at..at + n];
            at += n;
            t.iter()
                .for_each(|&id| used[id as usize] |= if phrase { 4 } else { 1 });
            Query {
                phrase,
                terms: Few::new(t),
                ops,
                pairs: Few::new(&[]),
            }
        })
        .collect();
    drop((keys, ids));
    terms.shrink(hash);
    let nterms = terms.keys.len();

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
    // The id at the start of a line, which is also its number.
    let first_doc = |at: usize| {
        let digits = docs[at..].iter().take_while(|c| c.is_ascii_digit());
        digits.fold(0, |x, &d| x * 10 + (d - b'0') as u32)
    };
    // The id of the last line gives the document count.
    let tail = &docs[..docs.len() - 1];
    let last_line = tail.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
    let n_docs = first_doc(last_line) as usize + 1;

    // Terms in over 1/32 of the documents are kept as bitsets (a list that long would not be smaller), the rest as
    // lists. The first 12/576 of the corpus is a good enough sample to choose; a wrong guess only costs speed.
    const SAMPLE: usize = 12;
    let sampled = par(
        SAMPLE,
        || vec![0u32; nterms],
        |stamp, j| {
            let range = (cut(j, 48 * SAMPLE), cut(j + 1, 48 * SAMPLE));
            let (mut lines, mut df) = (Vec::new(), vec![0u32; nterms]);
            scan(docs, range, first_doc(range.0), true, &mut lines, |batch| {
                for t in batch.iter().filter(|t| t.len <= 32) {
                    let k = key(docs, t.at as usize, t.len as usize);
                    if let Ok(id) = terms.find(hash(&k), &k) {
                        if used[id as usize] != 0 && stamp[id as usize] != t.doc + 1 {
                            stamp[id as usize] = t.doc + 1;
                            df[id as usize] += 1;
                        }
                    }
                }
                true
            });
            (df, lines.len() as u32)
        },
    );
    let seen: u32 = sampled.iter().map(|s| s.1).sum();
    let df: Vec<u32> = (0..nterms)
        .map(|t| sampled.iter().map(|s| s.0[t]).sum())
        .collect();
    drop(sampled);
    let (mut dense_of, mut ndense) = (vec![u32::MAX; nterms], 0);
    for t in 0..nterms {
        if used[t] & 1 != 0 && df[t] * 32 >= seen {
            (dense_of[t], ndense) = (ndense, ndense + 1);
        }
    }

    // The adjacent pairs each phrase needs postings for. A phrase with a rare term (in under 1/200 of the sampled
    // documents) needs just one: that term with its rarer neighbour, as its few candidates are verified anyway. A
    // phrase of common words keeps all its pairs, as only their intersection is short. The terms of those pairs are
    // the ones the scan tracks pairs for, so most common words never get looked at as half of a pair.
    let df = |t: u32| df[t as usize];
    for query in queries.iter_mut().filter(|q| q.phrase) {
        let t = &query.terms;
        let rare = (0..t.len()).min_by_key(|&i| df(t[i])).unwrap();
        let near = (rare.saturating_sub(1)..=rare).filter(|&i| i + 1 < t.len());
        let chosen: Vec<usize> = if df(t[rare]) * 200 < seen {
            near.min_by_key(|&i| df(t[i]) + df(t[i + 1]))
                .into_iter()
                .collect()
        } else {
            (0..t.len() - 1).collect()
        };
        for &i in &chosen {
            used[t[i] as usize] |= 2;
            used[t[i + 1] as usize] |= 2;
        }
        let key_of = |i: usize| pair(t[i], t[i + 1]);
        let pairs: Vec<u32> = chosen
            .iter()
            .map(|&i| grams.intern(key_of(i).wrapping_mul(M), key_of(i)))
            .collect();
        query.pairs = Few::new(&pairs);
    }
    grams.shrink(|k| k.wrapping_mul(M));
    let nkeys = nterms + grams.keys.len();
    assert!(nkeys < 1 << (32 - DOC_BITS));

    // What the scan probes. Per term: its id, whether BOOL and phrase queries use it, and its bitset if it has one.
    // A bloom filter over the phrase pairs lets the scan skip most pair lookups.
    assert!(ndense < NO_BITS && nkeys + 1 < 1 << 18);
    let meta = |t: usize| t as u32 | (used[t] as u32 & 3) << 18 | dense_of[t].min(NO_BITS) << 20;
    let miss = nkeys as u32 | NO_BITS << 20;
    let probe = Probe::new(&terms.keys, meta);
    let mut bloom = vec![0u64; 1 << (BLOOM_BITS - 6)];
    for k in &grams.keys {
        let h = (k.wrapping_mul(M) >> (64 - BLOOM_BITS)) as usize;
        bloom[h >> 6] |= 1 << (h & 63);
    }

    // Each chunk is scanned, indexed on its own, and then answers every query for its own documents. Chunks hold
    // consecutive documents, so nothing that grows with the corpus is ever kept: per query only the running count,
    // the sum of ids and the ten smallest ids seen so far.
    let count: Vec<AtomicU32> = (0..queries.len()).map(|_| AtomicU32::new(0)).collect();
    let sum: Vec<AtomicU64> = (0..queries.len()).map(|_| AtomicU64::new(0)).collect();
    // The ten smallest ids of each query so far, and in the eleventh slot how many of the ten there are.
    let first: Vec<Mutex<[u32; 11]>> = (0..queries.len()).map(|_| Mutex::new([0; 11])).collect();
    par(CHUNKS, Chunk::default, |w, c| {
        let range = (cut(c, CHUNKS), cut(c + 1, CHUNKS));
        let base = first_doc(range.0);
        let end = if range.1 == docs.len() {
            n_docs as u32
        } else {
            first_doc(range.1)
        };
        assert!(end - base <= 1 << DOC_BITS);
        let wl = ((end - base) as usize).div_ceil(64);
        let Chunk {
            pairs,
            offs,
            start,
            post,
            rows,
            order,
            run,
        } = w;
        // `start` holds, per key, the last document it was recorded for during the scan (as id + 1), then where its
        // postings begin.
        (start.clear(), start.resize(nkeys + 1, 0));
        (pairs.clear(), offs.clear(), rows.clear());
        rows.resize(ndense as usize * wl, 0);
        let (mut prev, mut prev_doc, mut prev_phrase) = (0, 0, false);
        scan(docs, range, base, true, offs, |batch| {
            // Look every token of the batch up first: the loads are independent, so their cache misses overlap.
            // First every token's key and home slot, asking for the slot's cache line straight away, so the misses
            // of the whole batch overlap instead of being waited for one at a time.
            let (mut found, mut keys) = ([miss; BATCH + 33], [(0, 0); BATCH + 33]);
            for (k, t) in keys.iter_mut().zip(batch) {
                *k = short_key(docs, t.at as usize, t.len as usize);
                // SAFETY: a prefetch is only a hint and never faults; the addresses are inside the table.
                unsafe {
                    let i = probe.bucket(*k);
                    _mm_prefetch(probe.buckets.as_ptr().add(i) as *const i8, _MM_HINT_T0);
                }
            }
            for ((m, t), &k) in found.iter_mut().zip(batch).zip(&keys) {
                *m = probe.find(k, miss);
                // A token over 16 bytes (none in the generated data) is looked up in full.
                if t.len > 16 {
                    let k = key(docs, t.at as usize, (t.len as usize).min(32));
                    let id = terms.find(hash(&k), &k).ok().filter(|_| t.len <= 32);
                    *m = id.map_or(miss, |id| meta(id as usize));
                }
            }
            // Then act on them.
            for (m, t) in found.iter().zip(batch) {
                let (id, flags, d) = ((m & 0x3FFFF) as usize, m >> 18 & 3, m >> 20);
                let (doc, local) = (t.doc, t.doc - base);
                if flags & 1 != 0 && d != NO_BITS {
                    // A term with a bitset: set the document's bit.
                    rows[d as usize * wl + local as usize / 64] |= 1 << (local % 64);
                } else if flags & 1 != 0 && start[id] != doc + 1 {
                    // Any other BOOL term: a pair, once per document.
                    start[id] = doc + 1;
                    pairs.push((id as u32) << DOC_BITS | local);
                }
                // Two phrase terms in a row: their pair, once per document, if the filter does not rule it out.
                let phrase = flags & 2 != 0;
                let (pk, h) = (pair(prev, id as u32), pair(prev, id as u32).wrapping_mul(M));
                let bit = (h >> (64 - BLOOM_BITS)) as usize;
                if phrase
                    && prev_phrase
                    && prev_doc == doc
                    && bloom[bit >> 6] >> (bit & 63) & 1 == 1
                {
                    if let Ok(g) = grams.find(h, &pk) {
                        let g = nterms + g as usize;
                        if start[g] != doc + 1 {
                            start[g] = doc + 1;
                            pairs.push((g as u32) << DOC_BITS | local);
                        }
                    }
                }
                (prev, prev_doc, prev_phrase) = (id as u32, doc, phrase);
            }
            true
        });

        // The chunk's postings, by key, ascending: a counting sort placing pairs from the back.
        (start.clear(), start.resize(nkeys + 1, 0));
        pairs
            .iter()
            .for_each(|&p| start[(p >> DOC_BITS) as usize] += 1);
        for i in 1..=nkeys {
            start[i] += start[i - 1];
        }
        post.resize(pairs.len(), 0);
        for &p in pairs.iter().rev() {
            let t = (p >> DOC_BITS) as usize;
            start[t] -= 1;
            post[start[t] as usize] = (p & ((1 << DOC_BITS) - 1)) as u16;
        }
        let list = |t: u32| &post[start[t as usize] as usize..start[t as usize + 1] as usize];
        let operand = |t: u32| match dense_of[t as usize] {
            u32::MAX => Operand::List(list(t)),
            d => Operand::Bits(&rows[d as usize * wl..][..wl]),
        };

        // Every query, on this chunk's documents.
        for (q, query) in queries.iter().enumerate() {
            if query.phrase {
                // Documents holding every tracked pair of the phrase; two terms need no more than that.
                (
                    order.clear(),
                    order.extend(query.pairs.iter().map(|&g| nterms as u32 + g)),
                );
                order.sort_by_key(|&t| list(t).len());
                if list(order[0]).is_empty() {
                    continue;
                }
                run.start(&Operand::List(list(order[0])));
                for &t in &order[1..] {
                    run.apply(b'A', &Operand::List(list(t)));
                }
                if query.terms.len() > 2 && !run.list.is_empty() {
                    let words: Vec<_> = query
                        .terms
                        .iter()
                        .map(|&t| terms.keys[t as usize])
                        .collect();
                    run.list.retain(|&d| {
                        let from = offs[d as usize] as usize;
                        let end = offs
                            .get(d as usize + 1)
                            .map_or(range.1, |&next| next as usize)
                            - 1;
                        let tab = docs[from..].iter().position(|&c| c == b'\t').unwrap();
                        has_phrase(docs, (from + tab + 1, end), &words)
                    });
                }
            } else {
                let start = operand(query.terms[0]);
                if matches!(start, Operand::List(l) if l.is_empty()) && !query.ops.contains(&b'O') {
                    continue;
                }
                run.start(&start);
                for (k, &op) in query.ops.iter().enumerate() {
                    run.apply(op, &operand(query.terms[k + 1]));
                }
            }
            let (n, s) = run.size_sum();
            if n > 0 {
                count[q].fetch_add(n as u32, Relaxed);
                sum[q].fetch_add(s + n * base as u64, Relaxed);
                let mut f = first[q].lock().unwrap();
                if f[10] < 10 || f[9] > base {
                    (order.clear(), order.extend_from_slice(&f[..f[10] as usize]));
                    run.smallest(base, order);
                    order.sort_unstable();
                    order.truncate(10);
                    f[..order.len()].copy_from_slice(order);
                    f[10] = order.len() as u32;
                }
            }
        }
    });

    let mut out = Vec::new();
    for q in 0..queries.len() {
        put(&mut out, count[q].load(Relaxed) as u64);
        out.push(b' ');
        put(&mut out, sum[q].load(Relaxed));
        let f = first[q].lock().unwrap();
        for &d in &f[..f[10] as usize] {
            out.push(b' ');
            put(&mut out, d as u64);
        }
        out.push(b'\n');
    }
    std::fs::write("/out/results.txt", out).expect("write output");
}
