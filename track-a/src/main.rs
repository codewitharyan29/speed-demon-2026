//! Track A (ingest): per (hour, endpoint) request count, latency percentiles and 5xx rate.
//!
//! Phase 1: workers take 1 MiB chunks of the log, read them in small blocks (they stay in the core's cache while parsed)
//! and turn every line into a 4-byte record: latency (20 bits), endpoint id (11 bits), 5xx flag. A latency that does not fit
//! is stored as `SENT` followed by a second word with the whole value. Records go to a region private to (chunk, hour), so
//! workers never share a cache line. Each worker interns endpoints in its own hash map, with no locking.
//! Merge: the maps are merged into one sorted map, so an endpoint's global id is its rank in the output order.
//! Phase 2: workers take whole hours, counting-sort the records by endpoint, pick p50/p95/p99 with selection and print the
//! lines; hours are written in order as soon as they are ready, and an hour's records are released once it is sorted.
//!
//! The spec fixes the shape of a line (27-byte timestamp, 72 hours, 3-digit status), so only the day and hour digits of the
//! timestamp, the first digit of the status and the endpoint and latency are ever looked at.

use std::arch::x86_64::*;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::*};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
}

const CHUNK: usize = 1 << 20;
const BLOCK: usize = 32 << 10;
const HOURS: usize = 72;
const SENT: u32 = (1 << 20) - 1;
const SLACK: usize = 192; // bytes the parser may read past the start of a line
/// Bytes for the records of one (chunk, hour): a whole chunk could be one hour, at 40 bytes a line and two words a record.
const REGION: usize = (((CHUNK + 256) / 40 + 4) * 8 + 8 + 4095) & !4095;

/// Hashes an endpoint from its length and its first and last 16 bytes; the map settles collisions by comparing bytes.
/// Keys are stored with 16 spare bytes, and looked up inside the input buffer, so the 16-byte reads stay in bounds.
#[derive(Default)]
struct Ends(u64);

impl Hasher for Ends {
    fn write_usize(&mut self, _len: usize) {}

