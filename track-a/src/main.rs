use std::collections::HashMap;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const SHARDS: usize = 12; // by hour of day, so shards are merged and summarised independently
const TS_LEN: usize = 27; // 2026-10-30T18:00:00.116445Z
const HOUR_LEN: usize = 13; // 2026-10-30T18

/// Multiplicative hash over the key bytes; the keys are short and not adversarial.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type Key = ([u8; HOUR_LEN], &'static [u8]);
type Groups = HashMap<Key, (u64, Vec<u32>), BuildHasherDefault<Fx>>;

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

/// Runs `f(job)` for every job in 0..jobs on THREADS threads that pull jobs from one shared counter.
/// Results come back in job order.
fn par<R: Send>(jobs: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut all: Vec<(usize, R)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let mut got = Vec::new();
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return got;
                        }
                        got.push((j, f(j)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect()
    });
    all.sort_unstable_by_key(|r| r.0);
    all.into_iter().map(|r| r.1).collect()
}

/// Groups the requests of one newline-aligned chunk by (hour, endpoint).
fn scan(b: &'static [u8]) -> Vec<Groups> {
    let mut groups: Vec<Groups> = (0..SHARDS).map(|_| Groups::default()).collect();
    let mut p = 0;
    while p < b.len() {
        let hour: [u8; HOUR_LEN] = b[p..p + HOUR_LEN].try_into().unwrap();
        p += TS_LEN + 1;
        while b[p] != b' ' {
            p += 1;
        }
        let s = p + 1;
        p = s;
        while b[p] != b' ' {
            p += 1;
        }
        let endpoint = &b[s..p];
        p += 1;
        let err = b[p] == b'5';
        p += 4;
        let mut latency = 0u32;
        while p < b.len() && b[p] != b'\n' {
            latency = latency * 10 + (b[p] - b'0') as u32;
            p += 1;
        }
        p += 1;
        let shard = ((hour[11] - b'0') as usize * 10 + (hour[12] - b'0') as usize) % SHARDS;
        let g = groups[shard].entry((hour, endpoint)).or_default();
        g.0 += err as u64;
        g.1.push(latency);
    }
    groups
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
    let b = map(&format!("{data}/requests.log"));

    let mut cuts = vec![0];
    for i in 1..CHUNKS {
        let mut c = (b.len() / CHUNKS * i).max(*cuts.last().unwrap());
        while c < b.len() && b[c - 1] != b'\n' {
            c += 1;
        }
        cuts.push(c);
    }
    cuts.push(b.len());

    let parts = par(CHUNKS, |i| scan(&b[cuts[i]..cuts[i + 1]]));
    let shards = par(SHARDS, |sh| {
        let mut merged: HashMap<Key, (u64, Vec<&Vec<u32>>), BuildHasherDefault<Fx>> = HashMap::default();
        for part in &parts {
            for (k, (e, v)) in &part[sh] {
                let g = merged.entry(*k).or_default();
                g.0 += e;
                g.1.push(v);
            }
        }
        let (mut buf, mut index) = (Vec::new(), Vec::new());
        for ((hour, endpoint), (errs, lists)) in merged {
            let mut v: Vec<u32> = lists.into_iter().flatten().copied().collect();
            v.sort_unstable();
            let n = v.len();
            let at = |p: usize| v[(p * n + 99) / 100 - 1];
            let start = buf.len();
            buf.extend_from_slice(&hour);
            buf.extend_from_slice(b":00:00Z ");
            buf.extend_from_slice(endpoint);
            let tail = format!(
                " {n} {} {} {} {errs} {}
",
                at(50),
                at(95),
                at(99),
                errs * 1_000_000 / n as u64
            );
            buf.extend_from_slice(tail.as_bytes());
            index.push(((hour, endpoint), start, buf.len()));
        }
        (buf, index)
    });

    let mut order: Vec<(Key, usize, usize, usize)> = Vec::new();
    for (i, (_, index)) in shards.iter().enumerate() {
        order.extend(index.iter().map(|&(k, s, e)| (k, i, s, e)));
    }
    order.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut text = Vec::new();
    for (_, i, s, e) in order {
        text.extend_from_slice(&shards[i].0[s..e]);
    }

    std::fs::create_dir_all(&out).unwrap();
    File::create(format!("{out}/summary.txt"))
        .unwrap()
        .write_all(&text)
        .unwrap();
}
