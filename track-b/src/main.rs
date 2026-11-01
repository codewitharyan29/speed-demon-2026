use std::arch::x86_64::*;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const ONES: u64 = 0x0101010101010101;
const DOC_BITS: u32 = 14;
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
                // SAFETY: n < BATCH + 32: the batch is handed over once it holds BATCH, and a block adds at most 32.
                *unsafe { batch.get_unchecked_mut(n) } = Tok {
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

/// The table that numbers every query term of up to 16 bytes, which is all of them in practice (the longest token in a
/// generated corpus has 17), and that the scan probes. A lookup compares a whole bucket at once; a full bucket spills
/// into the next one (wrapping around), and only then, as `spilled` records, does a lookup go on to the next bucket.
struct Probe {
    buckets: Vec<Bucket>,
    spilled: Vec<u64>,
    shift: u32,
}

impl Probe {
    /// An empty table with room for `max_keys` keys.
    fn new(max_keys: usize) -> Probe {
        let bits = (max_keys / 3 + 1)
            .next_power_of_two()
            .trailing_zeros()
            .max(1);
        Probe {
            buckets: vec![Bucket::default(); 1 << bits],
            spilled: vec![0; (1 << bits) / 64 + 1],
            shift: 64 - bits,
        }
    }

    /// The id of the key `k`; a new key is stored with id `next`, which then moves on.
    fn intern(&mut self, k: (u64, u64), next: &mut u32) -> u32 {
        let id = self.find(k, u32::MAX);
        if id != u32::MAX {
            return id;
        }
        let mut i = self.bucket(k);
        while let None = self.buckets[i].k0.iter().position(|&x| x == 0) {
            self.spilled[i / 64] |= 1 << (i % 64);
            i = (i + 1) % self.buckets.len();
        }
        let b = &mut self.buckets[i];
        let lane = b.k0.iter().position(|&x| x == 0).unwrap();
        (b.k0[lane], b.k1[lane], b.meta[lane], *next) = (k.0, k.1, *next, *next + 1);
        *next - 1
    }

    /// Asks for the cache line of bucket `i`.
    fn prefetch(&self, i: usize) {
        // SAFETY: a prefetch is only a hint and never faults; the address is inside the table.
        unsafe { _mm_prefetch(self.buckets.as_ptr().add(i) as *const i8, _MM_HINT_T0) };
    }

    fn bucket(&self, (k0, k1): (u64, u64)) -> usize {
        ((k0.wrapping_mul(M) ^ k1.wrapping_mul(0xC2B2AE3D27D4EB4F)) >> self.shift) as usize
    }

    /// The metadata of the term whose key words are `k`, or `miss`.
    fn find(&self, k: (u64, u64), miss: u32) -> u32 {
        self.find_from(self.bucket(k), k, miss)
    }

    /// The same, given the key's home bucket `i`.
    fn find_from(&self, mut i: usize, k: (u64, u64), miss: u32) -> u32 {
        loop {
            // SAFETY: bucket indexes are below the table's length (the hash's shift, and spills wrap around).
            let (b, spilled) = unsafe {
                (
                    self.buckets.get_unchecked(i),
                    *self.spilled.get_unchecked(i / 64),
                )
            };
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
            if hit != 0 || spilled >> (i % 64) & 1 == 0 {
                return if hit != 0 { found } else { miss };
            }
            i = (i + 1) % self.buckets.len();
        }
    }
}

/// A parsed query: term ids, and the operator byte (b'A', b'O' or b'N') between each neighbouring pair (none for a
/// phrase).
/// Everything is stored inline (a query has at most 8 terms), so the per-chunk pass over all queries reads memory in
/// order instead of chasing a pointer per query.
struct Query {
    phrase: bool,
    terms: Few<u32, 8>,
    ops: Few<u8, 8>,
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
    seq: Vec<u32>,
    hits: Vec<u32>,
    matched: Vec<u32>,
    pairs: Vec<u32>,
    offs: Vec<u32>,
    start: Vec<u32>,
    post: Vec<u16>,
    rows: Vec<u64>,
    order: Vec<u32>,
    run: Run,
}

/// Boundary i of n equal parts of `b`, moved to the start of the next line.
fn cut(b: &[u8], i: usize, n: usize) -> usize {
    match i {
        0 => 0,
        _ if i == n => b.len(),
        _ => {
            let at = (b.len() * i / n).saturating_sub(1);
            at + b[at..].iter().position(|&c| c == b'\n').unwrap() + 1
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
    // Parse every query, in line-aligned pieces at once; the keys of each piece's terms go into one list. They are read
    // from the whole mapped file, so that the eight-byte loads never run off the end of a short slice.
    let q = map("/data/queries.txt");
    let mut pieces = par(
        THREADS,
        || (),
        |_, j| {
            let mut keys: Vec<[u64; 4]> = Vec::new();
            let lines = q[cut(q, j, THREADS)..cut(q, j + 1, THREADS)].split(|&c| c == b'\n');
            let shapes: Vec<(bool, usize, Few<u8, 8>)> = (lines.filter(|l| !l.is_empty()))
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
            (j, keys, shapes)
        },
    );
    pieces.sort_unstable_by_key(|piece| piece.0);

    // Number the terms, sixteen at a time: the buckets of a batch are fetched together, so their cache misses overlap.
    // Terms of up to 16 bytes go straight into the table the scan probes, longer ones (rare) into a list. `used` says
    // how a term is used: bit 0 in a BOOL query (needs a posting list), bit 2 in a phrase; bit 1, set later, if it
    // triggers phrase checks.
    let all = pieces.iter().map(|piece| piece.1.len()).sum();
    let (mut probe, mut long, mut ids, mut next) = (Probe::new(all), Vec::new(), Vec::new(), 0);
    for batch in pieces.iter().flat_map(|piece| piece.1.chunks(16)) {
        batch
            .iter()
            .for_each(|k| probe.prefetch(probe.bucket((k[0], k[1]))));
        for k in batch {
            ids.push(match long.iter().position(|(l, _)| l == k) {
                _ if k[2] == 0 => probe.intern((k[0], k[1]), &mut next),
                Some(i) => long[i].1,
                None => (long.push((*k, next)), next += 1, next - 1).2,
            });
        }
    }
    let nterms = next as usize;
    let (mut used, mut at) = (vec![0u8; nterms], 0);
    let queries: Vec<Query> = (pieces.iter().flat_map(|piece| &piece.2))
        .map(|&(phrase, n, ops)| {
            let t = &ids[at..at + n];
            at += n;
            t.iter()
                .for_each(|&id| used[id as usize] |= if phrase { 4 } else { 1 });
            Query {
                phrase,
                terms: Few::new(t),
                ops,
            }
        })
        .collect();
    drop((pieces, ids));

    // Scan the corpus in newline-aligned chunks. A chunk yields (term, doc) pairs for the query terms only,
    // each once per document, in document order, plus the byte offset of each of its documents. The first token
    // of a line is the document id.
    let docs = map("/data/docs.txt");
    // The id at the start of a line, which is also its number.
    let first_doc = |at: usize| {
        let digits = docs[at..].iter().take_while(|c| c.is_ascii_digit());
        digits.fold(0, |x, &d| x * 10 + (d - b'0') as u32)
    };
    // The id of the last line gives the document count.
    let tail = &docs[..docs.len() - 1];
    let last_line = tail.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
    let n_docs = first_doc(last_line) as usize + 1;
    // The id of a token over 16 bytes (none in the generated data), looked up in full in `long`.
    let long_id = |t: &Tok| {
        let k = key(docs, t.at as usize, (t.len as usize).min(32));
        long.iter()
            .find(|(l, _)| *l == k && t.len <= 32)
            .map(|e| e.1)
    };

    // Terms in over 1/32 of the documents are kept as bitsets (a list that long would not be smaller), the rest as
    // lists. The first 12/1152 of the corpus is a good enough sample to choose; a wrong guess only costs speed.
    const SAMPLE: usize = 12;
    let sampled = par(
        SAMPLE,
        || vec![0u32; nterms],
        |stamp, j| {
            let range = (cut(docs, j, 96 * SAMPLE), cut(docs, j + 1, 96 * SAMPLE));
            let (mut lines, mut df) = (Vec::new(), vec![0u32; nterms]);
            scan(docs, range, first_doc(range.0), true, &mut lines, |batch| {
                for t in batch {
                    let id = match t.len {
                        0..=16 => {
                            probe.find(short_key(docs, t.at as usize, t.len as usize), u32::MAX)
                        }
                        _ => long_id(t).unwrap_or(u32::MAX),
                    } as usize;
                    if id != u32::MAX as usize && stamp[id] != t.doc + 1 {
                        (stamp[id], df[id]) = (t.doc + 1, df[id] + 1);
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

    // Each phrase is checked where its rarest term occurs: the scan keeps each document's term ids in order, and at the
    // end of the document compares the phrase around every occurrence of that term. `triggers` lists, per term from
    // `tstart[term]`, the phrases it triggers, as query << 3 | the term's place in the phrase.
    let df = |t: u32| df[t as usize];
    let mut by_term: Vec<(u32, u32)> = Vec::new();
    for (q, query) in queries.iter().enumerate().filter(|(_, q)| q.phrase) {
        let t = &query.terms;
        let i = (0..t.len()).min_by_key(|&i| df(t[i])).unwrap();
        used[t[i] as usize] |= 2;
        by_term.push((t[i], (q as u32) << 3 | i as u32));
    }
    by_term.sort_unstable();
    let mut tstart = vec![0u32; nterms + 1];
    by_term
        .iter()
        .for_each(|&(t, _)| tstart[t as usize + 1] += 1);
    for t in 0..nterms {
        tstart[t + 1] += tstart[t];
    }
    let triggers: Vec<u32> = by_term.iter().map(|e| e.1).collect();
    // The phrases found in one document (`seq` its term ids, `hits` where its triggers are), as query << DOC_BITS | doc.
    let check = |seq: &[u32], hits: &[u32], local: u32, found: &mut Vec<u32>| {
        for &at in hits {
            let t = seq[at as usize] as usize;
            for &e in &triggers[tstart[t] as usize..tstart[t + 1] as usize] {
                let (words, from) = (
                    &queries[e as usize >> 3].terms,
                    (at as usize).wrapping_sub(e as usize & 7),
                );
                if seq.get(from..from.wrapping_add(words.len())) == Some(&words[..]) {
                    found.push((e >> 3) << DOC_BITS | local);
                }
            }
        }
    };
    let nkeys = nterms;
    assert!(nkeys < 1 << (32 - DOC_BITS));

    // What the scan probes. Per term: its id, whether BOOL queries use it, whether it triggers phrase checks, and its
    // bitset if it has one.
    assert!(ndense < NO_BITS && nkeys + 1 < 1 << 18);
    let meta = |t: usize| t as u32 | (used[t] as u32 & 3) << 18 | dense_of[t].min(NO_BITS) << 20;
    let miss = nkeys as u32 | NO_BITS << 20;
    for b in probe.buckets.iter_mut() {
        for lane in (0..3).filter(|&lane| b.k0[lane] != 0) {
            b.meta[lane] = meta(b.meta[lane] as usize);
        }
    }

    // Each chunk is scanned, indexed on its own, and then answers every query for its own documents. Chunks hold
    // consecutive documents, so nothing that grows with the corpus is ever kept: per query only the running count,
    // the sum of ids and the ten smallest ids seen so far.
    let count: Vec<AtomicU32> = (0..queries.len()).map(|_| AtomicU32::new(0)).collect();
    let sum: Vec<AtomicU64> = (0..queries.len()).map(|_| AtomicU64::new(0)).collect();
    // The ten smallest ids of each query so far, and in the eleventh slot how many of the ten there are.
    let first: Vec<Mutex<[u32; 11]>> = (0..queries.len()).map(|_| Mutex::new([0; 11])).collect();
    par(CHUNKS, Chunk::default, |w, c| {
        let range = (cut(docs, c, CHUNKS), cut(docs, c + 1, CHUNKS));
        let base = first_doc(range.0);
        let end = if range.1 == docs.len() {
            n_docs as u32
        } else {
            first_doc(range.1)
        };
        assert!(end - base <= 1 << DOC_BITS);
        let wl = ((end - base) as usize).div_ceil(64);
        let Chunk {
            seq,
            hits,
            matched,
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
        (pairs.clear(), offs.clear(), rows.clear(), matched.clear());
        rows.resize(ndense as usize * wl, 0);
        let mut cur = base;
        (seq.clear(), hits.clear());
        // A batch's keys (with their home bucket) and what they look up, made once per chunk.
        let (mut found, mut keys) = ([miss; BATCH + 33], [((0, 0), 0); BATCH + 33]);
        scan(docs, range, base, true, offs, |batch| {
            // Look every token of the batch up first: the loads are independent, so their cache misses overlap.
            // First every token's key and home slot, asking for the slot's cache line straight away, so the misses
            // of the whole batch overlap instead of being waited for one at a time.
            for (k, t) in keys.iter_mut().zip(batch) {
                let k2 = short_key(docs, t.at as usize, t.len as usize);
                *k = (k2, probe.bucket(k2));
                probe.prefetch(k.1);
            }
            for ((m, t), &k) in found.iter_mut().zip(batch).zip(keys.iter()) {
                *m = probe.find_from(k.1, k.0, miss);
                // A token over 16 bytes (none in the generated data) is looked up in full.
                if t.len > 16 {
                    *m = long_id(t).map_or(miss, |id| meta(id as usize));
                }
            }
            // Then act on them.
            for (m, t) in found.iter().zip(batch) {
                let (id, flags, d) = ((m & 0x3FFFF) as usize, m >> 18 & 3, m >> 20);
                let (doc, local) = (t.doc, t.doc - base);
                // SAFETY: a term with a bitset has d < ndense, and local < the chunk's documents <= 64 * wl; every
                // id is at most nkeys, and `start` has nkeys + 1 entries.
                unsafe {
                    if flags & 1 != 0 && d != NO_BITS {
                        // A term with a bitset: set the document's bit.
                        *rows.get_unchecked_mut(d as usize * wl + local as usize / 64) |=
                            1 << (local % 64);
                    } else if flags & 1 != 0 && *start.get_unchecked(id) != doc + 1 {
                        // Any other BOOL term: a pair, once per document.
                        *start.get_unchecked_mut(id) = doc + 1;
                        pairs.push((id as u32) << DOC_BITS | local);
                    }
                }
                // The document's term ids in order, and where its phrase triggers are.
                if doc != cur {
                    check(seq, hits, cur - base, matched);
                    (seq.clear(), hits.clear(), cur = doc);
                }
                if flags & 2 != 0 {
                    hits.push(seq.len() as u32);
                }
                seq.push(id as u32);
            }
            true
        });
        check(seq, hits, cur - base, matched);

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

        // A query's result on this chunk goes into its running count, sum and ten smallest ids.
        let report = |q: usize, run: &Run, order: &mut Vec<u32>| {
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
        };
        // The phrases found while scanning, each document once.
        (matched.sort_unstable(), matched.dedup());
        for g in matched.chunk_by(|a, b| a >> DOC_BITS == b >> DOC_BITS) {
            (run.dense, ()) = (false, run.list.clear());
            run.list
                .extend(g.iter().map(|&e| (e & ((1 << DOC_BITS) - 1)) as u16));
            report((g[0] >> DOC_BITS) as usize, run, order);
        }
        // Every BOOL query, on this chunk's documents.
        for (q, query) in queries.iter().enumerate().filter(|(_, q)| !q.phrase) {
            let start = operand(query.terms[0]);
            if matches!(start, Operand::List(l) if l.is_empty()) && !query.ops.contains(&b'O') {
                continue;
            }
            run.start(&start);
            for (k, &op) in query.ops.iter().enumerate() {
                run.apply(op, &operand(query.terms[k + 1]));
            }
            report(q, run, order);
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
