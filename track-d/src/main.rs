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

/// The unsigned decimal that starts at b[*p], with *p moved past it and the one separator after it. Up to seven
/// digits are converted at once (SWAR): the first byte below '0' ends the number, and the digits, shifted to the top
/// of the word so that the empty bytes read as leading zeros, are combined pairwise in three multiplications (the
/// method of fast_float's parse_eight_digits). Longer numbers, and the last few bytes of `b`, take the plain loop.
fn fast_num(b: &[u8], p: &mut usize) -> u64 {
    let raw = b
        .get(*p..*p + 8)
        .map(|w| u64::from_le_bytes(w.try_into().unwrap()));
    let len = raw.map_or(8, |r| {
        (r.wrapping_sub(0x3030303030303030) & 0x8080808080808080).trailing_zeros() / 8
    });
    let Some(raw) = raw.filter(|_| len < 8) else {
        let x = num(b, p);
        *p += 1;
        return x;
    };
    let x = raw << (8 * (8 - len));
    let x = ((x & 0x0F0F0F0F0F0F0F0F).wrapping_mul(2561) >> 8) & 0x00FF00FF00FF00FF;
    let x = (x.wrapping_mul(6553601) >> 16) & 0x0000FFFF0000FFFF;
    *p += len as usize + 1;
    x.wrapping_mul(42949672960001) >> 32
}

/// Runs `f(state, job)` for every job in 0..jobs on THREADS threads. Each thread first works through its own
/// contiguous share of the jobs, so neighbouring jobs run on the same thread, and then takes jobs from the shares
/// the others have not finished, so uneven jobs still balance. `init` builds each thread's private state. Results come
/// back in job order.
fn par<S, R: Send>(
    jobs: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<R> {
    let share = |t: usize| t * jobs / THREADS;
    let next: Vec<AtomicUsize> = (0..THREADS).map(|t| AtomicUsize::new(share(t))).collect();
    let mut all: Vec<(usize, R)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|me| {
                let (init, f, next) = (&init, &f, &next);
                sc.spawn(move || {
                    let (mut state, mut got) = (init(), Vec::new());
                    for t in (me..THREADS).chain(0..me) {
                        loop {
                            let j = next[t].fetch_add(1, Relaxed);
                            if j >= share(t + 1) {
                                break;
                            }
                            got.push((j, f(&mut state, j)));
                        }
                    }
                    got
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
            let chunk = &g[cut(c)..cut(c + 1)];
            // An edge line has about 18 bytes: sized from that, the list never has to grow (and briefly double).
            let (mut p, mut edges) = (0, Vec::with_capacity(chunk.len() / 16 + 16));
            while p < chunk.len() {
                let (u, v, w) = (
                    fast_num(chunk, &mut p) as u32,
                    fast_num(chunk, &mut p) as u32,
                    fast_num(chunk, &mut p) as u32,
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
    // Queries go in order of their source's new id, so that each thread's share is one part of the map: its searches
    // keep touching the same few regions, of `dist` and of the graph, instead of the whole of both.
    let mut order: Vec<usize> = (0..queries.len()).collect();
    order.sort_unstable_by_key(|&i| new_id[queries[i].0]);
    let sorted = par(
        queries.len(),
        || (vec![0u32; n], Vec::new(), Vec::new(), Radix::new()),
        |(dist, wide, touched, heap), j| {
            let (s, t) = queries[order[j]];
            if s == t {
                0
            } else if !has_in[t].load(Relaxed) {
                -1
            } else {
                // Four-byte distances halve the arrays the threads keep resident. The few searches that pass 2^32
                // (they cross the edges of near-maximal weight) run again with eight-byte ones.
                let (s, t) = (new_id[s], new_id[t]);
                search(&first, &adj, s, t, dist, touched, heap).unwrap_or_else(|| {
                    if wide.is_empty() {
                        *wide = vec![0u64; n];
                    }
                    search(&first, &adj, s, t, wide, touched, heap).unwrap()
                })
            }
        },
    );

    let mut answers = vec![0; queries.len()];
    order.iter().zip(sorted).for_each(|(&i, a)| answers[i] = a);
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

/// Dijkstra from `s`, stopping when `t` is popped; -1 if unreachable, None if a distance does not fit in `D`.
/// `dist` holds distance + 1, with 0 meaning unseen: it is all zero on entry and restored on exit via `touched`, so
/// a query costs only its own search, and the zero-filled array is mapped lazily, so untouched pages cost no memory.
fn search<D: Copy + Default + Into<u64> + TryFrom<u64>>(
    start: &[u32],
    adj: &[AtomicU64],
    s: u32,
    t: u32,
    dist: &mut [D],
    touched: &mut Vec<u32>,
    heap: &mut Radix,
) -> Option<i64> {
    dist[s as usize] = D::try_from(1).ok()?;
    touched.push(s);
    heap.push(0, s);
    let mut ans = Some(-1);
    'search: while let Some((d, u)) = heap.pop() {
        if d + 1 > dist[u as usize].into() {
            continue;
        }
        if u == t {
            ans = Some(d as i64);
            break;
        }
        for e in &adj[start[u as usize] as usize..start[u as usize + 1] as usize] {
            let x = e.load(Relaxed);
            let (v, nd) = ((x >> 32) as usize, d + (x & 0xFFFF_FFFF));
            let seen: u64 = dist[v].into();
            if seen == 0 || nd + 1 < seen {
                let Ok(stored) = D::try_from(nd + 1) else {
                    ans = None;
                    break 'search;
                };
                if seen == 0 {
                    touched.push(v as u32);
                }
                dist[v] = stored;
                heap.push(nd, v as u32);
            }
        }
    }
    heap.clear();
    for v in touched.drain(..) {
        dist[v as usize] = D::default();
    }
    ans
}
