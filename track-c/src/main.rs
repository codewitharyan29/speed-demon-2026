//! Track C (join): aggregate `records.csv` per country of its `region_id`, as `regions.csv` says.
//!
//! Pipeline: (1) regions.csv is parsed in parallel into a direct-indexed table region_id -> country. (2) records.csv is
//! cut into chunks whose first record is found without looking at the data before the cut (see `record_start`), and the
//! chunks are parsed in parallel into per-chunk totals. A name is interned in one shared table (exact byte comparison)
//! whose entries carry a bitmask of the countries the name was seen in; "distinct names per country" is read off the
//! masks at the end. (3) Totals are merged and printed.
//!
//! The fast path relies on spec 2.2 in one place: inside a quoted field an LF is always followed by a lowercase letter
//! or a quote (notes are lowercase words), never by a digit 1-9 or a BOM, while a record terminator is. Every chunk
//! verifies that its parse ends exactly where the next chunk starts, and any inconsistency (a record longer than
//! `MAXREC`, more distinct names than the table holds, a country code that is not three capitals, ...) makes the
//! program redo everything with `safe`, a plain single-threaded parser of the grammar in spec 2.1.

use std::arch::x86_64::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering::*};

const BLOCK: usize = 64 << 10; // bytes read at a time
const MAXREC: usize = 1024; // a record longer than this is left to `safe`
const PAD: usize = 64; // zero bytes after the data in a buffer: the parser may read this far past a field
const CHUNK: usize = 1 << 20;
const NAME_BITS: u32 = 17; // 2^17 entries in the name table, filled to 80% at most: 104,857 names (the spec's pool has 100,000)

fn zeros<T: Default>(n: usize) -> Vec<T> {
    (0..n).map(|_| T::default()).collect()
}

/// Runs `f(0..jobs)` on all available threads, which pull jobs from one counter; the results come back in job order.
fn parallel<R: Send>(jobs: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = vec![];
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return done;
                        }
                        done.push((j, f(j)));
                    }
                })
            })
            .collect();
        let mut all: Vec<_> = workers.into_iter().flat_map(|w| w.join().unwrap()).collect();
        all.sort_by_key(|r| r.0);
        all.into_iter().map(|r| r.1).collect()
    })
}

/// The decimal number at `b[i..]`: its value and the index of the first non-digit after it.
unsafe fn digits(b: *const u8, mut i: usize) -> (u64, usize) {
    let mut v = 0u64;
    while (*b.add(i)).wrapping_sub(b'0') < 10 {
        v = v * 10 + (*b.add(i) - b'0') as u64;
        i += 1;
    }
    (v, i)
}

/// Index of the first `"` at or after `i`, or `end` if there is none before it.
unsafe fn quote(b: *const u8, mut i: usize, end: usize) -> usize {
    let q = _mm256_set1_epi8(b'"' as i8);
    while i < end {
        let hits = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(b.add(i) as _), q)) as u32;
        if hits != 0 {
            return (i + hits.trailing_zeros() as usize).min(end);
        }
        i += 32;
    }
    end
}

/// The text field at `i`: (start of its content, end of its content, number of `""` pairs in it, index of the byte
/// after the field). `None` if the field is not complete before `end`.
unsafe fn text(b: *const u8, i: usize, end: usize) -> Option<(usize, usize, usize, usize)> {
    if *b.add(i) != b'"' {
        let mut e = i;
        while e < end && !matches!(*b.add(e), b',' | b'\r' | b'\n') {
            e += 1;
        }
        return (e < end).then_some((i, e, 0, e));
    }
    let (mut q, mut doubled) = (i + 1, 0);
    loop {
        q = quote(b, q, end);
        if q + 1 >= end {
            return None;
        }
        if *b.add(q + 1) != b'"' {
            return Some((i + 1, q, doubled, q + 1));
        }
        (q, doubled) = (q + 2, doubled + 1);
    }
}

/// 64-bit hash of a name from its length and its first and last 16 bytes, which must be readable (`PAD` ensures it).
unsafe fn hash(s: *const u8, len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    let read = |p: *const u8| (p as *const u128).read_unaligned();
    let mix = |x: u128, k: u64| {
        let r = (x as u64 ^ k) as u128 * ((x >> 64) as u64 ^ 0xA076_1D64_78BD_642F) as u128;
        r as u64 ^ (r >> 64) as u64
    };
    let head = read(s) & (!0u128 >> (128 - 8 * len.min(16)));
    let tail = if len > 16 { read(s.add(len - 16)) } else { 0 };
    mix(head, 0x9E37_79B9_7F4A_7C15) ^ mix(tail ^ len as u128, 0xE703_7ED1_A0B4_28DB)
}

