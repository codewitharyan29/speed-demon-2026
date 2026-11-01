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

extern "C" {
    fn prctl(option: i32, ...) -> i32;
}

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

/// One distinct name, in a single cache line: comparing it and recording its country touch one line.
#[repr(align(64))]
struct Entry {
    /// The name's length.
    len: AtomicU64,
    /// Bit c is set once the name has been seen in country c.
    mask: [AtomicU64; COUNTRIES / 64],
    /// The name's first 24 bytes (zero padded); for a longer name, the offset of its bytes in `Names::spill`.
    name: [AtomicU64; 3],
}

/// Every distinct name seen, shared by all workers. `slots` is an open-addressing table of `hash tag << 32 | id + 1`,
/// small enough to stay in the cache; `entries[id]` is the name itself.
struct Names {
    slots: Vec<AtomicU64>,
    entries: &'static [Entry],
    spill: &'static [u8], // the bytes of the names longer than 24, written once each
    spilled: AtomicUsize,
    next: AtomicUsize,
    full: AtomicBool,
}

impl Names {
    fn new(bits: u32) -> Names {
        // The entries are zero from the start and cost memory only once used, except that the allocator zeroes memory by
        // hand if asked for more alignment than it gives, so ask for less and align by hand.
        let layout = std::alloc::Layout::from_size_align((64 << (bits - 1)) + 64, 16).unwrap();
        let base = (unsafe { std::alloc::alloc_zeroed(layout) } as usize + 63) & !63;
        let entries = unsafe { std::slice::from_raw_parts(base as *const Entry, 1 << (bits - 1)) };
        let spill = Box::leak(vec![0u8; 32 << bits].into_boxed_slice());
        Names { slots: zeros(1 << bits), entries, spill, spilled: AtomicUsize::new(0), next: AtomicUsize::new(0), full: AtomicBool::new(false) }
    }

    /// The slot where the search for a name with hash `h` starts.
    fn slot(&self, h: u64) -> &AtomicU64 {
        &self.slots[h as usize & (self.slots.len() - 1)]
    }

