//! Track A (ingest): per (hour, endpoint) request count, latency percentiles and 5xx rate.
//!
//! Phase 1: workers take 4 MiB chunks of the log, read them in blocks, parse each line with AVX2/SWAR and append a 6-byte
//! record (latency, endpoint id, 5xx flag) to a region private to (chunk, hour). Endpoint ids come from one shared
//! lock-free dictionary. Phase 2: workers take whole hours, counting-sort the records by endpoint, pick the
//! percentiles and format the lines; finished hours are written to the output file in order as soon as they are ready.

use std::arch::x86_64::*;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::*};
use std::sync::{Mutex, OnceLock};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
}

const THREADS: usize = 12;
const CHUNK: usize = 4 << 20;
const BLOCK: usize = 256 << 10; // bytes read at a time; small enough to stay in the core's cache while it is parsed
const HOURS: usize = 72;
const SLOTS: usize = 4096; // dictionary slots; the spec has at most 1,842 endpoints
const STRIDE: usize = 128; // bytes per stored endpoint; endpoints are 1..=125 bytes
/// A record is one `u32` word: latency in bits 0..20, endpoint id in bits 20..31, 5xx flag in bit 31. A latency that does not
/// fit (`SENT` or more) is stored as `SENT` and followed by a second word holding the whole latency.
const SENT: u32 = (1 << 20) - 1;
const SLACK: usize = 192; // bytes the parser may read past the start of a line
const MIN_LINE: usize = 40; // shortest possible line, bounds the records of one chunk
/// Bytes reserved for the records of one (chunk, hour): the whole chunk could fall in a single hour, with two words per line.
const REGION: usize = (((CHUNK + 256) / MIN_LINE + 4) * 8 + 8 + 4095) & !4095;
const DAYS: [&str; 4] = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"];

/// Endpoint dictionary shared by all workers: open addressing, slots are claimed with compare-and-swap.
/// A slot holds `hash tag << 32 | key length << 16 | id + 1`; the key bytes live in `keys`, `STRIDE` per id.
struct Dict {
    slots: Vec<AtomicU64>,
    lens: Vec<AtomicU8>,
    keys: usize,
    next: AtomicU32,
}

/// True if the first `len` bytes at `a` and `b` are equal. Both may be read up to 128 bytes past their start.
#[inline(always)]
unsafe fn key_eq(a: *const u8, b: *const u8, len: usize) -> bool {
    let block = |o: usize| {
        let (x, y) = (
            _mm256_loadu_si256(a.add(o) as _),
            _mm256_loadu_si256(b.add(o) as _),
        );
        _mm256_movemask_epi8(_mm256_cmpeq_epi8(x, y)) as u32 as u64
    };
    let need = u64::MAX >> (64 - len.min(64));
    if (block(0) | block(32) << 32) & need != need {
        return false;
    }
    (64..len).step_by(32).all(|o| {
        let need = u32::MAX >> (32 - (len - o).min(32));
        block(o) as u32 & need == need
    })
}

/// Hash of an endpoint from its first and last 16 bytes and its length; `key_eq` settles collisions.
#[inline(always)]
unsafe fn hash(s: *const u8, len: usize) -> u64 {
    let mix = |x: u128, k: u128| {
        let r = (x as u64 ^ k as u64) as u128 * ((x >> 64) as u64 ^ (k >> 64) as u64) as u128;
        r as u64 ^ (r >> 64) as u64
    };
    let head = (s as *const u128).read_unaligned() & (!0u128 >> (128 - 8 * len.min(16)));
    let tail =
        (s.add(len).sub(16) as *const u128).read_unaligned() & if len >= 16 { !0 } else { 0 };
    mix(head, 0x9E37_79B9_7F4A_7C15_D6E8_FEB8_6659_FD93)
        ^ mix(
            tail,
            0xA076_1D64_78BD_642F_E703_7ED1_A0B4_28DB ^ len as u128,
        )
}