/// One distinct name, in a single cache line so that finding it, comparing it and recording its country touch one line.
#[repr(align(64))]
struct Entry {
    /// `hash tag << 32 | length << 8 | state`; the state is `INSERTING` while the name is being written, then `READY`.
    head: AtomicU64,
    /// Bit c is set once the name has been seen in country c.
    mask: [AtomicU64; 4],
    /// The name's first 24 bytes (zero padded); for a longer name the offset of its bytes in `Names::spill`.
    name: [AtomicU64; 3],
}

const INSERTING: u64 = 1;
const READY: u64 = 2;

/// Every distinct name seen, shared by all workers: an open-addressing table of `Entry`.
struct Names {
    table: &'static [Entry],
    spill: usize, // address of the bytes of names longer than 24
    spilled: AtomicUsize,
    filled: AtomicUsize,
    full: AtomicBool,
}

impl Names {
    const SPILL: usize = 4 << 20;

    fn new(bits: u32) -> Names {
        // Zeroed memory from the allocator is not touched until used, so only the entries that get filled in cost memory.
        let layout = std::alloc::Layout::array::<Entry>(1 << bits).unwrap();
        let table = unsafe { std::slice::from_raw_parts(std::alloc::alloc_zeroed(layout) as *const Entry, 1 << bits) };
        let spill = Box::leak(vec![0u8; Self::SPILL].into_boxed_slice()).as_ptr() as usize;
        Names { table, spill, spilled: AtomicUsize::new(0), filled: AtomicUsize::new(0), full: AtomicBool::new(false) }
    }

    /// The slot where the search for a name with hash `h` starts.
    fn slot(&self, h: u64) -> &Entry {
        &self.table[h as usize & (self.table.len() - 1)]
    }

    /// The entry of the name at `s` (`len` bytes, 32 more readable) with hash `h`, adding it if it is new.
    unsafe fn entry(&self, s: *const u8, len: usize, h: u64) -> &Entry {
        let (ready, claim) = (h & !0xFFFF_FFFF | (len as u64) << 8 | READY, h & !0xFFFF_FFFF | (len as u64) << 8 | INSERTING);
        // The name as three words, for the comparison with an inline name.
        let word = |k: usize| match len.saturating_sub(8 * k) {
            0 => 0,
            n if n >= 8 => (s.add(8 * k) as *const u64).read_unaligned(),
            n => (s.add(8 * k) as *const u64).read_unaligned() & ((1u64 << (8 * n)) - 1),
        };
        let mut i = h as usize & (self.table.len() - 1);
        loop {
            let e = &self.table[i];
            let v = e.head.load(Acquire);
            if v == ready {
                let same = if len <= 24 {
                    (0..3).all(|k| e.name[k].load(Relaxed) == word(k))
                } else {
                    let at = e.name[0].load(Relaxed) as usize;
                    std::slice::from_raw_parts((self.spill + at) as *const u8, len) == std::slice::from_raw_parts(s, len)
                };
                if same {
                    return e;
                }
            } else if v == 0 {
                if self.filled.fetch_add(1, Relaxed) * 10 > self.table.len() * 8 || self.spilled.load(Relaxed) + len > Self::SPILL {
                    self.full.store(true, Relaxed);
                    return e; // the run is abandoned; any entry will do
                }
                if e.head.compare_exchange(0, claim, Acquire, Acquire).is_ok() {
                    if len <= 24 {
                        (0..3).for_each(|k| e.name[k].store(word(k), Relaxed));
                    } else {
                        let at = self.spilled.fetch_add(len, Relaxed);
                        std::ptr::copy_nonoverlapping(s, (self.spill + at) as *mut u8, len);
                        e.name[0].store(at as u64, Relaxed);
                    }
                    e.head.store(ready, Release);
                    return e;
                }
                self.filled.fetch_sub(1, Relaxed);
                continue; // somebody else took the slot: look at what is in it now
            } else if v & 0xFF == INSERTING {
                std::hint::spin_loop();
                continue; // wait for the name being written to become readable
            }
            i = (i + 1) & (self.table.len() - 1);
        }
    }
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
    const NONE: Agg = Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 };

    fn merge(&mut self, o: &Agg) {
        let (min, max) = (self.min.min(o.min), self.max.max(o.max));
        *self = Agg {
            count: self.count + o.count,
            id_sum: self.id_sum + o.id_sum,
            sum: self.sum + o.sum,
            min,
            max,
            name_bytes: self.name_bytes + o.name_bytes,
            note_bytes: self.note_bytes + o.note_bytes,
        };
    }
}