    /// Fetches, for writing, the entry that a later `entry` call for hash `h` will most likely look at.
    fn warm(&self, h: u64) {
        let mut i = h as usize & (self.slots.len() - 1);
        loop {
            let v = self.slots[i].load(Relaxed);
            if v == 0 {
                return;
            }
            if v >> 32 == h >> 32 {
                // SAFETY: prefetching has no effect on correctness, whatever the address.
                return unsafe { _mm_prefetch::<_MM_HINT_ET0>(&self.entries[(v & 0xFFFF_FFFF) as usize - 1] as *const Entry as *const i8) };
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
    }

    /// Whether entry `id` is the name `s[..len]`.
    unsafe fn same(&self, id: usize, s: *const u8, len: usize) -> bool {
        let e = &self.entries[id];
        if e.len.load(Relaxed) as usize != len {
            return false;
        }
        if len > 24 {
            let at = e.name[0].load(Relaxed) as usize;
            return self.spill[at..at + len] == *std::slice::from_raw_parts(s, len);
        }
        // The name as three zero-padded words, to compare with the entry's.
        (0..3).all(|k| match len.saturating_sub(8 * k) {
            0 => e.name[k].load(Relaxed) == 0,
            n => e.name[k].load(Relaxed) == (s.add(8 * k) as *const u64).read_unaligned() & (u64::MAX >> (64 - 8 * n.min(8))),
        })
    }

    /// The entry of the name `s[..len]` (`s` must have 32 readable bytes after the name) with hash `h`; adds it if new.
    unsafe fn entry(&self, s: *const u8, len: usize, h: u64) -> &Entry {
        let (tag, mut mine) = (h & !0xFFFF_FFFF, None);
        let mut i = h as usize & (self.slots.len() - 1);
        loop {
            let v = self.slots[i].load(Acquire);
            if v == 0 {
                let id = *mine.get_or_insert_with(|| self.add(s, len));
                if self.slots[i].compare_exchange(0, tag | (id as u64 + 1), AcqRel, Acquire).is_ok() {
                    return &self.entries[id];
                }
                continue; // the slot was taken meanwhile: look at what is in it now
            }
            if v & !0xFFFF_FFFF == tag && self.same((v & 0xFFFF_FFFF) as usize - 1, s, len) {
                return &self.entries[(v & 0xFFFF_FFFF) as usize - 1];
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
    }

    /// Stores a new name; returns its id. Sets `full` (and returns an id that is not used) if there is no room.
    unsafe fn add(&self, s: *const u8, len: usize) -> usize {
        let id = self.next.fetch_add(1, Relaxed);
        if id >= self.entries.len() || self.spilled.load(Relaxed) + len > self.spill.len() {
            self.full.store(true, Relaxed);
            return 0;
        }
        let e = &self.entries[id];
        e.len.store(len as u64, Relaxed);
        if len <= 24 {
            for k in 0..3 {
                let n = len.saturating_sub(8 * k);
                let w = if n == 0 { 0 } else { (s.add(8 * k) as *const u64).read_unaligned() & (u64::MAX >> (64 - 8 * n.min(8))) };
                e.name[k].store(w, Relaxed);
            }
        } else {
            let at = self.spilled.fetch_add(len, Relaxed);
            std::ptr::copy_nonoverlapping(s, self.spill.as_ptr().add(at) as *mut u8, len);
            e.name[0].store(at as u64, Relaxed);
        }
        id // published to other threads by the Release half of the compare-exchange of the slot
    }
}

/// 64-bit hash of a name from its length and its first and last 16 bytes, which must be readable. It only places names in
/// the table: every hit is compared byte for byte.
unsafe fn hash(s: *const u8, len: usize) -> u64 {
    let read = |p: *const u8| (p as *const u128).read_unaligned();
    let head = if len == 0 { 0 } else { read(s) & (!0u128 >> (128 - 8 * len.min(16))) };
    let tail = if len > 16 { read(s.add(len - 16)) } else { 0 };
    let x = head ^ tail.rotate_left(61) ^ (len as u128) << 120;
    let r = (x as u64 ^ 0x9E37_79B9_7F4A_7C15) as u128 * ((x >> 64) as u64 ^ 0xA076_1D64_78BD_642F) as u128;
    r as u64 ^ (r >> 64) as u64
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

#[derive(Clone, Copy)]
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

thread_local! { static MINE: std::cell::OnceCell<&'static Names> = const { std::cell::OnceCell::new() }; }
/// DIAGNOSTIC: a private name table per worker.
fn my(_c: &Ctx) -> &'static Names {
    MINE.with(|m| *m.get_or_init(|| Box::leak(Box::new(Names::new(18)))))
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

// The functions that read the buffer without bounds checks rely on one fact: the buffer extends 64 bytes past the data
// (`PAD`), and every index they use is at most a separator position plus 8.

/// The decimal number at `b[i..]` and the index after it. Up to 8 digits are converted at once, without a branch.
#[inline(always)]
unsafe fn number(b: &[u8], i: usize) -> (u64, usize) {
    let w = (b.as_ptr().add(i) as *const u64).read_unaligned();
    let t = w ^ 0x3030_3030_3030_3030; // a digit byte becomes 0..=9
    let not_digit = ((t & 0x7F7F_7F7F_7F7F_7F7F) + 0x7676_7676_7676_7676 | t) & 0x8080_8080_8080_8080;
    let n = (not_digit.trailing_zeros() / 8) as usize;
    if n < 8 {
        return (digits8(w << (8 * (8 - n))), i + n);
    }
    let (mut v, mut j) = (digits8(w), i + 8); // longer than 8 digits: the rest one by one
    while (*b.get_unchecked(j)).is_ascii_digit() {
        (v, j) = (v * 10 + (*b.get_unchecked(j) - b'0') as u64, j + 1);
    }
    (v, j)
}

/// The decimal number of `n` digits (1 or more) at `b[i..]`. Up to 8 digits are converted at once, without a branch.
#[inline(always)]
unsafe fn digits(b: &[u8], i: usize, n: usize) -> u64 {
    if n > 8 {
        return (i..i + n).fold(0, |v, k| v * 10 + (*b.get_unchecked(k) - b'0') as u64);
    }
    digits8((b.as_ptr().add(i) as *const u64).read_unaligned() << (8 * (8 - n)))
}

/// Where the quotes are in a buffer: bit i of word w is set if byte 64 w + i is a `"`, and `before[w]` quotes precede word w.
#[derive(Default)]
struct Quotes {
    words: Vec<u64>,
    before: Vec<u32>,
}

impl Quotes {
    /// The number of quotes among the bytes before `pos`.
    #[inline(always)]
    unsafe fn before(&self, pos: usize) -> u32 {
        *self.before.get_unchecked(pos / 64) + (*self.words.get_unchecked(pos / 64) & ((1u64 << (pos % 64)) - 1)).count_ones()
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
        // Eight positions are written whether or not there are that many, and the length advances by the real number: a
        // branch per block would be mispredicted about as often as it is taken. More than eight are rare.
        let (mut outside, count) = (separators & !in_quotes, (separators & !in_quotes).count_ones() as usize);
        at.reserve(64);
        for k in 0..count.max(8) {
            // SAFETY: 64 elements of spare capacity were reserved, and a block has at most 64 separators.
            unsafe { at.as_mut_ptr().add(at.len() + k).write((base + outside.trailing_zeros() as usize) as u32) };
            outside &= outside.wrapping_sub(1);
        }
        unsafe { at.set_len(at.len() + count) };
        (quotes.words.push(quote), quotes.before.push(seen));
        seen += quote.count_ones();
    }
    (quotes.words.push(0), quotes.before.push(seen)); // so that `before` works for a position at the very end
}

/// The text field `b[from..to]`: where its content starts and ends, and how many `""` pairs are in it.
#[inline(always)]
unsafe fn text(b: &[u8], q: &Quotes, from: usize, to: usize) -> Option<(usize, usize, usize)> {
    if to == from || *b.get_unchecked(from) != b'"' {
        return Some((from, to, 0)); // unquoted
    }
    if to - from < 2 || *b.get_unchecked(to - 1) != b'"' {
        return None;
    }
    // Two delimiting quotes, and two more for every pair inside.
    Some((from + 1, to - 1, (q.before(to) - q.before(from) - 2) as usize / 2))
}

/// Stage 2: the record that starts at `start` and whose four commas and LF are at `sep` goes into `batch`.
/// Fields are taken to be what the grammar says (spec 2.1: a solution may assume well-formed input), but the five
/// separators must be the bytes they should be: a chunk that started inside a quoted field would fail that.
unsafe fn record(c: &Ctx, b: &[u8], q: &Quotes, start: usize, sep: &[u32], batch: &mut Batch) -> Option<()> {
    let [e1, e2, e3, e4, e5] = [0, 1, 2, 3, 4].map(|k| sep[k] as usize);
    let at = |i: usize| *b.get_unchecked(i);
    let sep_ok = at(e1) == b',' && at(e2) == b',' && at(e3) == b',' && at(e4) == b',' && at(e5) == b'\n';
    let p = start + 3 * (at(start) == 0xEF) as usize; // a BOM in front of the record
    (sep_ok && (p == start || b[start + 1..start + 3] == [0xBB, 0xBF])).then_some(())?;
    let (neg, cr) = (at(e4 + 1) == b'-', (at(e5 - 1) == b'\r') as usize);
    let (whole_at, dot) = (e4 + 1 + neg as usize, e5 - cr - 3); // the amount ends in ".dd"
    let cents = (digits(b, whole_at, dot - whole_at) * 100 + (at(dot + 1) - b'0') as u64 * 10 + (at(dot + 2) - b'0') as u64) as i64;
    let (id, region) = (digits(b, p, e1 - p), digits(b, e3 + 1, e4 - e3 - 1) as usize);
    // A quoted text field has two delimiting quotes and two more for every `""` pair inside it. If the name and the note
    // together have no more than their delimiters, neither has a pair.
    let (name_quoted, note_quoted) = (e2 > e1 + 1 && at(e1 + 1) == b'"', e3 > e2 + 1 && at(e2 + 1) == b'"');
    let (mut nd, mut td) = (0, 0);
    if q.before(e3) - q.before(e1 + 1) != 2 * (name_quoted as u32 + note_quoted as u32) {
        nd = if name_quoted { (q.before(e2) - q.before(e1 + 1) - 2) as usize / 2 } else { 0 };
        td = if note_quoted { (q.before(e3) - q.before(e2 + 1) - 2) as usize / 2 } else { 0 };
    }
    let (ns, ne) = (e1 + 1 + name_quoted as usize, e2 - name_quoted as usize);
    let note_len = e3 - e2 - 1 - 2 * note_quoted as usize - td;
    let owned = (nd > 0).then(|| {
        let (mut v, mut k) = (Vec::with_capacity(ne - ns + 32), ns);
        while k < ne {
            v.push(at(k));
            k += 1 + (at(k) == b'"') as usize;
        }
        v.extend_from_slice(&[0; 32]); // readable padding, like the input buffer has
        v
    });
    let len = ne - ns - nd;
    let name = owned.as_ref().map_or(b[ns..].as_ptr(), |v| v.as_ptr());
    batch.owned.extend(owned);
    // SAFETY: both buffers have 32 readable bytes after the name; the hash reads at most 16 around its ends.
    let hash = hash(name, len);
    _mm_prefetch::<_MM_HINT_T0>(my(c).slot(hash) as *const AtomicU64 as *const i8);
    if let Some(r) = c.region.get(region) {
        _mm_prefetch::<_MM_HINT_T0>(r as *const AtomicU16 as *const i8);
    }
    let cents = if neg { -cents } else { cents };
    batch.recs.push(Rec { id, cents, region, name, len, hash, note_bytes: note_len });
    Some(())
}

/// Looks up the countries and names of the records in `batch` and adds them to `aggs`.
fn resolve(c: &Ctx, batch: &mut Batch, aggs: &mut [Agg]) {
    for r in batch.recs.iter() {
        let country = c.dense[c.region.get(r.region).map_or(0, |x| x.load(Relaxed)) as usize] as usize;
        // SAFETY: the name has 32 readable bytes after it, as `record` ensured.
        let name = unsafe { my(c).entry(r.name, r.len, r.hash) };
        name.mask[country / 64].fetch_or(1 << (country % 64), Relaxed);
        let a = &mut aggs[country];
        a.count += 1;
        a.id_sum += r.id;
        a.sum += r.cents;
        (a.min, a.max) = (a.min.min(r.cents), a.max.max(r.cents));
        a.name_bytes += r.len as u64;
        a.note_bytes += r.note_bytes as u64;
    }
    batch.recs.clear();
    batch.owned.clear();
}

/// The buffers of one worker, kept from chunk to chunk.
#[derive(Default)]
struct Worker {
    buf: Vec<u8>,
    batches: [Batch; 3],
    at: Vec<u32>,
    quotes: Quotes,
}

/// Parses the records between `lo` and `hi`, which are record starts, into `aggs`. False if something is not as expected.
fn parse_chunk(c: &Ctx, lo: usize, hi: usize, aggs: &mut [Agg], w: &mut Worker) -> bool {
    let Worker { buf, batches, at, quotes } = w;
    let (mut next, mut keep, mut now) = (lo, 0, 0);
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
        structure(buf, end, at, quotes);
        let mut start = 0;
        for sep in at.chunks_exact(5) {
            // SAFETY: the buffer has `PAD` bytes after `end`, and the separators are before `end`.
            if unsafe { record(c, buf, quotes, start, sep, &mut batches[now]) }.is_none() {
                return false;
            }
            start = sep[4] as usize + 1;
            if batches[now].recs.len() == BATCH {
                // The batch before this one has had the slots of its names prefetched while this one was parsed: look at
                // them now, and prefetch the entries. The batch before that has had its entries prefetched: resolve it.
                batches[(now + 2) % 3].recs.iter().for_each(|r| my(c).warm(r.hash));
                resolve(c, &mut batches[(now + 1) % 3], aggs);
                now = (now + 1) % 3;
            }
        }
        for k in [1, 2, 0] {
            // before the buffer changes
            let b = &mut batches[(now + k) % 3];
            b.recs.iter().for_each(|r| my(c).warm(r.hash));
            resolve(c, b, aggs);
        }
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

/// The start of the first line of `file` (`len` bytes) at or after `at`, where lines are at most 64 bytes long.
fn line_start(file: &File, len: usize, at: usize) -> usize {
    let (n, mut buf) = ((len - (at - 1)).min(64), [0u8; 64]);
    file.read_exact_at(&mut buf[..n], at as u64 - 1).unwrap();
    buf[..n].iter().position(|&c| c == b'\n').map_or(len, |k| at + k)
}

/// The aggregates of `file` (`len` bytes); `None` if the fast path gives up: `chunk` is the size of the pieces it is
/// parsed in, and `bits` the size of the name table.
fn run(regions: &File, file: &File, len: usize, bits: u32, chunk: usize) -> Option<Vec<u8>> {
    // regions.csv: a header, then "region_id,CCC\n" lines in ascending order, so the last line has the largest id.
    let rlen = regions.metadata().ok()?.len() as usize;
    let (mut head, mut tail) = ([0u8; 64], [0u8; 64]);
    regions.read_exact_at(&mut head, 0).ok()?;
    regions.read_exact_at(&mut tail, rlen as u64 - 64).ok()?;
    let first = head.iter().position(|&c| c == b'\n')? + 1;
    let last = tail[..63].iter().rposition(|&c| c == b'\n')? + 1;
    let max_id = tail[last..].iter().take_while(|c| c.is_ascii_digit()).fold(0, |v, &c| v * 10 + (c - b'0') as usize);
    let (region, seen): (Vec<AtomicU16>, Vec<AtomicBool>) = (zeros(max_id + 1), zeros(26 * 26 * 26 + 1));
    let pieces = 16;
    let cut = |j: usize| match j {
        0 => first,
        _ if j == pieces => rlen,
        _ => line_start(regions, rlen, first + (rlen - first) * j / pieces),
    };
    let parsed = parallel(pieces, |j| {
        let len = cut(j + 1) - cut(j);
        let mut text = vec![0u8; len + PAD];
        regions.read_exact_at(&mut text[..len], cut(j) as u64).ok()?;
        let mut p = 0;
        while p < len {
            // A line is "region_id,CCC" and an LF.
            // SAFETY: the buffer extends `PAD` bytes past the data.
            let (id, i) = unsafe { number(&text, p) };
            let country = &text[i + 1..i + 4];
            if text[i] != b',' || text[i + 4] != b'\n' || id as usize > max_id || !country.iter().all(|c| c.is_ascii_uppercase()) {
                return None;
            }
            let code = country.iter().fold(0, |v, &c| v * 26 + (c - b'A') as usize) + 1;
            region[id as usize].store(code as u16, Relaxed);
            seen[code].store(true, Relaxed);
            p = i + 5;
        }
        (p == len).then_some(())
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
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let results = parallel(threads, |_| {
        let (mut aggs, mut worker, mut ok) = (vec![NONE; countries], Worker::default(), true);
        loop {
            let j = next.fetch_add(1, Relaxed);
            if j >= chunks {
                return ok.then_some(aggs);
            }
            let lo = if j == 0 { body } else { record_start(&ctx, j * chunk) };
            let hi = if j + 1 == chunks { len } else { record_start(&ctx, (j + 1) * chunk) };
            ok &= lo >= hi || parse_chunk(&ctx, lo, hi, &mut aggs, &mut worker);
        }
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
    let parts = parallel(names.next.load(Relaxed).min(names.entries.len()).div_ceil(4096), |j| {
        let mut counts = vec![0u64; countries];
        for e in &names.entries[j * 4096..((j + 1) * 4096).min(names.next.load(Relaxed).min(names.entries.len()))] {
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
    // PR_SET_THP_DISABLE: with transparent huge pages every sparsely touched 2 MiB of the big tables would be backed in full.
    unsafe { prctl(41, 1u64, 0u64, 0u64, 0u64) };
    let regions = File::open("/data/regions.csv").unwrap();
    let file = File::open("/data/records.csv").unwrap();
    let len = file.metadata().unwrap().len() as usize;
    // If the parallel run gives up, parse everything as one chunk, with a name table that holds a name per record.
    let out = run(&regions, &file, len, 18, 1 << 20).or_else(|| run(&regions, &file, len, 23, len.max(1)));
    std::fs::write("/out/aggregates.txt", out.expect("records.csv is not valid")).unwrap();
    std::process::exit(0);
}
