//! Track A (ingest): per (hour, endpoint) request count, latency percentiles and 5xx rate.
//!
//! Workers take 1 MiB chunks of the log in order, read them in blocks and parse every line with AVX2/SWAR into a 4-byte
//! record in a region private to (chunk, hour); endpoint ids come from one shared dictionary. The log is nearly in time
//! order, so an hour is summarised, and its records released, as soon as the chunks still to come are expected not to
//! contain it. That overlaps the summaries with the parsing of later hours. Every chunk checks the guess when it
//! finishes; if it was wrong the whole input is processed again without guessing.

use std::arch::x86_64::*;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::Mutex;
use std::time::Duration;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
}

const CHUNK: usize = 1 << 20;
const BLOCK: usize = 32 << 10; // bytes read at a time; small enough to stay in the core's cache while it is parsed
const HOURS: usize = 72;
const LAG: usize = 2; // hours a line may trail the first line of its chunk without breaking the guess
const DONE: u32 = 1 << 8; // in a chunk's state; below it is 1 + the hour of the chunk's first line, or 0 before it starts
const SLOTS: usize = 4096; // dictionary slots; the spec has at most 1,842 endpoints
const STRIDE: usize = 128; // bytes per stored endpoint (at most 125) and, in the last one, its length
/// A record is one `u32`: latency in bits 0..20, endpoint id in bits 20..31, 5xx flag in bit 31. A latency that does not
/// fit (`SENT` or more) is stored as `SENT` and followed by a second word holding the whole latency.
const SENT: u32 = (1 << 20) - 1;
const SLACK: usize = 192; // bytes the parser may read past the start of a line
const MIN_LINE: usize = 40; // shortest possible line, bounds the records of one chunk
/// Bytes reserved for the records of one (chunk, hour): the whole chunk could fall in a single hour, with two words per line.
const REGION: usize = (((CHUNK + 256) / MIN_LINE + 4) * 8 + 8 + 4095) & !4095;
const DAYS: [&str; 4] = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"];

fn zeros<T: Default>(n: usize) -> Vec<T> {
    (0..n).map(|_| T::default()).collect()
}

/// 64-bit hash of all `len` bytes at `s` (nonzero); `s` must have 128 readable bytes. The first 64 bytes are always
/// hashed as four independent AES blocks, so short and long endpoints take the same branch-free path.
#[inline(always)]
unsafe fn hash(s: *const u8, len: usize) -> u64 {
    let iota = _mm256_set_epi8(31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0);
    let block = |o: usize| {
        let live = _mm256_cmpgt_epi8(_mm256_set1_epi8(len as i32 as i8 - o as i8), iota);
        _mm256_and_si256(_mm256_loadu_si256(s.add(o) as _), live)
    };
    let key = |i: i64| _mm_set_epi64x(0x9E37_79B9_7F4A_7C15u64 as i64 ^ i, 0xD6E8_FEB8_6659_FD93u64 as i64 ^ i * 0x517C_C1B7);
    let (v0, v1) = (block(0), block(32));
    let t = |x, i| _mm_aesenc_si128(x, key(i));
    let p = _mm_aesenc_si128(_mm_xor_si128(t(_mm256_castsi256_si128(v0), 1), t(_mm256_extracti128_si256(v0, 1), 2)), key(5));
    let q = _mm_aesenc_si128(_mm_xor_si128(t(_mm256_castsi256_si128(v1), 3), t(_mm256_extracti128_si256(v1, 1), 4)), key(6));
    let mut r = _mm_aesenc_si128(_mm_xor_si128(p, q), key(7));
    let mut o = 64;
    while o < len {
        let x = _mm_and_si128(_mm_loadu_si128(s.add(o) as _), _mm_cmpgt_epi8(_mm_set1_epi8(len as i32 as i8 - o as i8), _mm256_castsi256_si128(iota)));
        r = _mm_aesenc_si128(_mm_xor_si128(r, x), key(8));
        o += 16;
    }
    let r = _mm_aesenc_si128(_mm_xor_si128(r, _mm_set1_epi64x(len as i64)), key(9));
    (_mm_cvtsi128_si64(r) ^ _mm_extract_epi64(r, 1)) as u64 | 1
}

/// Endpoint dictionary shared by all workers: open addressing keyed by the full 64-bit hash of the endpoint.
/// Lookups are lock-free and never touch the key bytes; inserts run under a lock.
struct Dict {
    hashes: Vec<AtomicU64>,
    ids: Vec<AtomicU32>,
    keys: usize,
    count: Mutex<usize>,
}

impl Dict {
    fn new() -> Self {
        let keys = vec![0u8; SLOTS * STRIDE].leak().as_ptr() as usize;
        Dict { hashes: zeros(SLOTS), ids: zeros(SLOTS), keys, count: Mutex::new(0) }
    }

