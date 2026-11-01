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
use std::os::unix::fs::FileExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering::*};

extern "C" {
    fn prctl(option: i32, ...) -> i32;
}

const BLOCK: usize = 32 << 10; // bytes read at a time
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
    /// The name's length + 1, stored last (0: the entry is free or still being written).
    len: AtomicU64,
    /// Bit c is set once the name has been seen in country c.
    mask: [AtomicU64; COUNTRIES / 64],
    /// The name's first 24 bytes (zero padded); for a longer name, the offset of its bytes in `Names::spill`.
    name: [AtomicU64; 3],
}

/// Every distinct name seen, shared by all workers: an open-addressing table of entries, placed by the low bits of the hash.
/// `tags[i]` (small enough to stay in the cache) holds 8 high bits of the hash of the name in `entries[i]` (0: free), so
/// a lookup reads one entry line in all but rare cases.
struct Names {
    tags: Vec<AtomicU8>,
    entries: &'static [Entry],
    spill: &'static [u8], // the bytes of the names longer than 24, written once each
    counts: Counts,
}

/// What adding a name writes, kept away from the cache line that every lookup reads.
#[repr(align(128))]
#[derive(Default)]
struct Counts {
    spilled: AtomicUsize,
    next: AtomicUsize,
    full: AtomicBool,
}

/// The nonzero tag of hash `h`.
fn tag(h: u64) -> u8 {
    let t = (h >> 56) as u8;
    t + (t == 0) as u8
}

impl Names {
    fn new(bits: u32) -> Names {
        // The entries are zero from the start and cost memory only once used, except that the allocator zeroes memory by
        // hand if asked for more alignment than it gives, so ask for less and align by hand.
        let layout = std::alloc::Layout::from_size_align((64 << (bits - 1)) + 64, 16).unwrap();
        let base = (unsafe { std::alloc::alloc_zeroed(layout) } as usize + 63) & !63;
        let entries = unsafe { std::slice::from_raw_parts(base as *const Entry, 1 << (bits - 1)) };
        let spill = Box::leak(vec![0u8; 32 << bits].into_boxed_slice());
        Names { tags: zeros((1 << (bits - 1)) + 16), entries, spill, counts: Counts::default() }
    }

    /// The position where the search for a name with hash `h` starts.
    fn home(&self, h: u64) -> usize {
        h as usize & (self.entries.len() - 1)
    }