/// What the workers share while parsing records.
struct Ctx<'a> {
    file: &'a File,
    len: usize,
    region: &'a [AtomicU16], // region_id -> packed country code + 1 (0: no such region)
    dense: &'a [u16],        // packed country code + 1 -> index into the totals; index 0 is _UNMATCHED's
    names: &'a Names,
}

/// Start of the first record that begins at or after `cut`, judged by the bytes after `cut - 1` alone: an LF followed
/// by a digit 1-9 or a BOM is a record terminator (inside a quoted field an LF is followed by a letter or a quote,
/// spec 2.2). The file length if there is none.
fn record_start(c: &Ctx, cut: usize) -> usize {
    let mut at = cut.max(1) - 1;
    let mut buf = [0u8; 512];
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

/// A parsed record whose name and country are still to be looked up. Records are looked up in batches: the parser
/// prefetches what a lookup will need and moves on, and by the time the batch is resolved the data has arrived.
struct Rec {
    id: u64,
    cents: i64,
    region: usize,
    name: *const u8,
    name_len: usize,
    hash: u64,
    note_bytes: usize,
}

const BATCH: usize = 16;
const SLOT: usize = 256; // scratch bytes per batched record for an unescaped name, which can be at most SLOT - 32 long

/// Looks up the records of `batch` and adds them to `aggs`.
unsafe fn resolve(c: &Ctx, batch: &mut Vec<Rec>, aggs: &mut [Agg]) {
    for r in batch.drain(..) {
        let code = c.region.get(r.region).map_or(0, |x| x.load(Relaxed));
        let country = c.dense[code as usize] as usize;
        let name = c.names.entry(r.name, r.name_len, r.hash);
        name.mask[country / 64].fetch_or(1 << (country % 64), Relaxed);
        let a = &mut aggs[country];
        a.count += 1;
        a.id_sum += r.id;
        a.sum += r.cents;
        a.min = a.min.min(r.cents);
        a.max = a.max.max(r.cents);
        a.name_bytes += r.name_len as u64;
        a.note_bytes += r.note_bytes as u64;
    }
}

/// Parses the records of one chunk (from `lo`, a record start, up to `hi`, the next one) into `aggs`.
/// Returns false if anything is not as the fast path expects.
fn parse_chunk(c: &Ctx, lo: usize, hi: usize, aggs: &mut [Agg]) -> bool {
    let (mut buf, mut batch, mut scratch) = (vec![0u8; MAXREC + BLOCK + PAD], Vec::with_capacity(BATCH), vec![0u8; SLOT * BATCH]);
    let (mut at, mut keep) = (lo, 0);
    loop {
        let n = BLOCK.min(hi - at);
        c.file.read_exact_at(&mut buf[keep..keep + n], at as u64).unwrap();
        at += n;
        let (end, last) = (keep + n, at == hi);
        buf[end..end + PAD].fill(0);
        let lim = if last { end } else { end.saturating_sub(MAXREC) };
        let mut p = 0;
        while p < lim {
            // SAFETY: the buffer is `PAD` zero bytes longer than `end`, and `record` does not look past `end` + `PAD`.
            unsafe {
                match record(c, buf.as_ptr(), p, end, &mut batch, &mut scratch) {
                    Some(next) => p = next,
                    None => return false,
                }
                if batch.len() == BATCH {
                    resolve(c, &mut batch, aggs);
                }
            }
        }
        unsafe { resolve(c, &mut batch, aggs) }; // before the buffer is reused
        if last {
            return p == end;
        }
        keep = end - p;
        buf.copy_within(p..end, 0);
    }
}

/// Parses the record at `p` into `batch`; returns where the next one starts.
unsafe fn record(c: &Ctx, b: *const u8, mut p: usize, end: usize, batch: &mut Vec<Rec>, scratch: &mut [u8]) -> Option<usize> {
    if *b.add(p) == 0xEF {
        p += 3; // a BOM in front of the record
    }
    let (id, i) = digits(b, p);
    let (ns, ne, nd, i) = text(b, i + 1, end)?;
    let (ts, te, td, i) = text(b, i + 1, end)?;
    let (region, i) = digits(b, i + 1);
    let neg = *b.add(i + 1) == b'-';
    let (whole, i) = digits(b, i + 1 + neg as usize);
    if *b.add(i) != b'.' {
        return None;
    }
    let cents = (whole * 100 + (*b.add(i + 1) - b'0') as u64 * 10 + (*b.add(i + 2) - b'0') as u64) as i64;
    p = i + 3;
    p += (*b.add(p) == b'\r') as usize;
    if *b.add(p) == b'\n' {
        p += 1;
    } else if p != end {
        return None; // neither a terminator nor the end of the input
    }
    let (mut name, mut len) = (b.add(ns), ne - ns);
    if nd > 0 {
        // Unescape into this record's slot of the scratch buffer (a name too long for that ends the fast path).
        let out = scratch.as_mut_ptr().add(batch.len() * SLOT);
        if len - nd > SLOT - 32 {
            return None;
        }
        let (mut k, mut n) = (ns, 0);
        while k < ne {
            *out.add(n) = *b.add(k);
            n += 1;
            k += if *b.add(k) == b'"' { 2 } else { 1 };
        }
        std::ptr::write_bytes(out.add(n), 0, 32);
        (name, len) = (out as *const u8, n);
    }
    let hash = hash(name, len);
    _mm_prefetch::<_MM_HINT_T0>(c.names.slot(hash) as *const Entry as *const i8);
    if let Some(r) = c.region.get(region as usize) {
        _mm_prefetch::<_MM_HINT_T0>(r as *const AtomicU16 as *const i8);
    }
    batch.push(Rec { id, cents: if neg { -cents } else { cents }, region: region as usize, name, name_len: len, hash, note_bytes: te - ts - td });
    Some(p)
}

fn money(v: i64) -> String {
    format!("{}{}.{:02}", if v < 0 { "-" } else { "" }, v.abs() / 100, v.abs() % 100)
}

/// The output for the countries `keys` (in output order) with totals `aggs` and distinct-name counts `distinct`.
fn render(keys: &[Vec<u8>], aggs: &[Agg], distinct: &[u64]) -> Vec<u8> {
    let mut out = vec![];
    for ((key, a), d) in keys.iter().zip(aggs).zip(distinct).filter(|((_, a), _)| a.count > 0) {
        out.extend_from_slice(key);
        let rest = format!(" {} {} {} {} {} {d} {} {}\n", a.count, a.id_sum, money(a.sum), money(a.min), money(a.max), a.name_bytes, a.note_bytes);
        out.extend_from_slice(rest.as_bytes());
    }
    out
}

/// The parallel fast path; `None` if anything made it give up.
fn fast(regions: &[u8], file: &File, len: usize) -> Option<Vec<u8>> {
    // regions.csv: a header, then "region_id,CCC\n" lines in ascending order, so the last line has the largest id.
    let first = regions.iter().position(|&c| c == b'\n')? + 1;
    let last = regions[..regions.len() - 1].iter().rposition(|&c| c == b'\n').map_or(first, |k| k + 1);
    let table_len = regions[last..].iter().take_while(|c| c.is_ascii_digit()).fold(0, |v, &c| v * 10 + (c - b'0') as usize) + 1;
    let region: Vec<AtomicU16> = zeros(table_len);
    let seen: Vec<AtomicBool> = zeros(26 * 26 * 26 + 1);
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
            let comma = line.iter().position(|&c| c == b',')?;
            let id = line[..comma].iter().fold(0, |v, &c| v * 10 + (c - b'0') as usize);
            let country = &line[comma + 1..];
            if country.len() != 3 || !country.iter().all(|c| c.is_ascii_uppercase()) || id >= table_len {
                return None;
            }
            let code = country.iter().fold(0, |v, &c| v * 26 + (c - b'A') as usize) + 1;
            region[id].store(code as u16, Relaxed);
            seen[code].store(true, Relaxed);
        }
        Some(())
    });
    parsed.iter().all(|r| r.is_some()).then_some(())?;
    // Countries get indices in the byte order of their codes (the order of the packed code); _UNMATCHED comes last.
    let (mut dense, mut keys) = (vec![0u16; seen.len()], vec![]);
    for code in (1..seen.len()).filter(|&c| seen[c].load(Relaxed)) {
        dense[code] = keys.len() as u16;
        let k = code - 1;
        keys.push(vec![b'A' + (k / 676) as u8, b'A' + (k / 26 % 26) as u8, b'A' + (k % 26) as u8]);
    }
    dense[0] = keys.len() as u16;
    keys.push(b"_UNMATCHED".to_vec());
    let countries = keys.len();

    if countries > 256 {
        return None; // the masks of `Entry` have room for 256 countries
    }
    let names = Names::new(NAME_BITS);
    let ctx = Ctx { file, len, region: &region, dense: &dense, names: &names };
    let mut head = [0u8; 256];
    file.read_exact_at(&mut head[..len.min(256)], 0).ok()?;
    let body = head.iter().position(|&c| c == b'\n')? + 1; // the header has no quotes
    let chunks = len.div_ceil(CHUNK);
    let results = parallel(chunks, |j| {
        let lo = if j == 0 { body } else { record_start(&ctx, j * CHUNK) };
        let hi = if j + 1 == chunks { len } else { record_start(&ctx, (j + 1) * CHUNK) };
        let mut aggs = vec![Agg::NONE; countries];
        (lo >= hi || parse_chunk(&ctx, lo, hi, &mut aggs)).then_some(aggs)
    });
    if names.full.load(Relaxed) || results.iter().any(|r| r.is_none()) {
        return None;
    }
    let mut total = vec![Agg::NONE; countries];
    results.iter().flatten().for_each(|a| total.iter_mut().zip(a).for_each(|(t, a)| t.merge(a)));

    // Distinct names per country: the set bits of the masks, counted by ranges of entries in parallel.
    let parts = parallel(names.table.len() / 4096, |j| {
        let mut counts = vec![0u64; countries];
        for e in &names.table[j * 4096..(j + 1) * 4096] {
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
    let distinct: Vec<u64> = (0..countries).map(|c| parts.iter().map(|p| p[c]).sum()).collect();
    Some(render(&keys, &total, &distinct))
}

/// A plain, single-threaded parser of spec 2.1 with exact sets of names; slow, and used only when `fast` gives up.
fn safe(regions: &[u8], b: &[u8]) -> Vec<u8> {
    let parse = |s: &[u8]| std::str::from_utf8(s).unwrap().parse::<i64>().unwrap();
    let lookup: HashMap<i64, &[u8]> = regions
        .split(|&c| c == b'\n')
        .skip(1)
        .filter(|l| !l.is_empty())
        .map(|l| l.split_at(l.iter().position(|&c| c == b',').unwrap()))
        .map(|(id, country)| (parse(id), &country[1..]))
        .collect();
    let bom: &[u8] = b"\xef\xbb\xbf";
    // The unescaped value of the field at `*p`, leaving `*p` on the delimiter after it.
    let field = |p: &mut usize| {
        let mut v = vec![];
        if b.get(*p) == Some(&b'"') {
            *p += 1;
            while b[*p] != b'"' || b.get(*p + 1) == Some(&b'"') {
                v.push(b[*p]);
                *p += if b[*p] == b'"' { 2 } else { 1 };
            }
            *p += 1;
        } else {
            while *p < b.len() && !matches!(b[*p], b',' | b'\r' | b'\n') {
                v.push(b[*p]);
                *p += 1;
            }
        }
        v
    };
    let mut groups: BTreeMap<Vec<u8>, (Agg, HashSet<Vec<u8>>)> = BTreeMap::new();
    let mut p = if b.starts_with(bom) { 3 } else { 0 };
    p += b[p..].iter().position(|&c| c == b'\n').unwrap() + 1; // the header
    while p < b.len() {
        if b[p..].starts_with(bom) {
            p += 3;
        }
        let id = parse(&field(&mut p));
        p += 1;
        let name = field(&mut p);
        p += 1;
        let note = field(&mut p);
        p += 1;
        let region = parse(&field(&mut p));
        p += 1;
        let cents = parse(&field(&mut p).iter().filter(|&&c| c != b'.').copied().collect::<Vec<_>>());
        p += if b[p..].starts_with(b"\r\n") { 2 } else { (p < b.len()) as usize };
        let key = lookup.get(&region).map_or(b"_UNMATCHED".to_vec(), |c| c.to_vec());
        let (a, names) = groups.entry(key).or_insert((Agg::NONE, HashSet::new()));
        a.merge(&Agg { count: 1, id_sum: id as u64, sum: cents, min: cents, max: cents, name_bytes: name.len() as u64, note_bytes: note.len() as u64 });
        names.insert(name);
    }
    let (keys, aggs): (Vec<_>, Vec<_>) = groups.iter().map(|(k, v)| (k.clone(), v.0)).unzip();
    let distinct: Vec<u64> = groups.values().map(|v| v.1.len() as u64).collect();
    render(&keys, &aggs, &distinct)
}

fn main() {
    let regions = std::fs::read("/data/regions.csv").unwrap();
    let file = File::open("/data/records.csv").unwrap();
    let len = file.metadata().unwrap().len() as usize;
    let text = fast(&regions, &file, len).unwrap_or_else(|| safe(&regions, &std::fs::read("/data/records.csv").unwrap()));
    File::create("/out/aggregates.txt").unwrap().write_all(&text).unwrap();
    std::process::exit(0);
}