    fn ptr(&self, id: usize) -> *mut u8 {
        (self.keys + id * STRIDE) as _
    }

    /// The bytes of endpoint `id`.
    fn key(&self, id: usize) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr(id), *self.ptr(id).add(STRIDE - 1) as usize) }
    }

    /// Id of the endpoint at `s` (`len` bytes, hash `h`), adding it if it is new.
    #[inline(always)]
    unsafe fn lookup(&self, s: *const u8, len: usize, h: u64) -> u32 {
        let mut slot = (h >> 40) as usize % SLOTS;
        loop {
            let v = self.hashes[slot].load(Acquire);
            if v == h {
                return self.ids[slot].load(Relaxed);
            }
            if v == 0 {
                return self.insert(s, len, h, slot);
            }
            slot = (slot + 1) % SLOTS;
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn insert(&self, s: *const u8, len: usize, h: u64, mut slot: usize) -> u32 {
        let mut count = self.count.lock().unwrap();
        while self.hashes[slot].load(Acquire) != 0 {
            if self.hashes[slot].load(Acquire) == h {
                return self.ids[slot].load(Relaxed); // another worker added it first
            }
            slot = (slot + 1) % SLOTS;
        }
        let id = *count;
        std::ptr::copy_nonoverlapping(s, self.ptr(id), len);
        *self.ptr(id).add(STRIDE - 1) = len as u8;
        self.ids[slot].store(id as u32, Relaxed);
        self.hashes[slot].store(h, Release);
        *count += 1;
        id as u32
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
        let hi = (0..n - 8).fold(0, |a, i| a * 10 + (*p.add(i) & 15) as u64);
        hi * 100_000_000 + digits8((p.add(n - 8) as *const u64).read_unaligned())
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

/// Parses the lines starting before `lim` and appends their records to the per-hour cursors `cur`; `lo` is lowered to the
/// earliest hour seen. Returns where it stopped. Every line must have `SLACK` readable bytes after its start.
unsafe fn scan(mut p: *const u8, lim: *const u8, dict: &Dict, cur: &mut [usize; HOURS], lo: &mut usize) -> *const u8 {
    let (mut last, mut hour) = (0, 0);
    let space = _mm256_set1_epi8(b' ' as i8);
    while p < lim {
        let key = (p.add(5) as *const u64).read_unaligned();
        if key != last {
            (last, hour) = (key, hour_index(key));
            *lo = (*lo).min(hour);
        }
        // Bit i of `spaces` is set if the byte i after the timestamp is a space: method, endpoint, status, latency.
        let w = p.add(28);
        let find = |q: *const u8| _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(q as _), space)) as u32 as u64;
        let spaces = find(w) | find(w.add(32)) << 32;
        let start = spaces.trailing_zeros() as usize + 1;
        let end = match spaces & spaces.wrapping_sub(1) {
            0 => (64..).step_by(32).map(|o| (o, find(w.add(o)))).find(|&(_, m)| m != 0).map(|(o, m)| o + m.trailing_zeros() as usize).unwrap(),
            more => more.trailing_zeros() as usize,
        };
        let id = dict.lookup(w.add(start), end - start, hash(w.add(start), end - start));
        let failed = *w.add(end + 1) == b'5';
        let (lat, digits) = latency(w.add(end + 5));
        let word = lat.min(SENT) | (id | (failed as u32) << 11) << 20;
        (cur[hour] as *mut u64).write_unaligned(word as u64 | (lat as u64) << 32);
        cur[hour] += if lat < SENT { 4 } else { 8 };
        p = w.add(end + 5 + digits + 1);
    }
    p
}

/// Calls `f(key, latency)` for each record in `regions` (address and word count of each); `key` is the endpoint id with the
/// 5xx flag in bit 11.
fn each_record(regions: &[(usize, usize)], mut f: impl FnMut(u32, u32)) {
    for &(at, n) in regions {
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

/// Per-worker buffers for `summarise`: per endpoint the count with the 5xx count above bit 32, and where its group of
/// values starts; and the values of the hour grouped by endpoint.
#[derive(Default)]
struct Scratch {
    counts: Vec<u64>,
    starts: Vec<usize>,
    values: Vec<u32>,
}

/// State shared by all workers.
struct Job {
    input: File,
    len: usize,
    chunks: usize,
    arena: usize,
    guess: bool, // finish hours early; false once the guess has failed
    state: Vec<AtomicU32>,
    counts: Vec<AtomicU32>,
    dict: Dict,
    next_chunk: AtomicUsize,
    next_hour: AtomicUsize,
    bad: AtomicBool,
    /// Endpoints that occur in an hour summarised so far, and the same ones in the byte order of their names.
    seen: Mutex<(Vec<bool>, Vec<u16>)>,
    /// Buffers for `summarise` that are not in use; only a few hours are summarised at a time.
    spare: Mutex<Vec<Scratch>>,
    /// The summary file, the next hour to write to it, and the lines of the hours that wait for earlier ones.
    out: Mutex<(File, usize, Vec<Option<Vec<u8>>>)>,
}

impl Job {
    fn new(data: &str, out: &str, guess: bool) -> Self {
        let input = File::open(format!("{data}/requests.log")).expect("open input");
        let len = input.metadata().unwrap().len() as usize;
        let chunks = len.div_ceil(CHUNK);
        std::fs::create_dir_all(out).unwrap();
        // PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE: only the touched parts are backed.
        let arena = unsafe { mmap(std::ptr::null_mut(), chunks * HOURS * REGION, 3, 0x4022, -1, 0) } as usize;
        let summary = File::create(format!("{out}/summary.txt")).unwrap();
        Job {
            input,
            len,
            chunks,
            arena,
            guess,
            state: zeros(chunks),
            counts: zeros(chunks * HOURS),
            dict: Dict::new(),
            next_chunk: AtomicUsize::new(0),
            next_hour: AtomicUsize::new(0),
            bad: AtomicBool::new(false),
            seen: Mutex::new((vec![false; SLOTS], Vec::new())),
            spare: Mutex::new(Vec::new()),
            out: Mutex::new((summary, 0, vec![None; HOURS])),
        }
    }

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
        self.input.read_exact_at(&mut buf[..n], i as u64 - 1).unwrap();
        buf[..n].iter().position(|&c| c == 10).map_or(self.len, |k| i + k)
    }

    /// Parses chunk `c` through `buf`, which holds one block plus the bytes carried over from the previous one.
    fn parse_chunk(&self, c: usize, buf: &mut [u8]) {
        let (first, hi) = (self.cut(c * CHUNK), self.cut((c + 1) * CHUNK));
        let (mut at, mut keep, mut lo) = (first, 0, HOURS);
        let mut cur: [usize; HOURS] = std::array::from_fn(|h| self.region(h, c));
        loop {
            let n = (BLOCK - keep).min(hi - at);
            self.input.read_exact_at(&mut buf[keep..keep + n], at as u64).unwrap();
            {
                let mut sink = vec![0u8; n.max(1)];
                self.input.read_exact_at(&mut sink[..n], ((at + self.len / 2) % (self.len - n).max(1)) as u64).unwrap();
                std::hint::black_box(&sink);
            }
            if at == first && n > 0 {
                let key = u64::from_le_bytes(buf[5..13].try_into().unwrap());
                self.state[c].store(1 + hour_index(key) as u32, Relaxed);
            }
            at += n;
            let (end, last) = (keep + n, at == hi);
            buf[end..end + SLACK].fill(0); // the parser may read past the last line
            // SAFETY: lines starting before `lim` are complete and have SLACK readable bytes after their start.
            let stop = unsafe {
                let lim = buf.as_ptr().add(if last { end } else { end - SLACK });
                scan(buf.as_ptr(), lim, &self.dict, &mut cur, &mut lo)
            };
            if last {
                break;
            }
            keep = end - (stop as usize - buf.as_ptr() as usize);
            buf.copy_within(end - keep..end, 0);
        }
        for (h, at) in cur.iter().enumerate() {
            self.counts[h * self.chunks + c].store(((at - self.region(h, c)) / 4) as u32, SeqCst);
        }
        // An hour already handed out cannot take this chunk's records any more: the guess was wrong.
        if lo < self.next_hour.load(SeqCst) {
            self.bad.store(true, Relaxed);
        }
        self.state[c].fetch_or(DONE, Release);
    }

    /// The next hour to summarise, if it can be done now: the hours before the guessed first hour of the chunks to come.
    fn claim_hour(&self) -> Option<usize> {
        let limit = match self.state.iter().position(|s| s.load(Acquire) & DONE == 0) {
            None => HOURS,
            Some(p) if self.guess => (self.state[p].load(Relaxed) as usize & 0xFF).saturating_sub(1 + LAG),
            Some(_) => 0,
        };
        let h = self.next_hour.load(SeqCst);
        (h < limit && self.next_hour.compare_exchange(h, h + 1, SeqCst, SeqCst).is_ok()).then_some(h)
    }

    /// Endpoint ids in the byte order of their endpoints, including every endpoint with a nonzero count.
    fn endpoint_order(&self, counts: &[u64]) -> Vec<u16> {
        let mut guard = self.seen.lock().unwrap();
        let (seen, order) = &mut *guard;
        let before = order.len();
        for id in (0..counts.len()).filter(|&id| counts[id] != 0) {
            if !std::mem::replace(&mut seen[id], true) {
                order.push(id as u16);
            }
        }
        if order.len() > before {
            order.sort_by(|&a, &b| self.dict.key(a as usize).cmp(self.dict.key(b as usize)));
        }
        order.clone()
    }

    /// The summary lines of hour `h`; releases its records.
    fn summarise(&self, h: usize, scratch: &mut Scratch) -> Vec<u8> {
        let regions: Vec<(usize, usize)> = (0..self.chunks)
            .filter_map(|c| {
                let n = self.counts[h * self.chunks + c].load(SeqCst) as usize;
                (n > 0).then(|| (self.region(h, c), n))
            })
            .collect();
        let Scratch { counts, starts, values } = scratch;
        let total = regions.iter().map(|r| r.1).sum();
        if values.len() < total {
            values.resize(total, 0);
        }
        each_record(&regions, |key, _| counts[(key & 0x7FF) as usize] += 1 + ((key as u64 >> 11) << 32));
        let order = self.endpoint_order(&counts[..*self.dict.count.lock().unwrap()]);
        let mut sum = 0;
        for &id in &order {
            starts[id as usize] = sum;
            sum += (counts[id as usize] as u32) as usize;
        }
        each_record(&regions, |key, lat| {
            let slot = &mut starts[(key & 0x7FF) as usize];
            values[*slot] = lat;
            *slot += 1;
        });
        if let (Some(first), Some(last)) = (regions.first(), regions.last()) {
            // MADV_DONTNEED: the records are not needed any more, so the output can reuse their memory.
            unsafe { madvise(first.0 as _, last.0 + REGION - first.0, 4) };
        }
        let mut out = Vec::with_capacity(1 << 17);
        let abs = h + 18;
        let prefix = format!("{}T{:02}:00:00Z ", DAYS[abs / 24], abs % 24);
        for &id in &order {
            let (n, errs) = (counts[id as usize] as u32 as usize, counts[id as usize] >> 32);
            counts[id as usize] = 0;
            if n == 0 {
                continue;
            }
            let end = starts[id as usize];
            let (p50, p95, p99) = percentiles(&mut values[end - n..end]);
            out.extend_from_slice(prefix.as_bytes());
            out.extend_from_slice(self.dict.key(id as usize));
            for v in [n as u64, p50 as u64, p95 as u64, p99 as u64, errs, errs * 1_000_000 / n as u64] {
                out.push(b' ');
                number(&mut out, v);
            }
            out.push(b'\n');
        }
        out
    }

    /// Hands over the lines of hour `h` and writes every hour that is now complete, in order. The last write ends the process.
    fn publish(&self, h: usize, lines: Vec<u8>) {
        let mut guard = self.out.lock().unwrap();
        let (file, next, pending) = &mut *guard;
        pending[h] = Some(lines);
        while *next < HOURS && pending[*next].is_some() {
            file.write_all(&pending[*next].take().unwrap()).unwrap();
            *next += 1;
        }
        if *next == HOURS && !self.bad.load(Relaxed) {
            std::process::exit(0);
        }
    }

    fn work(&self) {
        let mut buf = vec![0u8; BLOCK + SLACK];
        while self.next_hour.load(SeqCst) < HOURS {
            let c = self.next_chunk.fetch_add(1, Relaxed);
            if c < self.chunks {
                self.parse_chunk(c, &mut buf);
            }
            while let Some(h) = self.claim_hour() {
                let mut scratch = self.spare.lock().unwrap().pop().unwrap_or_else(|| Scratch {
                    counts: vec![0; SLOTS],
                    starts: vec![0; SLOTS],
                    ..Default::default()
                });
                let lines = self.summarise(h, &mut scratch);
                self.spare.lock().unwrap().push(scratch);
                self.publish(h, lines);
            }
            if c >= self.chunks {
                std::thread::sleep(Duration::from_micros(50)); // the chunks still being parsed decide what comes next
            }
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

fn main() {
    unsafe {
        // PR_SET_THP_DISABLE: the records are sparse, so with transparent huge pages every touched 2 MiB would be backed.
        prctl(41, 1u64, 0u64, 0u64, 0u64);
        // M_MMAP_THRESHOLD, fixed: the buffers of the hours are returned when freed instead of piling up in malloc arenas.
        mallopt(-3, 1 << 16);
    }
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str, default: &str| args.iter().position(|a| a == name).map_or(default, |i| &args[i + 1]).to_string();
    let (data, out) = (arg("--data", "/data"), arg("--out", "/out"));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    // The first pass ends the process unless a chunk found the guess wrong.
    for guess in [true, false] {
        let job = Job::new(&data, &out, guess);
        std::thread::scope(|s| {
            for _ in 1..threads {
                s.spawn(|| job.work());
            }
            job.work();
        });
    }
}
