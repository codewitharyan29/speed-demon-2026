//! Track C (join): aggregate `records.csv` per country of its `region_id`, as `regions.csv` says.
//!
//! 1. regions.csv is parsed in parallel into a direct-indexed table region_id -> country.
//! 2. records.csv is cut into chunks and the chunks are parsed in parallel into per-chunk totals. A chunk starts at a
//!    record found from the bytes after the cut alone (`record_start`), so no pass over the data is needed to learn
//!    which quotes are open. Records are looked up in batches: the parser prefetches what a lookup needs and moves on.
//! 3. Each distinct name has one cache line in a shared table (exact comparison of the bytes) holding a bitmask of the
//!    countries it occurred in, so "distinct names per country" is read off the masks at the end.
//!
//! The one place the fast path relies on spec 2.2 is `record_start`: inside a quoted field an LF is followed by a
//! lowercase letter or a quote (notes are lowercase words), never by a digit 1-9 or a BOM, which is what follows a record
//! terminator. Every chunk must end exactly where the next one starts, and the name table must not overflow; if anything
//! is off, the whole input is parsed again as one chunk by one thread, which needs no such assumption.

use std::arch::x86_64::*;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering::*};

const BLOCK: usize = 64 << 10; // bytes read at a time
const PAD: usize = 64; // zero bytes after the data in a buffer, which the parser may read
const BATCH: usize = 16; // records looked up together
const COUNTRIES: usize = 256; // room in a name's mask (the spec has 240, plus _UNMATCHED)

fn zeros<T: Default>(n: usize) -> Vec<T> {
    (0..n).map(|_| T::default()).collect()
}

/// Runs `f(0..jobs)` on all available threads, which take jobs from one counter; returns the results in any order.
fn parallel<R: Send>(jobs: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        let work = || std::iter::from_fn(|| Some(next.fetch_add(1, Relaxed)).filter(|&j| j < jobs).map(&f)).collect::<Vec<_>>();
        let workers: Vec<_> = (0..threads).map(|_| s.spawn(work)).collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    })
}

/// One distinct name, in a single cache line: finding it, comparing it and recording its country touch one line.
#[repr(align(64))]
struct Entry {
    /// `hash tag << 32 | length << 8 | state`; the state is `INSERTING` while the name is written, then `READY`.
    head: AtomicU64,
    /// Bit c is set once the name has been seen in country c.
    mask: [AtomicU64; COUNTRIES / 64],
    /// The name's first 24 bytes (zero padded); for a longer name, the offset of its bytes in `Names::spill`.
    name: [AtomicU64; 3],
}

const INSERTING: u64 = 1;
const READY: u64 = 2;

/// Every distinct name seen: an open-addressing table of `Entry`, shared by all workers.
struct Names {
    table: &'static [Entry],
    spill: &'static [u8], // the bytes of the names longer than 24, written once each
    spilled: AtomicUsize,
    filled: AtomicUsize,
    full: AtomicBool,
}

impl Names {
    fn new(bits: u32) -> Names {
        // Zeroed memory from the allocator is not touched until used, so only filled entries cost memory.
        let layout = std::alloc::Layout::array::<Entry>(1 << bits).unwrap();
        let table = unsafe { std::slice::from_raw_parts(std::alloc::alloc_zeroed(layout) as *const Entry, 1 << bits) };
        let spill = Box::leak(vec![0u8; 32 << bits].into_boxed_slice());
        Names { table, spill, spilled: AtomicUsize::new(0), filled: AtomicUsize::new(0), full: AtomicBool::new(false) }
    }

    /// The slot where the search for a name with hash `h` starts.
    fn slot(&self, h: u64) -> &Entry {
        &self.table[h as usize & (self.table.len() - 1)]
    }