impl Dict {
    fn new() -> Self {
        let keys = vec![0u8; SLOTS * STRIDE + 128].leak().as_mut_ptr() as usize;
        Dict {
            slots: (0..SLOTS).map(|_| AtomicU64::new(0)).collect(),
            lens: (0..SLOTS).map(|_| AtomicU8::new(0)).collect(),
            keys,
            next: AtomicU32::new(0),
        }
    }

    fn key(&self, id: usize) -> *const u8 {
        (self.keys + id * STRIDE) as *const u8
    }

    /// Id of the endpoint at `s` (`len` bytes), adding it if it is new.
    #[inline(always)]
    unsafe fn lookup(&self, s: *const u8, len: usize) -> u32 {
        let h = hash(s, len);
        let mut i = h as usize & (SLOTS - 1);
        loop {
            let v = self.slots[i].load(Acquire);
            if v == 0 {
                return self.insert(s, len, h, i);
            }
            if (v >> 32) as u32 == (h >> 32) as u32 && (v >> 16) as usize & 0xFF == len {
                let id = (v & 0xFFFF) as usize - 1;
                if key_eq(s, self.key(id), len) {
                    return id as u32;
                }
            }
            i = (i + 1) & (SLOTS - 1);
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn insert(&self, s: *const u8, len: usize, h: u64, mut i: usize) -> u32 {
        let id = self.next.fetch_add(1, Relaxed) as usize;
        std::ptr::copy_nonoverlapping(s, self.key(id) as *mut u8, len);
        self.lens[id].store(len as u8, Relaxed);
        let new = h & !0xFFFF_FFFF | (len as u64) << 16 | (id as u64 + 1);
        loop {
            match self.slots[i].compare_exchange(0, new, AcqRel, Acquire) {
                Ok(_) => return id as u32,
                Err(v) if (v >> 32) == (new >> 32) && (v >> 16) & 0xFF == len as u64 => {
                    // Another worker may have added this very endpoint first.
                    let other = (v & 0xFFFF) as usize - 1;
                    if key_eq(s, self.key(other), len) {
                        return other as u32;
                    }
                    i = (i + 1) & (SLOTS - 1);
                }
                Err(_) => i = (i + 1) & (SLOTS - 1),
            }
        }
    }
}

/// The 8 digits in `w` (first digit in the lowest byte) as a number; padding bytes must be zero.
#[inline(always)]
fn digits8(w: u64) -> u64 {
    let w = (w & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(2561) >> 8;
    let w = (w & 0x00FF_00FF_00FF_00FF).wrapping_mul(6553601) >> 16;
    (w & 0x0000_FFFF_0000_FFFF).wrapping_mul(42949672960001) >> 32
}

/// Parses the latency at `p` (1..=10 digits then a byte below '0'); returns it and its digit count.
#[inline(always)]
unsafe fn latency(p: *const u8) -> (u32, usize) {
    let below = _mm_cmplt_epi8(_mm_loadu_si128(p as _), _mm_set1_epi8(b'0' as i8));
    let n = (_mm_movemask_epi8(below) as u32).trailing_zeros() as usize;
    let v = if n <= 8 {
        digits8((p as *const u64).read_unaligned() << (8 * (8 - n)))
    } else {
        let lead = n - 8;
        let hi = (0..lead).fold(0, |a, i| a * 10 + (*p.add(i) & 15) as u64);
        hi * 100_000_000 + digits8((p.add(lead) as *const u64).read_unaligned())
    };
    (v as u32, n)
}

/// Hour 0..72 of the log from the 8 bytes "MM-DDTHH" of the timestamp (year and minutes are fixed).
#[inline(always)]
fn hour_index(key: u64) -> usize {
    let d = |i: u32| ((key >> (8 * i)) & 0xFF) as i32 - 48;
    let day = d(3) * 10 + d(4);
    let days = if d(1) == 0 { day - 30 } else { day + 1 };
    ((days * 24 + d(6) * 10 + d(7) - 18) as usize).min(HOURS - 1)
}

/// Parses the lines starting before `lim` and appends their records to the per-hour cursors `cur`.
/// Returns where it stopped. Every line must have `SLACK` readable bytes after its start.
unsafe fn scan(
    mut p: *const u8,
    lim: *const u8,
    dict: &Dict,
    cur: &mut [usize; HOURS],
) -> *const u8 {
    let (mut last, mut hour) = (0, 0);
    let space = _mm256_set1_epi8(b' ' as i8);
    while p < lim {
        let key = (p.add(5) as *const u64).read_unaligned();
        if key != last {
            (last, hour) = (key, hour_index(key));
        }
        // Bit i of `spaces` is set if the byte i after the timestamp is a space: method, endpoint, status, latency.
        let w = p.add(28);
        let find = |q: *const u8| {
            _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(q as _), space)) as u32 as u64
        };
        let spaces = find(w) | find(w.add(32)) << 32;
        let start = spaces.trailing_zeros() as usize + 1;
        let end = match spaces & spaces.wrapping_sub(1) {
            0 => (64..).find(|&i| *w.add(i) == b' ').unwrap(),
            more => more.trailing_zeros() as usize,
        };
        let id = dict.lookup(w.add(start), end - start);
        let failed = *w.add(end + 1) == b'5';
        let (lat, digits) = latency(w.add(end + 5));
        let word = lat.min(SENT) | (id | (failed as u32) << 11) << 20;
        (cur[hour] as *mut u64).write_unaligned(word as u64 | (lat as u64) << 32);
        cur[hour] += if lat < SENT { 4 } else { 8 };
        p = w.add(end + 5 + digits + 1);
    }
    p
}

/// State shared by all workers.
struct Job {
    input: File,
    len: usize,
    chunks: usize,
    arena: usize,
    counts: Vec<AtomicU32>,
    dict: Dict,
    next_chunk: AtomicUsize,
    next_hour: AtomicUsize,
    parsed: AtomicUsize,
    order: OnceLock<Vec<u16>>,
    ready: Vec<AtomicBool>,
    hours: Vec<Mutex<Vec<u8>>>,
    written: AtomicUsize,
    file: Mutex<Option<File>>,
    out: String,
}

impl Job {
    /// Address of the records of chunk `c` in hour `h`; an hour's regions are contiguous so they can be released at once.
    fn region(&self, h: usize, c: usize) -> usize {
        self.arena + (h * self.chunks + c) * REGION
    }

