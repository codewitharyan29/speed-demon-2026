use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 96;
const BOM: &[u8] = b"\xef\xbb\xbf";

/// Maps a file read-only. The mapping is never unmapped, so the slice is 'static.
fn map(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    // SAFETY: PROT_READ (1), MAP_PRIVATE (2) on a file we just opened; the length is the file's.
    unsafe {
        std::slice::from_raw_parts(
            mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0),
            len,
        )
    }
}

/// Runs `f(state, job)` for every job in 0..jobs on THREADS threads that pull jobs from one shared counter.
/// `init` builds each thread's private state; the states come back when the jobs are done.
fn par<S: Send>(
    jobs: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) + Sync,
) -> Vec<S> {
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let mut state = init();
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return state;
                        }
                        f(&mut state, j);
                    }
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    })
}

/// Reads one CSV field starting at `p`. Returns (content start, content end, doubled quotes inside, index of the
/// delimiter byte that ended it: ',', '\r', '\n' or the end of the input).
fn field(b: &[u8], mut p: usize) -> (usize, usize, usize, usize) {
    if b.get(p) == Some(&b'"') {
        let start = p + 1;
        p = start;
        let mut doubled = 0;
        loop {
            while b[p] != b'"' {
                p += 1;
            }
            if b.get(p + 1) == Some(&b'"') {
                doubled += 1;
                p += 2;
            } else {
                return (start, p, doubled, p + 1);
            }
        }
    }
    let start = p;
    while p < b.len() && !matches!(b[p], b',' | b'\r' | b'\n') {
        p += 1;
    }
    (start, p, 0, p)
}