    /// The entry of the name `s[..len]` (`s` must have 32 readable bytes after the name) with hash `h`; adds it if new.
    unsafe fn entry(&self, s: *const u8, len: usize, h: u64) -> &Entry {
        let tag = h & !0xFFFF_FFFF | (len as u64) << 8;
        // The name as three zero-padded words, to compare with an inline name.
        let word = |k: usize| match len.saturating_sub(8 * k) {
            0 => 0,
            n => (s.add(8 * k) as *const u64).read_unaligned() & (u64::MAX >> (64 - 8 * n.min(8))),
        };
        let mut i = h as usize & (self.table.len() - 1);
        loop {
            let e = &self.table[i];
            let v = e.head.load(Acquire);
            if v == tag | READY {
                let at = e.name[0].load(Relaxed) as usize;
                let same = if len > 24 {
                    self.spill[at..at + len] == *std::slice::from_raw_parts(s, len)
                } else {
                    (0..3).all(|k| e.name[k].load(Relaxed) == word(k))
                };
                if same {
                    return e;
                }
            } else if v == 0 {
                if self.filled.fetch_add(1, Relaxed) * 10 > self.table.len() * 8 || self.spilled.load(Relaxed) + len > self.spill.len() {
                    self.full.store(true, Relaxed);
                    return e; // the run is abandoned; any entry will do
                }
                if e.head.compare_exchange(0, tag | INSERTING, Acquire, Acquire).is_ok() {
                    if len <= 24 {
                        (0..3).for_each(|k| e.name[k].store(word(k), Relaxed));
                    } else {
                        let at = self.spilled.fetch_add(len, Relaxed);
                        std::ptr::copy_nonoverlapping(s, self.spill.as_ptr().add(at) as *mut u8, len);
                        e.name[0].store(at as u64, Relaxed);
                    }
                    e.head.store(tag | READY, Release);
                    return e;
                }
                self.filled.fetch_sub(1, Relaxed);
                continue; // somebody else took the slot: look at what is in it now
            } else if v & 0xFF == INSERTING {
                std::hint::spin_loop();
                continue; // a name is being written here: wait for it
            }
            i = (i + 1) & (self.table.len() - 1);
        }
    }
}

/// 64-bit hash of a name from its length and its first and last 16 bytes, which must be readable.
unsafe fn hash(s: *const u8, len: usize) -> u64 {
    let read = |p: *const u8| (p as *const u128).read_unaligned();
    let mix = |x: u128, k: u64| {
        let r = (x as u64 ^ k) as u128 * ((x >> 64) as u64 ^ 0xA076_1D64_78BD_642F) as u128;
        r as u64 ^ (r >> 64) as u64
    };
    let head = if len == 0 { 0 } else { read(s) & (!0u128 >> (128 - 8 * len.min(16))) };
    let tail = if len > 16 { read(s.add(len - 16)) } else { 0 };
    mix(head, 0x9E37_79B9_7F4A_7C15) ^ mix(tail ^ len as u128, 0xE703_7ED1_A0B4_28DB)
}

/// Running totals of one country.
#[derive(Clone, Copy)]
struct Agg {
    count: u64,
    id_sum: u64,
    sum: i64,
    min: i64,
    max: i64,
    name_bytes: u64,
    note_bytes: u64,
}

const NONE: Agg = Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 };

/// A parsed record whose country and name are still to be looked up. The name is in the input buffer, or in the batch's
/// `owned` list (unescaped, with padding) when it contained `""`; its hash is computed and its table slot prefetched.
struct Rec {
    id: u64,
    cents: i64,
    region: usize,
    name: *const u8,
    len: usize,
    hash: u64,
    note_bytes: usize,
}

/// Records waiting to be looked up, and the unescaped names some of them point to.
#[derive(Default)]
struct Batch {
    recs: Vec<Rec>,
    owned: Vec<Vec<u8>>,
}

/// What the workers share.
struct Ctx<'a> {
    file: &'a File,
    len: usize,
    region: &'a [AtomicU16], // region_id -> packed country code + 1 (0: no such region)
    dense: &'a [u16],        // packed country code + 1 -> index into the totals (0: _UNMATCHED, which is last)
    names: &'a Names,
}