    /// Start of the first line that begins at or after byte `i`.
    fn cut(&self, i: usize) -> usize {
        if i == 0 || i >= self.len {
            return i.min(self.len);
        }
        let mut buf = [0u8; 256]; // longer than any line
        let n = (self.len - (i - 1)).min(buf.len());
        self.input
            .read_exact_at(&mut buf[..n], i as u64 - 1)
            .unwrap();
        buf[..n]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(self.len, |k| i + k)
    }

    /// Parses chunk `c` through `buf`, which holds one block plus the bytes carried over from the previous one.
    fn parse_chunk(&self, c: usize, buf: &mut [u8]) {
        let (mut at, hi) = (self.cut(c * CHUNK), self.cut((c + 1) * CHUNK));
        let mut cur: [usize; HOURS] = std::array::from_fn(|h| self.region(h, c));
        let mut keep = 0;
        loop {
            let n = (BLOCK - keep).min(hi - at);
            self.input
                .read_exact_at(&mut buf[keep..keep + n], at as u64)
                .unwrap();
            at += n;
            let (end, last) = (keep + n, at == hi);
            buf[end..end + SLACK].fill(0); // the parser may read past the last line
                                           // SAFETY: lines starting before `lim` are complete and have SLACK readable bytes after their start.
            let stop = unsafe {
                let lim = buf.as_ptr().add(if last { end } else { end - SLACK });
                scan(buf.as_ptr(), lim, &self.dict, &mut cur)
            };
            if last {
                break;
            }
            keep = end - (stop as usize - buf.as_ptr() as usize);
            buf.copy_within(end - keep..end, 0);
        }
        for (h, at) in cur.iter().enumerate() {
            self.counts[h * self.chunks + c]
                .store(((at - self.region(h, c)) / 4) as u32, Relaxed);
        }
    }

