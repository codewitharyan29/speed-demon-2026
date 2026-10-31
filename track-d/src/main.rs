use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const REGION: usize = 1 << 15;

/// Maps a file read-only. The mapping is never unmapped, so the slice is 'static.
fn map(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    // SAFETY: PROT_READ (1), MAP_PRIVATE (2) on a file we just opened; the length is the file's.
    unsafe {
        std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len)
    }
}

/// Next unsigned decimal at or after `*p`; skips any non-digit bytes first.
fn num(b: &[u8], p: &mut usize) -> u64 {
    while b[*p] < b'0' {
        *p += 1;
    }
    let mut x = 0;
    while *p < b.len() && b[*p] >= b'0' {
        x = x * 10 + (b[*p] - b'0') as u64;
        *p += 1;
    }
    x
}

/// Runs `f(state, job)` for every job in 0..jobs on THREADS threads that pull jobs from one shared counter,
/// so uneven jobs balance themselves. `init` builds each thread's private state. Results come back in job order.
fn par<S, R: Send>(
    jobs: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut all: Vec<(usize, R)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let (mut state, mut got) = (init(), Vec::new());
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return got;
                        }
                        got.push((j, f(&mut state, j)));
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

/// Monotone priority queue (keys popped never decrease, as in Dijkstra). An entry lives in the bucket given by the
/// highest bit in which its key differs from the last popped key; popping re-spreads one bucket into lower ones.
/// Each entry moves down at most 64 times and usually only a few, so this beats a binary heap's log-n sift.
struct Radix {
    bucket: Vec<Vec<(u64, u32)>>,
    last: u64,
}

impl Radix {
    fn new() -> Self {
        Radix {
            bucket: vec![Vec::new(); 65],
            last: 0,
        }
    }

    fn push(&mut self, key: u64, node: u32) {
        self.bucket[(64 - (key ^ self.last).leading_zeros()) as usize].push((key, node));
    }

    fn pop(&mut self) -> Option<(u64, u32)> {
        if self.bucket[0].is_empty() {
            let i = (1..65).find(|&i| !self.bucket[i].is_empty())?;
            let mut moved = std::mem::take(&mut self.bucket[i]);
            self.last = moved.iter().map(|e| e.0).min().unwrap();
            for (k, v) in moved.drain(..) {
                self.push(k, v);
            }
            self.bucket[i] = moved;
        }
        self.bucket[0].pop()
    }

    fn clear(&mut self) {
        self.bucket.iter_mut().for_each(Vec::clear);
        self.last = 0;
    }
}