/// The 8 digits in `w` (first digit in the lowest byte) as a number; padding bytes must be zero.
fn digits8(w: u64) -> u64 {
    let w = (w & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(2561) >> 8;
    let w = (w & 0x00FF_00FF_00FF_00FF).wrapping_mul(6553601) >> 16;
    (w & 0x0000_FFFF_0000_FFFF).wrapping_mul(42949672960001) >> 32
}

/// The decimal number at `b[i..]` and the index after it. Up to 8 digits are converted at once, without a branch.
fn number(b: &[u8], i: usize) -> (u64, usize) {
    let w = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let t = w ^ 0x3030_3030_3030_3030; // a digit byte becomes 0..=9
    let not_digit = ((t & 0x7F7F_7F7F_7F7F_7F7F) + 0x7676_7676_7676_7676 | t) & 0x8080_8080_8080_8080;
    let n = (not_digit.trailing_zeros() / 8) as usize;
    if n < 8 {
        return (digits8(w << (8 * (8 - n))), i + n);
    }
    let (mut v, mut j) = (digits8(w), i + 8); // longer than 8 digits: the rest one by one
    while b[j].is_ascii_digit() {
        (v, j) = (v * 10 + (b[j] - b'0') as u64, j + 1);
    }
    (v, j)
}

/// Where the quotes are in a buffer: bit i of word w is set if byte 64 w + i is a `"`, and `before[w]` quotes precede word w.
#[derive(Default)]
struct Quotes {
    words: Vec<u64>,
    before: Vec<u32>,
}

impl Quotes {
    /// The number of quotes among the bytes before `pos`.
    fn before(&self, pos: usize) -> u32 {
        self.before[pos / 64] + (self.words[pos / 64] & ((1u64 << (pos % 64)) - 1)).count_ones()
    }
}

/// Stage 1: the positions of the commas and LFs outside quotes in `b[..end]` (which starts outside quotes) go to `at`,
/// and the quotes to `quotes`. A quote toggles being inside quotes, and so does each half of a `""`, so the bytes inside
/// quotes are the prefix XOR of the quote bits, which a carry-less multiplication by all ones computes.
fn structure(b: &[u8], end: usize, at: &mut Vec<u32>, quotes: &mut Quotes) {
    (at.clear(), quotes.words.clear(), quotes.before.clear());
    let (mut inside, mut seen) = (0u64, 0u32);
    for base in (0..end).step_by(64) {
        // SAFETY: the buffer extends 64 bytes past `end`.
        let (quote, separators, in_quotes) = unsafe {
            let (lo, hi) = (_mm256_loadu_si256(b.as_ptr().add(base) as _), _mm256_loadu_si256(b.as_ptr().add(base + 32) as _));
            let bits = |c: u8| {
                let c = _mm256_set1_epi8(c as i8);
                let (x, y) = (_mm256_cmpeq_epi8(lo, c), _mm256_cmpeq_epi8(hi, c));
                _mm256_movemask_epi8(x) as u32 as u64 | (_mm256_movemask_epi8(y) as u32 as u64) << 32
            };
            let quote = bits(b'"');
            let prefix_xor = _mm_clmulepi64_si128(_mm_set_epi64x(0, quote as i64), _mm_set1_epi8(-1), 0);
            (quote, bits(b',') | bits(b'\n'), _mm_cvtsi128_si64(prefix_xor) as u64 ^ inside)
        };
        inside = ((in_quotes as i64) >> 63) as u64; // all ones if the next 64 bytes start inside quotes
        let mut outside = separators & !in_quotes;
        while outside != 0 {
            at.push((base + outside.trailing_zeros() as usize) as u32);
            outside &= outside - 1;
        }
        (quotes.words.push(quote), quotes.before.push(seen));
        seen += quote.count_ones();
    }
    (quotes.words.push(0), quotes.before.push(seen)); // so that `before` works for a position at the very end
}

/// The text field `b[from..to]`: where its content starts and ends, and how many `""` pairs are in it.
fn text(b: &[u8], q: &Quotes, from: usize, to: usize) -> Option<(usize, usize, usize)> {
    if to == from || b[from] != b'"' {
        return Some((from, to, 0)); // unquoted
    }
    if to - from < 2 || b[to - 1] != b'"' {
        return None;
    }
    // Two delimiting quotes, and two more for every pair inside.
    Some((from + 1, to - 1, (q.before(to) - q.before(from) - 2) as usize / 2))
}

/// Stage 2: the record that starts at `start` and whose four commas and LF are at `sep` goes into `batch`.
fn record(c: &Ctx, b: &[u8], q: &Quotes, start: usize, sep: &[u32], batch: &mut Batch) -> Option<()> {
    let [e1, e2, e3, e4, e5] = [0, 1, 2, 3, 4].map(|k| sep[k] as usize);
    let sep_ok = b[e1] == b',' && b[e2] == b',' && b[e3] == b',' && b[e4] == b',' && b[e5] == b'\n';
    let p = start + 3 * (b[start] == 0xEF) as usize; // a BOM in front of the record
    (sep_ok && (p == start || b[start + 1..start + 3] == [0xBB, 0xBF])).then_some(())?;
    let (id, i) = number(b, p);
    let (ns, ne, nd) = text(b, q, e1 + 1, e2)?;
    let (ts, te, td) = text(b, q, e2 + 1, e3)?;
    let (region, j) = number(b, e3 + 1);
    let neg = b[e4 + 1] == b'-';
    let (whole, k) = number(b, e4 + 1 + neg as usize);
    let cr = (b[e5 - 1] == b'\r') as usize;
    let fraction = b[k + 1].is_ascii_digit() && b[k + 2].is_ascii_digit();
    (i == e1 && j == e4 && b[k] == b'.' && fraction && k + 3 + cr == e5).then_some(())?;
    let cents = (whole * 100 + (b[k + 1] - b'0') as u64 * 10 + (b[k + 2] - b'0') as u64) as i64;
    let owned = (nd > 0).then(|| {
        let (mut v, mut k) = (Vec::with_capacity(ne - ns + 32), ns);
        while k < ne {
            v.push(b[k]);
            k += 1 + (b[k] == b'"') as usize;
        }
        v.extend_from_slice(&[0; 32]); // readable padding, like the input buffer has
        v
    });
    let len = ne - ns - nd;
    let name = owned.as_ref().map_or(b[ns..].as_ptr(), |v| v.as_ptr());
    batch.owned.extend(owned);
    // SAFETY: both buffers have 32 readable bytes after the name; the hash reads at most 16 around its ends.
    let hash = unsafe { hash(name, len) };
    unsafe {
        // The entry is probably within four lines of the slot where its search starts.
        let slot = c.names.slot(hash) as *const Entry as *const i8;
        if let Some(r) = c.region.get(region as usize) {
            _mm_prefetch::<_MM_HINT_T0>(r as *const AtomicU16 as *const i8);
        }
    }
    let cents = if neg { -cents } else { cents };
    batch.recs.push(Rec { id, cents, region: region as usize, name, len, hash, note_bytes: te - ts - td });
    Some(())
}

/// Looks up the countries and names of the records in `batch` and adds them to `aggs`.
fn resolve(c: &Ctx, batch: &mut Batch, aggs: &mut [Agg]) {
    for r in batch.recs.drain(..) {
        let country = c.dense[c.region.get(r.region).map_or(0, |x| x.load(Relaxed)) as usize] as usize;
        // SAFETY: the name has 32 readable bytes after it, as `record` ensured.
        let name = &c.names.table[0]; // DIAGNOSTIC: no name lookups
        let a = &mut aggs[country];
        a.count += 1;
        a.id_sum += r.id;
        a.sum += r.cents;
        (a.min, a.max) = (a.min.min(r.cents), a.max.max(r.cents));
        a.name_bytes += r.len as u64;
        a.note_bytes += r.note_bytes as u64;
    }
    batch.owned.clear();
}

/// Parses the records between `lo` and `hi`, which are record starts, into `aggs`. False if something is not as expected.
fn parse_chunk(c: &Ctx, lo: usize, hi: usize, aggs: &mut [Agg]) -> bool {
    let (mut buf, mut batch, mut at, mut quotes) = (vec![], Batch::default(), vec![], Quotes::default());
    let (mut next, mut keep) = (lo, 0);
    loop {
        let n = BLOCK.min(hi - next);
        buf.resize(keep + n + PAD, 0);
        c.file.read_exact_at(&mut buf[keep..keep + n], next as u64).unwrap();
        buf[keep + n..].fill(0);
        next += n;
        let (mut end, last) = (keep + n, next == hi);
        if last && buf[end - 1] != b'\n' {
            (buf[end], end) = (b'\n', end + 1); // the last record may lack its terminator
        }
        structure(&buf, end, &mut at, &mut quotes);
        let mut start = 0;
        for sep in at.chunks_exact(5) {
            if record(c, &buf, &quotes, start, sep, &mut batch).is_none() {
                return false;
            }
            start = sep[4] as usize + 1;
            if batch.recs.len() == BATCH {
                resolve(c, &mut batch, aggs);
            }
        }
        resolve(c, &mut batch, aggs); // before the buffer changes
        if last {
            return start == end;
        }
        keep = end - start;
        buf.copy_within(start..end, 0);
    }
}

/// Start of the first record that begins at or after `cut`, judged by the bytes after `cut - 1` alone: an LF followed
/// by a digit 1-9 or a BOM. (Inside a quoted field an LF is followed by a letter or a quote, spec 2.2.)
fn record_start(c: &Ctx, cut: usize) -> usize {
    let (mut at, mut buf) = (cut.max(1) - 1, [0u8; 512]);
    while at + 1 < c.len {
        let n = (c.len - at).min(buf.len());
        c.file.read_exact_at(&mut buf[..n], at as u64).unwrap();
        if let Some(k) = (0..n - 1).find(|&k| buf[k] == b'\n' && (buf[k + 1].wrapping_sub(b'1') < 9 || buf[k + 1] == 0xEF)) {
            return at + k + 1;
        }
        at += n - 1; // the last byte is looked at again, together with its successor
    }
    c.len
}

fn money(v: i64) -> String {
    format!("{}{}.{:02}", if v < 0 { "-" } else { "" }, v.abs() / 100, v.abs() % 100)
}

/// The aggregates of `file` (`len` bytes); `None` if the fast path gives up: `chunk` is the size of the pieces it is
/// parsed in, and `bits` the size of the name table.
fn run(regions: &[u8], file: &File, len: usize, bits: u32, chunk: usize) -> Option<Vec<u8>> {
    // regions.csv: a header, then "region_id,CCC\n" lines in ascending order, so the last line has the largest id.
    let first = regions.iter().position(|&c| c == b'\n')? + 1;
    let last = regions[..regions.len() - 1].iter().rposition(|&c| c == b'\n').map_or(first, |k| k + 1);
    let max_id = regions[last..].iter().take_while(|c| c.is_ascii_digit()).fold(0, |v, &c| v * 10 + (c - b'0') as usize);
    let (region, seen): (Vec<AtomicU16>, Vec<AtomicBool>) = (zeros(max_id + 1), zeros(26 * 26 * 26 + 1));
    let pieces = 16;
    let cut = |j: usize| match j {
        0 => first,
        _ if j == pieces => regions.len(),
        _ => {
            let nominal = first + (regions.len() - first) * j / pieces;
            nominal + regions[nominal - 1..].iter().position(|&c| c == b'\n').unwrap()
        }
    };
    let parsed = parallel(pieces, |j| {
        for line in regions[cut(j)..cut(j + 1)].split(|&c| c == b'\n').filter(|l| !l.is_empty()) {
            let (id, country) = line.split_at(line.iter().position(|&c| c == b',')?);
            let (id, country) = (id.iter().fold(0, |v, &c| v * 10 + (c - b'0') as usize), &country[1..]);
            if country.len() != 3 || !country.iter().all(|c| c.is_ascii_uppercase()) || id > max_id {
                return None;
            }
            let code = country.iter().fold(0, |v, &c| v * 26 + (c - b'A') as usize) + 1;
            region[id].store(code as u16, Relaxed);
            seen[code].store(true, Relaxed);
        }
        Some(())
    });
    parsed.iter().all(|r| r.is_some()).then_some(())?;
    // Countries get indices in the order of their packed codes, which is the byte order of the codes; _UNMATCHED is last.
    let (mut dense, mut keys) = (vec![0u16; seen.len()], vec![]);
    for code in (1..seen.len()).filter(|&c| seen[c].load(Relaxed)) {
        dense[code] = keys.len() as u16;
        keys.push(vec![b'A' + ((code - 1) / 676) as u8, b'A' + ((code - 1) / 26 % 26) as u8, b'A' + ((code - 1) % 26) as u8]);
    }
    dense[0] = keys.len() as u16;
    keys.push(b"_UNMATCHED".to_vec());
    let countries = keys.len();
    (countries <= COUNTRIES).then_some(())?;

    let names = Names::new(bits);
    let ctx = Ctx { file, len, region: &region, dense: &dense, names: &names };
    let mut head = [0u8; 256];
    file.read_exact_at(&mut head[..len.min(256)], 0).ok()?;
    let body = head.iter().position(|&c| c == b'\n')? + 1; // the header has no quotes
    let chunks = len.div_ceil(chunk);
    let results = parallel(chunks, |j| {
        let lo = if j == 0 { body } else { record_start(&ctx, j * chunk) };
        let hi = if j + 1 == chunks { len } else { record_start(&ctx, (j + 1) * chunk) };
        let mut aggs = vec![NONE; countries];
        (lo >= hi || parse_chunk(&ctx, lo, hi, &mut aggs)).then_some(aggs)
    });
    if names.full.load(Relaxed) || results.iter().any(|r| r.is_none()) {
        return None;
    }
    let mut total = vec![NONE; countries];
    for part in results.iter().flatten() {
        for (t, a) in total.iter_mut().zip(part) {
            *t = Agg {
                count: t.count + a.count,
                id_sum: t.id_sum + a.id_sum,
                sum: t.sum + a.sum,
                min: t.min.min(a.min),
                max: t.max.max(a.max),
                name_bytes: t.name_bytes + a.name_bytes,
                note_bytes: t.note_bytes + a.note_bytes,
            };
        }
    }

    // Distinct names per country: the set bits of the masks, counted by ranges of entries in parallel.
    let parts = parallel(names.table.len().div_ceil(4096), |j| {
        let mut counts = vec![0u64; countries];
        for e in &names.table[j * 4096..((j + 1) * 4096).min(names.table.len())] {
            for (w, mask) in e.mask.iter().enumerate() {
                let mut bits = mask.load(Relaxed);
                while bits != 0 {
                    counts[w * 64 + bits.trailing_zeros() as usize] += 1;
                    bits &= bits - 1;
                }
            }
        }
        counts
    });
    let mut out = vec![];
    for (c, (key, a)) in keys.iter().zip(&total).enumerate().filter(|(_, (_, a))| a.count > 0) {
        let distinct: u64 = parts.iter().map(|p| p[c]).sum();
        out.extend_from_slice(key);
        let (sum, min, max) = (money(a.sum), money(a.min), money(a.max));
        writeln!(out, " {} {} {sum} {min} {max} {distinct} {} {}", a.count, a.id_sum, a.name_bytes, a.note_bytes).unwrap();
    }
    Some(out)
}

fn main() {
    let regions = std::fs::read("/data/regions.csv").unwrap();
    let file = File::open("/data/records.csv").unwrap();
    let len = file.metadata().unwrap().len() as usize;
    // If the parallel run gives up, parse everything as one chunk, with a name table that holds a name per record.
    let out = run(&regions, &file, len, 17, 1 << 20).or_else(|| run(&regions, &file, len, 22, len.max(1)));
    std::fs::write("/out/aggregates.txt", out.expect("records.csv is not valid")).unwrap();
    std::process::exit(0);
}