    /// Endpoint ids in the byte order of their endpoints.
    fn endpoint_order(&self) -> Vec<u16> {
        let key = |id: u16| unsafe {
            std::slice::from_raw_parts(
                self.dict.key(id as usize),
                self.dict.lens[id as usize].load(Relaxed) as usize,
            )
        };
        let mut ids: Vec<u16> = (0..self.dict.next.load(Relaxed) as u16).collect();
        ids.sort_unstable_by(|&a, &b| key(a).cmp(key(b)));
        ids
    }

    /// The summary lines of hour `h`.
    fn summarise(&self, h: usize, order: &[u16], scratch: &mut Scratch) -> Vec<u8> {
        let regions = || {
            (0..self.chunks).filter_map(move |c| {
                let n = self.counts[h * self.chunks + c].load(Relaxed) as usize;
                (n > 0).then(|| (self.region(h, c), n))
            })
        };
        let total: usize = regions().map(|r| r.1).sum();
        let (counts, starts, values) = (
            &mut scratch.counts,
            &mut scratch.starts,
            &mut scratch.values,
        );
        if values.len() < total {
            values.resize(total, 0);
        }
        each_record(regions(), |key, _| counts[(key & 0x7FF) as usize] += 1 + ((key as u64 >> 11) << 32));
        let mut sum = 0;
        for &id in order {
            starts[id as usize] = sum;
            sum += (counts[id as usize] as u32) as usize;
        }
        each_record(regions(), |key, lat| {
            let slot = &mut starts[(key & 0x7FF) as usize];
            values[*slot] = lat;
            *slot += 1;
        });
        let mut out = Vec::with_capacity(1 << 17);
        let abs = h + 18;
        let prefix = format!("{}T{:02}:00:00Z ", DAYS[abs / 24], abs % 24);
        for &id in order {
            let (n, errs) = (
                counts[id as usize] as u32 as usize,
                counts[id as usize] >> 32,
            );
            counts[id as usize] = 0;
            if n == 0 {
                continue;
            }
            let end = starts[id as usize];
            let (p50, p95, p99) = percentiles(&mut values[end - n..end]);
            out.extend_from_slice(prefix.as_bytes());
            out.extend_from_slice(unsafe {
                std::slice::from_raw_parts(
                    self.dict.key(id as usize),
                    self.dict.lens[id as usize].load(Relaxed) as usize,
                )
            });
            for v in [
                n as u64,
                p50 as u64,
                p95 as u64,
                p99 as u64,
                errs,
                errs * 1_000_000 / n as u64,
            ] {
                out.push(b' ');
                number(&mut out, v);
            }
            out.push(b'\n');
        }
        out
    }

    /// Hands over the lines of hour `h` and writes every hour that is now complete, in order.
    fn publish(&self, h: usize, lines: Vec<u8>) {
        *self.hours[h].lock().unwrap() = lines;
        self.ready[h].store(true, SeqCst);
        loop {
            let next = {
                let Ok(mut guard) = self.file.try_lock() else {
                    return;
                };
                let file = guard.get_or_insert_with(|| {
                    std::fs::create_dir_all(&self.out).unwrap();
                    File::create(format!("{}/summary.txt", self.out)).unwrap()
                });
                let mut next = self.written.load(SeqCst);
                while next < HOURS && self.ready[next].load(SeqCst) {
                    file.write_all(&std::mem::take(&mut *self.hours[next].lock().unwrap()))
                        .unwrap();
                    next += 1;
                    self.written.store(next, SeqCst);
                }
                next
            };
            // The lock is released before this check, so an hour published meanwhile is never missed.
            if next == HOURS || !self.ready[next].load(SeqCst) {
                return;
            }
        }
    }