    /// Fetches, for writing, the entry that a later `entry` call for hash `h` will most likely look at; returns its
    /// position + 1 (0: none yet), which `entry` takes as a hint. The 16 tags from the start position are compared at
    /// once: the first that matches before the first free one.
    fn warm(&self, h: u64) -> u32 {
        let i = self.home(h);
        // SAFETY: `tags` has 16 more elements than positions.
        let (hit, free) = unsafe {
            let v = _mm_loadu_si128(self.tags.as_ptr().add(i) as *const __m128i);
            let m = |t: u8| _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(t as i8))) as u32;
            (m(tag(h)), m(0) | 1 << 16)
        };
        let hit = hit & (free & free.wrapping_neg()).wrapping_sub(1);
        if hit == 0 {
            return 0;
        }
        let p = (i + hit.trailing_zeros() as usize) & (self.entries.len() - 1);
        // SAFETY: prefetching has no effect on correctness, whatever the address.
        unsafe { _mm_prefetch::<_MM_HINT_ET0>(&self.entries[p] as *const Entry as *const i8) };
        p as u32 + 1
    }

    /// Whether entry `i` is the name `s[..len]`.
    unsafe fn same(&self, i: usize, s: *const u8, len: usize) -> bool {
        let e = &self.entries[i];
        let l = e.len.load(Acquire); // the name's bytes were written before its length
        if len > 24 {
            let at = e.name[0].load(Relaxed) as usize;
            return l == len as u64 + 1 && self.spill[at..at + len] == *std::slice::from_raw_parts(s, len);
        }
        // No branch on the length: the lengths of names are not predictable.
        (0..3).fold(l ^ (len as u64 + 1), |d, k| d | e.name[k].load(Relaxed) ^ word(s, len, k)) == 0
    }

    /// The entry of the name `s[..len]` (`s` must have 32 readable bytes after the name) with hash `h`; adds it if new. `hint` is
    /// an entry that probably is it (0: none): it is checked first, so that the common case does not walk the table again.
    unsafe fn entry(&self, s: *const u8, len: usize, h: u64, hint: u32) -> &Entry {
        if hint != 0 && self.same(hint as usize - 1, s, len) {
            return &self.entries[hint as usize - 1];
        }
        let (t, mut i) = (tag(h), self.home(h));
        loop {
            let v = self.tags[i].load(Acquire);
            if v == 0 {
                // Keep at least 1/8 of the table free, so that every search ends.
                if self.counts.next.fetch_add(1, Relaxed) >= self.entries.len() / 8 * 7 || self.counts.spilled.load(Relaxed) + len > self.spill.len() {
                    self.counts.full.store(true, Relaxed);
                    return &self.entries[i];
                }
                if self.tags[i].compare_exchange(0, t, AcqRel, Acquire).is_ok() {
                    if i < 16 {
                        self.tags[self.entries.len() + i].store(t, Release); // the copy that a search near the end reads
                    }
                    self.add(i, s, len);
                    return &self.entries[i];
                }
                self.counts.next.fetch_sub(1, Relaxed);
                continue; // the position was taken meanwhile: look at what is in it now
            }
            if v == t {
                while self.entries[i].len.load(Acquire) == 0 {
                    std::hint::spin_loop(); // another worker is writing this name
                }
                if self.same(i, s, len) {
                    return &self.entries[i];
                }
            }
            i = (i + 1) & (self.entries.len() - 1);
        }
    }

    /// Writes the name into the free entry `i`, which this worker has claimed.
    unsafe fn add(&self, i: usize, s: *const u8, len: usize) {
        let e = &self.entries[i];
        if len <= 24 {
            for k in 0..3 {
                e.name[k].store(word(s, len, k), Relaxed);
            }
        } else {
            let at = self.counts.spilled.fetch_add(len, Relaxed);
            std::ptr::copy_nonoverlapping(s, self.spill.as_ptr().add(at) as *mut u8, len);
            e.name[0].store(at as u64, Relaxed);
        }
        e.len.store(len as u64 + 1, Release); // publishes the name to the other workers
    }
}

/// Bytes 8k.. of the name `s[..len]` as a word, zero padded past the end of the name (8 bytes after 8k must be readable).
#[inline(always)]
unsafe fn word(s: *const u8, len: usize, k: usize) -> u64 {
    (s.add(8 * k) as *const u64).read_unaligned() & ((1u128 << (8 * len.saturating_sub(8 * k).min(8))) - 1) as u64
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

impl Agg {
    fn merge(&mut self, o: &Agg) {
        self.count += o.count;
        self.id_sum += o.id_sum;
        self.sum += o.sum;
        (self.min, self.max) = (self.min.min(o.min), self.max.max(o.max));
        self.name_bytes += o.name_bytes;
        self.note_bytes += o.note_bytes;
    }
}

const NONE: Agg = Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 };

#[derive(Clone, Copy)]
/// A parsed record whose country and name are still to be looked up. The name is in the input buffer, as written; its
/// hash is computed and its table slot prefetched.
struct Rec {
    id: u64,
    cents: i64,
    name: *const u8,
    hash: u64,
    region: u32,
    len: u32,
    name_bytes: u32,
    note_bytes: u32,
    hint: u32,
}

/// Records waiting to be looked up.
struct Batch {
    recs: [Rec; BATCH],
    n: usize,
}

impl Default for Batch {
    fn default() -> Batch {
        // SAFETY: a `Rec` of zeros is valid (a null pointer is never read: `n` counts the records written).
        Batch { recs: unsafe { std::mem::zeroed() }, n: 0 }
    }
}

/// What the workers share.
struct Ctx<'a> {
    file: &'a File,
    len: usize,
    region: &'a [AtomicU8], // region_id -> index of its country in the totals (0: no such region, which is _UNMATCHED)
    names: &'a Names,
}