/// 64-bit hash of a name: multiply-fold over 8-byte words.
fn hash(s: &[u8]) -> u64 {
    let mut h = (s.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for c in s.chunks(8) {
        let mut w = [0u8; 8];
        w[..c.len()].copy_from_slice(c);
        let x = (h ^ u64::from_le_bytes(w)).wrapping_mul(0xff51_afd7_ed55_8ccd);
        h = x ^ (x >> 32);
    }
    let h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    (h ^ (h >> 29)) | 1 // never 0, which marks an empty slot
}

/// Open-addressing table from a name's hash to the set of countries it appears in, as a bitmask of `words` words.
struct Names {
    keys: Vec<u64>,
    masks: Vec<u64>,
    words: usize,
    len: usize,
}

impl Names {
    fn new(words: usize) -> Names {
        let cap = 1 << 18;
        Names { keys: vec![0; cap], masks: vec![0; cap * words], words, len: 0 }
    }

    fn slot(&mut self, h: u64) -> usize {
        let mask = self.keys.len() - 1;
        let mut i = (h >> 7) as usize & mask;
        while self.keys[i] != h {
            if self.keys[i] == 0 {
                self.keys[i] = h;
                self.len += 1;
                if self.len * 2 > self.keys.len() {
                    self.grow();
                    return self.slot(h);
                }
                break;
            }
            i = (i + 1) & mask;
        }
        i
    }

    fn grow(&mut self) {
        let cap = self.keys.len() * 2;
        let (keys, masks) = (std::mem::replace(&mut self.keys, vec![0; cap]), std::mem::take(&mut self.masks));
        self.masks = vec![0; self.keys.len() * self.words];
        self.len = 0;
        for (i, &k) in keys.iter().enumerate() {
            if k != 0 {
                let s = self.slot(k);
                self.masks[s * self.words..(s + 1) * self.words].copy_from_slice(&masks[i * self.words..(i + 1) * self.words]);
            }
        }
    }

    fn add(&mut self, h: u64, country: usize) {
        let s = self.slot(h);
        self.masks[s * self.words + country / 64] |= 1 << (country % 64);
    }
}

#[derive(Clone)]
struct Agg {
    count: u64,
    id_sum: u64,
    sum: i64,
    min: i64,
    max: i64,
    name_bytes: u64,
    note_bytes: u64,
}

struct State {
    aggs: Vec<Agg>,
    names: Names,
}

fn number(b: &[u8], p: &mut usize) -> u64 {
    let mut x = 0;
    while b[*p].is_ascii_digit() {
        x = x * 10 + (b[*p] - b'0') as u64;
        *p += 1;
    }
    x
}

/// Aggregates the records that start in `b[lo..hi]`; `lo` and `hi` are record starts.
fn run(b: &[u8], lo: usize, hi: usize, region: &[u16], st: &mut State) {
    let mut p = lo;
    let mut unescaped = Vec::new();
    while p < hi {
        if b[p..].starts_with(BOM) {
            p += BOM.len();
        }
        let id = number(b, &mut p);
        let (ns, ne, nd, np) = field(b, p + 1);
        let (ts, te, td, tp) = field(b, np + 1);
        p = tp + 1;
        let rid = number(b, &mut p) as usize;
        p += 1;
        let neg = b[p] == b'-';
        p += neg as usize;
        let whole = number(b, &mut p) as i64;
        p += 1;
        let cents = whole * 100 + (b[p] - b'0') as i64 * 10 + (b[p + 1] - b'0') as i64;
        p += 2;
        let cents = if neg { -cents } else { cents };
        if b.get(p) == Some(&b'\r') {
            p += 1;
        }
        if b.get(p) == Some(&b'\n') {
            p += 1;
        }

        let c = region.get(rid).copied().unwrap_or(0) as usize;
        let a = &mut st.aggs[c];
        a.count += 1;
        a.id_sum += id;
        a.sum += cents;
        a.min = a.min.min(cents);
        a.max = a.max.max(cents);
        a.name_bytes += (ne - ns - nd) as u64;
        a.note_bytes += (te - ts - td) as u64;
        let h = if nd == 0 {
            hash(&b[ns..ne])
        } else {
            unescaped.clear();
            let mut i = ns;
            while i < ne {
                unescaped.push(b[i]);
                i += if b[i] == b'"' { 2 } else { 1 };
            }
            hash(&unescaped)
        };
        st.names.add(h, c);
    }
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
    let r = map(&format!("{data}/regions.csv"));
    let b = map(&format!("{data}/records.csv"));

    // Countries are numbered from 1 in order of first appearance; 0 is _UNMATCHED.
    let mut keys: Vec<&[u8]> = vec![b"_UNMATCHED"];
    let mut ids: HashMap<&[u8], u16> = HashMap::new();
    let mut pairs = Vec::new();
    let mut p = r.iter().position(|&c| c == b'\n').unwrap() + 1;
    while p < r.len() {
        let mut q = p;
        let rid = number(r, &mut q) as usize;
        let (s, e, _, end) = field(r, q + 1);
        let country = &r[s..e];
        let next = *ids.entry(country).or_insert(keys.len() as u16);
        if next as usize == keys.len() {
            keys.push(country);
        }
        pairs.push((rid, next));
        p = end + if r.get(end) == Some(&b'\r') { 2 } else { 1 };
    }
    let mut region = vec![0u16; pairs.iter().map(|x| x.0).max().unwrap_or(0) + 1];
    for (rid, c) in pairs {
        region[rid] = c;
    }
    let nc = keys.len();
    let words = nc.div_ceil(64);

    // Quote parity of each chunk gives the quote state at every chunk start, so a chunk can find the first
    // record start at or after its cut even when newlines occur inside quoted fields.
    let mut cuts: Vec<usize> = (0..=CHUNKS).map(|i| b.len() / CHUNKS * i).collect();
    cuts[CHUNKS] = b.len();
    let quotes = {
        let counts = std::sync::Mutex::new(vec![0usize; CHUNKS]);
        par(CHUNKS, || (), |_, i| {
            let n = b[cuts[i]..cuts[i + 1]].iter().filter(|&&c| c == b'"').count();
            counts.lock().unwrap()[i] = n;
        });
        counts.into_inner().unwrap()
    };
    let mut starts = vec![b.iter().position(|&c| c == b'\n').unwrap() + 1];
    let mut inside = false;
    for i in 1..CHUNKS {
        inside ^= quotes[i - 1] % 2 == 1;
        let mut k = cuts[i];
        let mut q = inside;
        if b[k - 1] == b'\n' && !q {
            k -= 1;
        } else {
            while k < b.len() && !(b[k] == b'\n' && !q) {
                q ^= b[k] == b'"';
                k += 1;
            }
        }
        starts.push((k + 1).min(b.len()).max(*starts.last().unwrap()));
    }
    starts.push(b.len());

    let states = par(
        CHUNKS,
        || State {
            aggs: vec![Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 }; nc],
            names: Names::new(words),
        },
        |st, i| run(b, starts[i], starts[i + 1], &region, st),
    );

    let mut total = states[0].aggs.clone();
    total.iter_mut().for_each(|t| *t = Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 });
    let mut names = Names::new(words);
    for st in &states {
        for (t, a) in total.iter_mut().zip(&st.aggs) {
            t.count += a.count;
            t.id_sum += a.id_sum;
            t.sum += a.sum;
            t.min = t.min.min(a.min);
            t.max = t.max.max(a.max);
            t.name_bytes += a.name_bytes;
            t.note_bytes += a.note_bytes;
        }
    }
    let mut distinct = vec![0u64; nc];
    for st in &states {
        for (i, &k) in st.names.keys.iter().enumerate() {
            if k != 0 {
                let s = names.slot(k);
                for w in 0..words {
                    names.masks[s * words + w] |= st.names.masks[i * words + w];
                }
            }
        }
    }
    for (i, &k) in names.keys.iter().enumerate() {
        if k != 0 {
            for c in 0..nc {
                distinct[c] += names.masks[i * words + c / 64] >> (c % 64) & 1;
            }
        }
    }

    let mut order: Vec<usize> = (0..nc).filter(|&c| total[c].count > 0).collect();
    order.sort_unstable_by_key(|&c| keys[c]);
    let cents = |v: i64| format!("{}{}.{:02}", if v < 0 { "-" } else { "" }, v.abs() / 100, v.abs() % 100);
    let mut text = Vec::new();
    for c in order {
        let a = &total[c];
        text.extend_from_slice(keys[c]);
        text.extend_from_slice(
            format!(
                " {} {} {} {} {} {} {} {}\n",
                a.count, a.id_sum, cents(a.sum), cents(a.min), cents(a.max), distinct[c], a.name_bytes, a.note_bytes
            )
            .as_bytes(),
        );
    }
    std::fs::create_dir_all(&out).unwrap();
    File::create(format!("{out}/aggregates.txt")).unwrap().write_all(&text).unwrap();
}