    fn write(&mut self, key: &[u8]) {
        let read = |p: *const u8| unsafe { (p as *const u128).read_unaligned() };
        let mix = |x: u128| {
            let r = (x as u64 ^ 0x9E37_79B9_7F4A_7C15) as u128 * ((x >> 64) as u64 ^ 0xA076_1D64_78BD_642F) as u128;
            r as u64 ^ (r >> 64) as u64
        };
        let n = key.len();
        let head = read(key.as_ptr()) & (!0u128 >> (128 - 8 * n.min(16)));
        let tail = if n > 16 { read(key[n - 16..].as_ptr()) } else { 0 };
        self.0 = mix(head) ^ mix(tail ^ n as u128);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type Dict = HashMap<Vec<u8>, u32, BuildHasherDefault<Ends>>;

/// The 8 digits in `w` (first digit in the lowest byte) as a number; padding bytes must be zero.
fn digits8(w: u64) -> u64 {
    let w = (w & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(2561) >> 8;
    let w = (w & 0x00FF_00FF_00FF_00FF).wrapping_mul(6553601) >> 16;
    (w & 0x0000_FFFF_0000_FFFF).wrapping_mul(42949672960001) >> 32
}

/// Parses the latency at `p` (1..=10 digits then a byte below '0'); returns it and its digit count.
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

/// Hour 0..72 of the log from the 8 bytes "MM-DDTHH" of a timestamp: 2026-10-30 18:00 is hour 0.
fn hour_index(key: u64) -> usize {
    let d = |i: u32| ((key >> (8 * i)) & 0xFF) as i32 - 48;
    let day = d(3) * 10 + d(4);
    let days = if d(1) == 0 { day - 30 } else { day + 1 };
    ((days * 24 + d(6) * 10 + d(7) - 18) as usize).min(HOURS - 1)
}

/// The id of `key` in `dict`, adding it if it is new.
fn intern(dict: &mut Dict, key: &[u8]) -> u32 {
    if let Some(&id) = dict.get(key) {
        return id;
    }
    let mut owned = vec![0u8; key.len() + 16];
    owned[..key.len()].copy_from_slice(key);
    owned.truncate(key.len());
    dict.insert(owned, dict.len() as u32);
    dict.len() as u32 - 1
}

/// Parses the lines starting before `lim` and appends their records to the per-hour cursors `cur`; returns where it
/// stopped. Every line must have `SLACK` readable bytes after its start.
unsafe fn scan(mut p: *const u8, lim: *const u8, dict: &mut Dict, cur: &mut [usize; HOURS]) -> *const u8 {
    let (mut last, mut hour) = (0, 0);
    let space = _mm256_set1_epi8(b' ' as i8);
    while p < lim {
        let key = (p.add(5) as *const u64).read_unaligned();
        if key != last {
            (last, hour) = (key, hour_index(key));
        }
        // Bit i of `spaces` is set if byte 28 + i is a space: after the method, the endpoint, the status, the latency.
        let w = p.add(28);
        let find = |q: *const u8| _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(q as _), space)) as u32 as u64;
        let spaces = find(w) | find(w.add(32)) << 32;
        let start = spaces.trailing_zeros() as usize + 1;
        let end = match spaces & spaces.wrapping_sub(1) {
            0 => (64..).find(|&i| *w.add(i) == b' ').unwrap(),
            more => more.trailing_zeros() as usize,
        };
        let id = intern(dict, std::slice::from_raw_parts(w.add(start), end - start));
        let failed = *w.add(end + 1) == b'5';
        let (lat, digits) = latency(w.add(end + 5));
        let word = lat.min(SENT) | (id | (failed as u32) << 11) << 20;
        (cur[hour] as *mut u64).write_unaligned(word as u64 | (lat as u64) << 32);
        cur[hour] += if lat < SENT { 4 } else { 8 };
        p = w.add(end + 5 + digits + 1);
    }
    p
}

/// Start of the first line that begins at or after byte `i` of `file`, which is `len` bytes long.
fn cut(file: &File, len: usize, i: usize) -> usize {
    if i == 0 || i >= len {
        return i.min(len);
    }
    let mut buf = [0u8; 256]; // longer than any line
    let n = (len - (i - 1)).min(buf.len());
    file.read_exact_at(&mut buf[..n], i as u64 - 1).unwrap();
    buf[..n].iter().position(|&c| c == b'\n').map_or(len, |k| i + k)
}

/// Calls `f(endpoint, failed, latency)` for each record in `regions` (address, word count and owner of each); `remap`
/// turns the owner's endpoint ids into global ones.
fn each_record(regions: &[(usize, usize, usize)], remap: &[Vec<u32>], mut f: impl FnMut(usize, bool, u32)) {
    for &(at, n, owner) in regions {
        let word = |i: usize| unsafe { ((at + i * 4) as *const u32).read_unaligned() };
        let mut i = 0;
        while i < n {
            let w = word(i);
            let wide = w & SENT == SENT;
            f(remap[owner][(w >> 20 & 0x7FF) as usize] as usize, w >> 31 != 0, if wide { word(i + 1) } else { w & SENT });
            i += 1 + wide as usize;
        }
    }
}

/// Nearest-rank p50, p95 and p99 of `v`, which it reorders.
fn percentiles(v: &mut [u32]) -> [u32; 3] {
    let n = v.len();
    let at = |p: usize| (p * n + 99) / 100 - 1;
    if n <= 32 {
        v.sort_unstable();
        return [v[at(50)], v[at(95)], v[at(99)]];
    }
    // The median first: p95 and p99 are then found among the larger half only, which shrinks quickly.
    let (_, p50, upper) = v.select_nth_unstable(at(50));
    let (_, p95, upper) = upper.select_nth_unstable(at(95) - at(50) - 1);
    [*p50, *p95, *upper.select_nth_unstable(at(99) - at(95) - 1).1]
}

fn main() {
    let input = File::open("/data/requests.log").unwrap();
    let len = input.metadata().unwrap().len() as usize;
    let chunks = len.div_ceil(CHUNK);
    let threads = std::thread::available_parallelism().unwrap().get();
    // Sparse anonymous memory, backed only where touched: PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE.
    let arena = unsafe { mmap(std::ptr::null_mut(), chunks * HOURS * REGION, 3, 0x4022, -1, 0) } as usize;
    // MADV_NOHUGEPAGE: with transparent huge pages every touched 2 MiB of the sparse arena would be backed.
    unsafe { madvise(arena as _, chunks * HOURS * REGION, 15) };
    let region = |h: usize, c: usize| arena + (h * chunks + c) * REGION;
    let counts: Vec<AtomicU32> = (0..chunks * HOURS).map(|_| AtomicU32::new(0)).collect();

    // Phase 1: each worker returns its endpoint map and the chunks it parsed.
    let next = AtomicUsize::new(0);
    let workers: Vec<(Dict, Vec<usize>)> = std::thread::scope(|s| {
        let spawned: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let (mut dict, mut mine, mut buf) = (Dict::default(), vec![], vec![0u8; BLOCK + SLACK]);
                    loop {
                        let c = next.fetch_add(1, Relaxed);
                        if c >= chunks {
                            return (dict, mine);
                        }
                        let (mut at, hi) = (cut(&input, len, c * CHUNK), cut(&input, len, (c + 1) * CHUNK));
                        let (mut keep, mut cur) = (0, std::array::from_fn(|h| region(h, c)));
                        loop {
                            let n = (BLOCK - keep).min(hi - at);
                            input.read_exact_at(&mut buf[keep..keep + n], at as u64).unwrap();
                            at += n;
                            let (end, last) = (keep + n, at == hi);
                            buf[end..end + SLACK].fill(0); // the parser may read past the last line
                            let lim = if last { end } else { end - SLACK };
                            // SAFETY: lines starting before `lim` are complete and have SLACK readable bytes after them.
                            let stop = unsafe { scan(buf.as_ptr(), buf.as_ptr().add(lim), &mut dict, &mut cur) };
                            if last {
                                break;
                            }
                            keep = end - (stop as usize - buf.as_ptr() as usize);
                            buf.copy_within(end - keep..end, 0);
                        }
                        for (h, at) in cur.iter().enumerate() {
                            counts[h * chunks + c].store(((at - region(h, c)) / 4) as u32, Relaxed);
                        }
                        mine.push(c);
                    }
                })
            })
            .collect();
        spawned.into_iter().map(|w| w.join().unwrap()).collect()
    });

    // Merge: a global id is the rank of the endpoint in byte order; `remap` translates each worker's ids.
    let mut sorted: BTreeMap<&[u8], u32> = workers.iter().flat_map(|w| w.0.keys()).map(|k| (k.as_slice(), 0)).collect();
    sorted.values_mut().enumerate().for_each(|(rank, id)| *id = rank as u32);
    let keys: Vec<&[u8]> = sorted.keys().copied().collect();
    let mut owner = vec![0; chunks];
    let mut remap = vec![];
    for (t, (dict, mine)) in workers.iter().enumerate() {
        remap.push(vec![0u32; dict.len()]);
        dict.iter().for_each(|(k, &id)| remap[t][id as usize] = sorted[k.as_slice()]);
        mine.iter().for_each(|&c| owner[c] = t);
    }

    // Phase 2.
    let (next, written) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let summary = File::create("/out/summary.txt").unwrap();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let (mut tally, mut starts) = (vec![0u64; keys.len()], vec![0usize; keys.len()]);
                let mut values = vec![];
                loop {
                    let h = next.fetch_add(1, Relaxed);
                    if h >= HOURS {
                        return;
                    }
                    let regions: Vec<_> = (0..chunks)
                        .filter_map(|c| match counts[h * chunks + c].load(Relaxed) as usize {
                            0 => None,
                            n => Some((region(h, c), n, owner[c])),
                        })
                        .collect();
                    values.resize(regions.iter().map(|r| r.1).sum(), 0);
                    // Per endpoint: the count, with the 5xx count above bit 32, and where its group of values starts.
                    each_record(&regions, &remap, |g, failed, _| tally[g] += 1 + ((failed as u64) << 32));
                    let mut sum = 0;
                    for (start, count) in starts.iter_mut().zip(&tally) {
                        (*start, sum) = (sum, sum + *count as u32 as usize);
                    }
                    each_record(&regions, &remap, |g, _, lat| {
                        values[starts[g]] = lat;
                        starts[g] += 1;
                    });
                    if let (Some(first), Some(last)) = (regions.first(), regions.last()) {
                        // MADV_DONTNEED: the records are not needed any more, so the output can reuse their memory.
                        unsafe { madvise(first.0 as _, last.0 + REGION - first.0, 4) };
                    }
                    let abs = h + 18;
                    let date = ["10-30", "10-31", "11-01", "11-02"][abs / 24];
                    let mut lines = vec![];
                    for (g, key) in keys.iter().enumerate() {
                        let (n, errs) = (tally[g] as u32 as usize, tally[g] >> 32);
                        tally[g] = 0;
                        if n > 0 {
                            let [p50, p95, p99] = percentiles(&mut values[starts[g] - n..starts[g]]);
                            write!(lines, "2026-{date}T{:02}:00:00Z ", abs % 24).unwrap();
                            lines.extend_from_slice(key);
                            writeln!(lines, " {n} {p50} {p95} {p99} {errs} {}", errs * 1_000_000 / n as u64).unwrap();
                        }
                    }
                    while written.load(Acquire) != h {
                        std::thread::yield_now(); // hours are written in order
                    }
                    (&summary).write_all(&lines).unwrap();
                    written.store(h + 1, Release);
                }
            });
        }
    });
}