/// The 8 digits in `w` (first digit in the lowest byte) as a number; padding bytes must be zero.
fn digits8(w: u64) -> u64 {
    let w = (w & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(2561) >> 8;
    let w = (w & 0x00FF_00FF_00FF_00FF).wrapping_mul(6553601) >> 16;
    (w & 0x0000_FFFF_0000_FFFF).wrapping_mul(42949672960001) >> 32
}

// The parser reads at most 64 bytes past a position before `end`; the buffer has `PAD` zero bytes after the data. A function
// that runs into `end` returns `Err(true)` (the record is not complete in this buffer); `Err(false)` means it is not valid.

/// The first `c` at or after `q` and before `end`, 32 bytes at a time.
#[inline(always)]
unsafe fn scan(b: &[u8], mut q: usize, end: usize, c: u8) -> Result<usize, bool> {
    loop {
        if q >= end {
            return Err(true);
        }
        let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(b.as_ptr().add(q) as _), _mm256_set1_epi8(c as i8))) as u32;
        if m != 0 {
            let k = q + m.trailing_zeros() as usize;
            return if k < end { Ok(k) } else { Err(true) };
        }
        q += 32;
    }
}

/// The decimal number (1 or more digits) at `b[p..]` that ends before the first `delim`, and its digit count.
#[inline(always)]
unsafe fn number(b: &[u8], p: usize, end: usize, delim: u8) -> Result<(u64, usize), bool> {
    let w = (b.as_ptr().add(p) as *const u64).read_unaligned();
    let x = w ^ 0x0101_0101_0101_0101 * delim as u64;
    let zero = x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080;
    let n = if zero != 0 { zero.trailing_zeros() as usize / 8 } else { scan(b, p, end, delim)? - p };
    if p + n >= end {
        return Err(true);
    }
    match n {
        0 => Err(false),
        1..=8 => Ok((digits8(w << (8 * (8 - n))), n)),
        _ => Ok(((p..p + n).fold(0, |v, k| v * 10 + (*b.get_unchecked(k) - b'0') as u64), n)),
    }
}

/// The text field at `b[p..]`: where its bytes as written start, how many there are, how many `""` pairs they hold, and the
/// position just after the field. An unquoted field has no quotes and ends at a comma.
#[inline(always)]
unsafe fn text(b: &[u8], p: usize, end: usize) -> Result<(usize, usize, usize, usize), bool> {
    if p >= end {
        return Err(true);
    }
    if *b.get_unchecked(p) != b'"' {
        let k = scan(b, p, end, b',')?;
        return Ok((p, k - p, 0, k));
    }
    let (mut q, mut pairs) = (p + 1, 0);
    loop {
        let k = scan(b, q, end, b'"')?;
        if *b.get_unchecked(k + 1) != b'"' {
            return Ok((p + 1, k - p - 1, pairs, k + 1));
        }
        (q, pairs) = (k + 2, pairs + 1);
    }
}