    fn work(&self) {
        let mut buf = vec![0u8; BLOCK + SLACK];
        loop {
            let c = self.next_chunk.fetch_add(1, Relaxed);
            if c >= self.chunks {
                break;
            }
            self.parse_chunk(c, &mut buf);
        }
        if self.parsed.fetch_add(1, SeqCst) == THREADS - 1 {
            self.order.set(self.endpoint_order()).unwrap();
        }
        let order = loop {
            match self.order.get() {
                Some(o) => break o,
                None => std::thread::yield_now(),
            }
        };
        let mut scratch = Scratch {
            counts: vec![0; SLOTS],
            starts: vec![0; SLOTS],
            values: Vec::new(),
        };
        loop {
            let h = self.next_hour.fetch_add(1, Relaxed);
            if h >= HOURS {
                break;
            }
            let lines = self.summarise(h, order, &mut scratch);
            // MADV_DONTNEED: the records of this hour are no longer needed, so the output can reuse their memory.
            unsafe { madvise(self.region(h, 0) as *mut u8, self.chunks * REGION, 4) };
            self.publish(h, lines);
        }
    }
}

/// Per-worker buffers for `summarise`: per-endpoint count (low 32 bits) and 5xx count (high), group starts, values.
struct Scratch {
    counts: Vec<u64>,
    starts: Vec<usize>,
    values: Vec<u32>,
}

/// Calls `f(key, latency)` for each record in `regions` (address and word count of each); `key` is the endpoint id with the 5xx flag in bit 11.
fn each_record(regions: impl Iterator<Item = (usize, usize)>, mut f: impl FnMut(u32, u32)) {
    for (at, n) in regions {
        let word = |i: usize| unsafe { ((at + i * 4) as *const u32).read_unaligned() };
        let mut i = 0;
        while i < n {
            let w = word(i);
            let wide = w & SENT == SENT;
            f(w >> 20, if wide { word(i + 1) } else { w & SENT });
            i += 1 + wide as usize;
        }
    }
}

/// Nearest-rank p50, p95 and p99 of `v`, which it reorders.
fn percentiles(v: &mut [u32]) -> (u32, u32, u32) {
    let n = v.len();
    let at = |p: usize| (p * n + 99) / 100 - 1;
    if n <= 32 {
        for i in 1..n {
            let mut j = i;
            while j > 0 && v[j - 1] > v[j] {
                v.swap(j - 1, j);
                j -= 1;
            }
        }
        return (v[at(50)], v[at(95)], v[at(99)]);
    }
    // The median first: p95 and p99 are then found among the larger half only, which shrinks quickly.
    let (_, p50, upper) = v.select_nth_unstable(at(50));
    let (_, p95, upper) = upper.select_nth_unstable(at(95) - at(50) - 1);
    (*p50, *p95, *upper.select_nth_unstable(at(99) - at(95) - 1).1)
}

/// Appends `v` in decimal.
fn number(out: &mut Vec<u8>, mut v: u64) {
    let (mut digits, mut i) = ([0u8; 20], 20);
    loop {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

/// Maps `len` bytes of anonymous memory, which is only backed once touched, and never unmaps it.
fn map(len: usize) -> usize {
    // PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, no file.
    let at = unsafe { mmap(std::ptr::null_mut(), len, 3, 0x4022, -1, 0) };
    // MADV_NOHUGEPAGE: the touched parts are sparse, so with transparent huge pages every touched 2 MiB would be backed.
    unsafe { madvise(at, len, 15) };
    at as usize
}

fn main() {
    let (mut data, mut out) = ("/data".to_string(), "/out".to_string());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data" => data = args.next().unwrap(),
            "--out" => out = args.next().unwrap(),
            _ => {}
        }
    }
    let file = File::open(format!("{data}/requests.log")).expect("open input");
    let len = file.metadata().unwrap().len() as usize;
    let chunks = len.div_ceil(CHUNK);
    let arena = map(chunks * HOURS * REGION);
    let job = Job {
        input: file,
        len,
        chunks,
        arena,
        counts: (0..chunks * HOURS).map(|_| AtomicU32::new(0)).collect(),
        dict: Dict::new(),
        next_chunk: AtomicUsize::new(0),
        next_hour: AtomicUsize::new(0),
        parsed: AtomicUsize::new(0),
        order: OnceLock::new(),
        ready: (0..HOURS).map(|_| AtomicBool::new(false)).collect(),
        hours: (0..HOURS).map(|_| Mutex::new(Vec::new())).collect(),
        written: AtomicUsize::new(0),
        file: Mutex::new(None),
        out,
    };
    std::thread::scope(|s| {
        for _ in 1..THREADS {
            s.spawn(|| job.work());
        }
        job.work();
    });
}