fn main() {
    let g = map("/data/graph.txt");
    let mut p = 0;
    let n = num(g, &mut p) as usize;
    num(g, &mut p);
    // Cut the edge lines into CHUNKS pieces, each ending just after a newline.
    let (body, len) = (p + 1, g.len());
    let cut = |i: usize| match i {
        0 => body,
        CHUNKS => len,
        _ => {
            let at = body + (len - body) * i / CHUNKS - 1;
            at + g[at..].iter().position(|&c| c == b'\n').unwrap() + 1
        }
    };
    // Parse the chunks in parallel. Out-degrees and "has an in-edge" are recorded with atomics as we go.
    let degree: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let has_in: Vec<AtomicBool> = (0..n).map(|_| AtomicBool::new(false)).collect();
    let parsed = par(
        CHUNKS,
        || (),
        |_, c| {
            let (chunk, mut p, mut edges) = (&g[cut(c)..cut(c + 1)], 0, Vec::new());
            while p + 1 < chunk.len() {
                let (u, v, w) = (
                    num(chunk, &mut p) as u32,
                    num(chunk, &mut p) as u32,
                    num(chunk, &mut p) as u32,
                );
                if u != v {
                    edges.push((u, v, w));
                    degree[u as usize].fetch_add(1, Relaxed);
                    has_in[v as usize].store(true, Relaxed);
                }
            }
            edges
        },
    );
    let mut start = vec![0u32; n + 1];
    for i in 0..n {
        // The degree array turns into the write cursor: each tail's next free slot, starting at its first.
        start[i + 1] = start[i] + degree[i].swap(start[i], Relaxed);
    }
    // Counting sort by tail. An edge is packed as head << 32 | weight; slots are claimed with fetch_add, so the
    // order of parallel edges is arbitrary, which Dijkstra does not care about.
    let adj0: Vec<AtomicU64> = (0..start[n]).map(|_| AtomicU64::new(0)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            for &(u, v, w) in &parsed[c] {
                adj0[degree[u as usize].fetch_add(1, Relaxed) as usize]
                    .store((v as u64) << 32 | w as u64, Relaxed);
            }
        },
    );
    drop((parsed, degree));

    // Renumber nodes so that a search's neighbourhood is contiguous in memory. Ids are a random relabelling of a road
    // lattice, so every neighbour lookup would otherwise be a cache miss. Each thread repeatedly takes an unclaimed
    // node and breadth-first grows a region of up to REGION nodes around it; regions are numbered one after another.
    let claimed: Vec<AtomicBool> = (0..n).map(|_| AtomicBool::new(false)).collect();
    let scan = AtomicUsize::new(0);
    let regions = par(
        THREADS,
        || (),
        |_, _| {
            let mut regions = Vec::new();
            loop {
                let from = scan.fetch_add(1024, Relaxed);
                if from >= n {
                    return regions;
                }
                for root in from..(from + 1024).min(n) {
                    if claimed[root].swap(true, Relaxed) {
                        continue;
                    }
                    let (mut region, mut head) = (vec![root as u32], 0);
                    while head < region.len() && region.len() < REGION {
                        let u = region[head] as usize;
                        head += 1;
                        for e in &adj0[start[u] as usize..start[u + 1] as usize] {
                            let v = (e.load(Relaxed) >> 32) as usize;
                            if !claimed[v].load(Relaxed) && !claimed[v].swap(true, Relaxed) {
                                region.push(v as u32);
                            }
                        }
                    }
                    regions.push(region);
                }
            }
        },
    );
    let order: Vec<u32> = regions.into_iter().flatten().flatten().collect();
    let mut new_id = vec![0; n];
    for (i, &u) in order.iter().enumerate() {
        new_id[u as usize] = i as u32;
    }
    // Rebuild the CSR in the new numbering, a block of nodes per job.
    let mut first = vec![0u32; n + 1];
    for i in 0..n {
        let u = order[i] as usize;
        first[i + 1] = first[i] + start[u + 1] - start[u];
    }
    let adj: Vec<AtomicU64> = (0..first[n]).map(|_| AtomicU64::new(0)).collect();
    let block = n.div_ceil(CHUNKS);
    par(
        CHUNKS,
        || (),
        |_, c| {
            for i in c * block..((c + 1) * block).min(n) {
                let u = order[i] as usize;
                for (k, e) in adj0[start[u] as usize..start[u + 1] as usize]
                    .iter()
                    .enumerate()
                {
                    let x = e.load(Relaxed);
                    adj[first[i] as usize + k].store(
                        (new_id[(x >> 32) as usize] as u64) << 32 | x & 0xFFFF_FFFF,
                        Relaxed,
                    );
                }
            }
        },
    );

    drop(adj0);
    let q = map("/data/queries.txt");
    let mut p = 0;
    let queries: Vec<_> = (0..q.iter().filter(|&&c| c == b'\n').count())
        .map(|_| (num(q, &mut p) as usize, num(q, &mut p) as usize))
        .collect();
    let answers = par(
        queries.len(),
        || (vec![0; n], Vec::new(), Radix::new()),
        |(dist, touched, heap), i| {
            let (s, t) = queries[i];
            if s == t {
                0
            } else if !has_in[t].load(Relaxed) {
                -1
            } else {
                search(&first, &adj, new_id[s], new_id[t], dist, touched, heap)
            }
        },
    );

    let mut out = Vec::with_capacity(answers.len() * 8);
    for a in answers {
        if a < 0 {
            out.extend_from_slice(b"-1\n");
        } else {
            let mut buf = [b'\n'; 20];
            let (mut i, mut x) = (19, a as u64);
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
    }
    std::fs::write("/out/distances.txt", out).expect("write output");
}

/// Dijkstra from `s`, stopping when `t` is popped; -1 if unreachable.
/// `dist` holds distance + 1, with 0 meaning unseen: it is all zero on entry and restored on exit via `touched`, so
/// a query costs only its own search, and the zero-filled array is mapped lazily, so untouched pages cost no memory.
fn search(
    start: &[u32],
    adj: &[AtomicU64],
    s: u32,
    t: u32,
    dist: &mut [u64],
    touched: &mut Vec<u32>,
    heap: &mut Radix,
) -> i64 {
    dist[s as usize] = 1;
    touched.push(s);
    heap.push(0, s);
    let mut ans = -1;
    while let Some((d, u)) = heap.pop() {
        if d + 1 > dist[u as usize] {
            continue;
        }
        if u == t {
            ans = d as i64;
            break;
        }
        for e in &adj[start[u as usize] as usize..start[u as usize + 1] as usize] {
            let x = e.load(Relaxed);
            let (v, nd) = ((x >> 32) as usize, d + (x & 0xFFFF_FFFF));
            if dist[v] == 0 || nd + 1 < dist[v] {
                if dist[v] == 0 {
                    touched.push(v as u32);
                }
                dist[v] = nd + 1;
                heap.push(nd, v as u32);
            }
        }
    }
    heap.clear();
    for v in touched.drain(..) {
        dist[v as usize] = 0;
    }
    ans
}