/// Parses the record that starts at `s` into `batch`, in one pass over its bytes; returns where the next record starts.
#[inline(always)]
unsafe fn record(c: &Ctx, b: &[u8], s: usize, end: usize, batch: &mut Batch) -> Result<usize, bool> {
    let at = |i: usize| *b.get_unchecked(i);
    if s + 3 >= end {
        return Err(true);
    }
    let bom = at(s) == 0xEF; // a BOM in front of the record
    if bom && (b.as_ptr().add(s + 1) as *const u16).read_unaligned() != 0xBFBB {
        return Err(false);
    }
    let (id, n) = number(b, s + 3 * bom as usize, end, b',')?;
    let (ns, len, pairs, p) = text(b, s + 3 * bom as usize + n + 1, end)?;
    if at(p) != b',' {
        return Err(p >= end);
    }
    let (_, note_raw, note_pairs, p) = text(b, p + 1, end)?;
    if at(p) != b',' {
        return Err(p >= end);
    }
    let (region, n) = number(b, p + 1, end, b',')?;
    let p = p + n + 2;
    let neg = at(p) == b'-';
    let (whole, n) = number(b, p + neg as usize, end, b'.')?;
    let dot = p + neg as usize + n; // the amount ends in ".dd", then CRLF or LF
    let lf = dot + 3 + (at(dot + 3) == b'\r') as usize;
    if lf >= end {
        return Err(true);
    }
    if at(lf) != b'\n' {
        return Err(false);
    }
    let cents = (whole * 100 + (at(dot + 1) - b'0') as u64 * 10 + (at(dot + 2) - b'0') as u64) as i64;
    let region = region as usize;
    // A name is looked up by its bytes as written, with `""` for each `"`: writing a value that way is one to one, so two names
    // are equal exactly when these bytes are. Its length after unescaping is one less per pair.
    let name = b.as_ptr().add(ns);
    // SAFETY: the buffer has 32 readable bytes after the name.
    let hash = hash(name, len);
    _mm_prefetch::<_MM_HINT_T0>(&c.names.tags[c.names.home(hash)] as *const AtomicU8 as *const i8);
    if let Some(r) = c.region.get(region) {
        _mm_prefetch::<_MM_HINT_T0>(r as *const AtomicU8 as *const i8);
    }
    let cents = if neg { -cents } else { cents };
    let (name_bytes, note_bytes) = ((len - pairs) as u32, (note_raw - note_pairs) as u32);
    *batch.recs.get_unchecked_mut(batch.n) = Rec { id, cents, name, hash, region: region.min(u32::MAX as usize) as u32, len: len as u32, name_bytes, note_bytes, hint: 0 };
    batch.n += 1;
    Ok(lf + 1)
}

/// Looks up the countries and names of the records in `batch` and adds them to `aggs`.
fn resolve(c: &Ctx, batch: &mut Batch, aggs: &mut [Agg]) {
    for r in &batch.recs[..batch.n] {
        let country = c.region.get(r.region as usize).map_or(0, |x| x.load(Relaxed)) as usize;
        // SAFETY: the name has 32 readable bytes after it, as `record` ensured.
        let name = unsafe { c.names.entry(r.name, r.len as usize, r.hash, r.hint) };
        name.mask[country / 64].fetch_or(1 << (country % 64), Relaxed);
        let a = &mut aggs[country];
        a.count += 1;
        a.id_sum += r.id;
        a.sum += r.cents;
        (a.min, a.max) = (a.min.min(r.cents), a.max.max(r.cents));
        a.name_bytes += r.name_bytes as u64;
        a.note_bytes += r.note_bytes as u64;
    }
    batch.n = 0;
}

/// The buffers of one worker, kept from chunk to chunk.
#[derive(Default)]
struct Worker {
    buf: Vec<u8>,
    batches: [Batch; 3],
}

/// Parses the records between `lo` and `hi`, which are record starts, into `aggs`. False if something is not as expected.
fn parse_chunk(c: &Ctx, lo: usize, hi: usize, aggs: &mut [Agg], w: &mut Worker) -> bool {
    let Worker { buf, batches } = w;
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
        let mut start = 0;
        loop {
            // SAFETY: the buffer has `PAD` bytes after `end`.
            match unsafe { record(c, buf, start, end, &mut batches[now]) } {
                Ok(next) => start = next,
                Err(more) if more => break,
                Err(_) => return false,
            }
            if batches[now].n == BATCH {
                // The batch before this one has had the slots of its names prefetched while this one was parsed: look at
                // them now, and prefetch the entries. The batch before that has had its entries prefetched: resolve it.
                let w = &mut batches[(now + 2) % 3];
                w.recs[..w.n].iter_mut().for_each(|r| r.hint = c.names.warm(r.hash));
                resolve(c, &mut batches[(now + 1) % 3], aggs);
                now = (now + 1) % 3;
            }
        }
        for k in [1, 2, 0] {
            // before the buffer changes
            let b = &mut batches[(now + k) % 3];
            b.recs[..b.n].iter_mut().for_each(|r| r.hint = c.names.warm(r.hash));
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
    // Countries get indices 1, 2, ... in the order the pieces meet them (0 is _UNMATCHED); the output is sorted afterwards.
    let (region, index, codes): (Vec<AtomicU8>, Vec<AtomicU8>, _) = (zeros(max_id + 1), zeros(26 * 26 * 26), Mutex::new(vec![]));
    let pieces = 48;
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
            // A line is "region_id,CCC" and an LF. The comma is found among the first 8 bytes (the lowest zero byte of
            // `w ^ ",,,,,,,,"`) unless the id has more than 7 digits.
            let w = u64::from_le_bytes(text[p..p + 8].try_into().unwrap());
            let x = w ^ 0x2C2C_2C2C_2C2C_2C2C;
            let zero = x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080;
            let i = p + if zero != 0 { zero.trailing_zeros() as usize / 8 } else { text[p..].iter().position(|&c| c == b',')? };
            let id = match i - p {
                0 => return None,
                n @ 1..=8 => digits8(w << (8 * (8 - n))) as usize,
                _ => text[p..i].iter().fold(0, |v, &c| v * 10 + (c - b'0') as usize),
            };
            let country = [text[i + 1], text[i + 2], text[i + 3]].map(|c| c.wrapping_sub(b'A') as usize);
            if text[i + 4] != b'\n' || id > max_id || country.iter().any(|&c| c >= 26) {
                return None;
            }
            let code = country[0] * 676 + country[1] * 26 + country[2];
            if index[code].load(Relaxed) == 0 {
                let mut codes = codes.lock().unwrap();
                if index[code].load(Relaxed) == 0 {
                    codes.push(code);
                    index[code].store(u8::try_from(codes.len()).ok()?, Relaxed);
                }
            }
            region[id].store(index[code].load(Relaxed), Relaxed);
            p = i + 5;
        }
        (p == len).then_some(())
    });
    parsed.iter().all(|r| r.is_some()).then_some(())?;
    // The output order: the countries by their codes, then _UNMATCHED.
    let mut order: Vec<(Vec<u8>, usize)> = (codes.into_inner().ok()?.into_iter())
        .map(|code| (vec![b'A' + (code / 676) as u8, b'A' + (code / 26 % 26) as u8, b'A' + (code % 26) as u8], index[code].load(Relaxed) as usize))
        .collect();
    order.sort();
    order.push((b"_UNMATCHED".to_vec(), 0));
    let countries = order.len(); // at most 256: the indices are bytes

    let names = Names::new(bits);
    let ctx = Ctx { file, len, region: &region, names: &names };
    let mut head = [0u8; 256];
    file.read_exact_at(&mut head[..len.min(256)], 0).ok()?;
    let body = head.iter().position(|&c| c == b'\n')? + 1; // the header has no quotes
    let chunks = len.div_ceil(chunk);
    // Where chunk j starts: the first record at or after its cut.
    let start = |j: usize| match j {
        0 => body,
        _ if j == chunks => len,
        _ => record_start(&ctx, j * chunk),
    };
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let results = parallel(threads, |_| {
        let (mut aggs, mut worker, mut ok) = (vec![NONE; countries], Worker::default(), true);
        loop {
            let j = next.fetch_add(1, Relaxed);
            if j >= chunks {
                return ok.then_some(aggs);
            }
            let (lo, hi) = (start(j), start(j + 1));
            ok &= lo >= hi || parse_chunk(&ctx, lo, hi, &mut aggs, &mut worker);
        }
    });
    if names.counts.full.load(Relaxed) || results.iter().any(|r| r.is_none()) {
        return None;
    }
    let mut total = vec![NONE; countries];
    for part in results.iter().flatten() {
        total.iter_mut().zip(part).for_each(|(t, a)| t.merge(a));
    }

    // Distinct names per country: the set bits of the masks, counted by ranges of entries in parallel.
    let used = names.entries.len();
    let parts = parallel(used.div_ceil(4096), |j| {
        let mut counts = vec![0u64; countries];
        for e in &names.entries[j * 4096..((j + 1) * 4096).min(used)] {
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
    for (key, c) in order.iter().filter(|(_, c)| total[*c].count > 0) {
        let (a, distinct) = (&total[*c], parts.iter().map(|p| p[*c]).sum::<u64>());
        out.extend_from_slice(key);
        let (sum, min, max) = (money(a.sum), money(a.min), money(a.max));
        let line = format!(" {} {} {sum} {min} {max} {distinct} {} {}\n", a.count, a.id_sum, a.name_bytes, a.note_bytes);
        out.extend_from_slice(line.as_bytes());
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
