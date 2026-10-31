use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const REGION: usize = 1 << 15;
const WINDOW: u64 = 1 << 14;

fn map(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    unsafe { std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len) }
}

#[inline(always)]
unsafe fn prefetch<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    _mm_prefetch(p as *const i8, _MM_HINT_T0);
}

struct Dial {
    ring: Vec<Vec<u32>>,
    at: u64,
    len: usize,
    far: BinaryHeap<Reverse<(u64, u32)>>,
    far_min: u64,
}

impl Dial {
    fn new() -> Self {
        Dial { ring: vec![Vec::new(); WINDOW as usize], at: 0, len: 0, far: BinaryHeap::new(), far_min: u64::MAX }
    }
    #[inline(always)]
    fn push(&mut self, key: u64, node: u32) {
        if key < self.at + WINDOW {
            unsafe { self.ring.get_unchecked_mut((key % WINDOW) as usize) }.push(node);
            self.len += 1;
        } else {
            self.far.push(Reverse((key, node)));
            self.far_min = self.far_min.min(key);
        }
    }
    #[inline(always)]
    fn pop(&mut self) -> Option<(u64, u32)> {
        loop {
            if let Some(node) = unsafe { self.ring.get_unchecked_mut((self.at % WINDOW) as usize) }.pop() {
                self.len -= 1;
                return Some((self.at, node));
            }
            if self.far_min == self.at {
                let popped = self.far.pop().map(|r| r.0);
                self.far_min = self.far.peek().map_or(u64::MAX, |r| r.0 .0);
                return popped;
            }
            self.at = if self.len > 0 { self.at + 1 } else { Some(self.far_min).filter(|&k| k < u64::MAX)? };
        }
    }
    fn clear(&mut self) {
        if self.len > 0 { self.ring.iter_mut().for_each(Vec::clear); }
        (self.at, self.len, self.far_min) = (0, 0, u64::MAX);
        self.far.clear();
    }
}

#[inline(always)]
fn fast_num(b: &[u8], p: &mut usize) -> u64 {
    let raw = b.get(*p..*p + 8).map(|w| u64::from_le_bytes(w.try_into().unwrap()));
    let len = raw.map_or(8, |r| (r.wrapping_sub(0x3030303030303030) & 0x8080808080808080).trailing_zeros() / 8);
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

fn num(b: &[u8], p: &mut usize) -> u64 {
    while b[*p] < b'0' { *p += 1; }
    let mut x = 0u64;
    while *p < b.len() && b[*p] >= b'0' {
        x = x * 10 + (b[*p] - b'0') as u64;
        *p += 1;
    }
    x
}

fn par<S, R: Send>(jobs: usize, init: impl Fn() -> S + Sync, f: impl Fn(&mut S, usize) -> R + Sync) -> Vec<R> {
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
                            if j >= share(t + 1) { break; }
                            got.push((j, f(&mut state, j)));
                        }
                    }
                    got
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    all.sort_unstable_by_key(|r| r.0);
    all.into_iter().map(|r| r.1).collect()
}

/// Zero-copy transmute of Vec<AtomicU64> to Vec<u64>. Safe because AtomicU64 has the same size/alignment as u64 and
/// the caller guarantees exclusive ownership (all threads have joined).
unsafe fn vec_from_atomic(v: Vec<AtomicU64>) -> Vec<u64> {
    let mut v = std::mem::ManuallyDrop::new(v);
    Vec::from_raw_parts(v.as_mut_ptr() as *mut u64, v.len(), v.capacity())
}

fn main() {
    let g = map("/data/graph.txt");
    let mut p = 0;
    let n = num(g, &mut p) as usize;
    num(g, &mut p);
    let (body, len) = (p + 1, g.len());
    let cut = |i: usize| match i {
        0 => body,
        CHUNKS => len,
        _ => { let at = body + (len - body) * i / CHUNKS - 1; at + g[at..].iter().position(|&c| c == b'\n').unwrap() + 1 }
    };
    let degree: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let has_in: Vec<AtomicBool> = (0..n).map(|_| AtomicBool::new(false)).collect();
    let parsed = par(CHUNKS, || (), |_, c| {
        let chunk = &g[cut(c)..cut(c + 1)];
        let (mut p, mut edges) = (0, Vec::with_capacity(chunk.len() / 16 + 16));
        while p < chunk.len() {
            let (u, v, w) = (fast_num(chunk, &mut p) as u32, fast_num(chunk, &mut p) as u32, fast_num(chunk, &mut p) as u32);
            if u != v {
                edges.push((u, v, w));
                degree[u as usize].fetch_add(1, Relaxed);
                has_in[v as usize].store(true, Relaxed);
            }
        }
        edges
    });
    let mut start = vec![0u32; n + 1];
    for i in 0..n { start[i + 1] = start[i] + degree[i].swap(start[i], Relaxed); }
    let adj0: Vec<AtomicU64> = (0..start[n]).map(|_| AtomicU64::new(0)).collect();
    par(CHUNKS, || (), |_, c| {
        for &(u, v, w) in &parsed[c] {
            adj0[degree[u as usize].fetch_add(1, Relaxed) as usize].store((v as u64) << 32 | w as u64, Relaxed);
        }
    });
    drop((parsed, degree));

    // Renumber nodes for cache locality via BFS regions.
    let claimed: Vec<AtomicBool> = (0..n).map(|_| AtomicBool::new(false)).collect();
    let scan = AtomicUsize::new(0);
    let regions = par(THREADS, || (), |_, _| {
        let mut regions = Vec::new();
        loop {
            let from = scan.fetch_add(1024, Relaxed);
            if from >= n { return regions; }
            for root in from..(from + 1024).min(n) {
                if claimed[root].swap(true, Relaxed) { continue; }
                let (mut region, mut head) = (vec![root as u32], 0);
                while head < region.len() && region.len() < REGION {
                    let u = region[head] as usize;
                    head += 1;
                    for e in &adj0[start[u] as usize..start[u + 1] as usize] {
                        let v = (e.load(Relaxed) >> 32) as usize;
                        if !claimed[v].load(Relaxed) && !claimed[v].swap(true, Relaxed) { region.push(v as u32); }
                    }
                }
                regions.push(region);
            }
        }
    });
    let regions: Vec<Vec<u32>> = regions.into_iter().flatten().collect();
    let mut offset = vec![0; regions.len() + 1];
    for (r, region) in regions.iter().enumerate() { offset[r + 1] = offset[r] + region.len(); }
    let atomics = || (0..n + 1).map(|_| AtomicU32::new(0)).collect::<Vec<_>>();
    let (order, new_id, deg) = (atomics(), atomics(), atomics());
    par(regions.len(), || (), |_, r| {
        for (k, &u) in regions[r].iter().enumerate() {
            let i = offset[r] + k;
            order[i].store(u, Relaxed);
            new_id[u as usize].store(i as u32, Relaxed);
            deg[i + 1].store(start[u as usize + 1] - start[u as usize], Relaxed);
        }
    });
    let plain = |a: Vec<AtomicU32>| a.into_iter().map(AtomicU32::into_inner).collect::<Vec<u32>>();
    let (order, new_id, mut first) = (plain(order), plain(new_id), plain(deg));
    for i in 0..n { first[i + 1] += first[i]; }
    // Rebuild CSR in the new numbering.
    let adj: Vec<AtomicU64> = (0..first[n]).map(|_| AtomicU64::new(0)).collect();
    let block = n.div_ceil(CHUNKS);
    par(CHUNKS, || (), |_, c| {
        for i in c * block..((c + 1) * block).min(n) {
            let u = order[i] as usize;
            for (k, e) in adj0[start[u] as usize..start[u + 1] as usize].iter().enumerate() {
                let x = e.load(Relaxed);
                adj[first[i] as usize + k].store((new_id[(x >> 32) as usize] as u64) << 32 | x & 0xFFFF_FFFF, Relaxed);
            }
        }
    });
    drop(adj0);
    // Zero-copy convert to plain u64 slice; all threads have joined, no more atomic access needed.
    let adj: Vec<u64> = unsafe { vec_from_atomic(adj) };

    let q = map("/data/queries.txt");
    let mut p = 0;
    let queries: Vec<_> = (0..q.iter().filter(|&&c| c == b'\n').count())
        .map(|_| (num(q, &mut p) as usize, num(q, &mut p) as usize))
        .collect();
    let mut order: Vec<usize> = (0..queries.len()).collect();
    order.sort_unstable_by_key(|&i| new_id[queries[i].0]);
    let sorted = par(
        queries.len(),
        || (vec![0u32; n], Vec::new(), Vec::new(), Dial::new()),
        |(dist, wide, touched, heap), j| {
            let (s, t) = queries[order[j]];
            if s == t { return 0; }
            if !has_in[t].load(Relaxed) { return -1; }
            let (s, t) = (new_id[s], new_id[t]);
            search(&first, &adj, s, t, dist, touched, heap).unwrap_or_else(|| {
                if wide.is_empty() { *wide = vec![0u64; n]; }
                search(&first, &adj, s, t, wide, touched, heap).unwrap()
            })
        },
    );

    let mut answers = vec![0i64; queries.len()];
    order.iter().zip(sorted).for_each(|(&i, a)| answers[i] = a);
    let mut out = Vec::with_capacity(answers.len() * 8);
    for a in answers {
        if a < 0 {
            out.extend_from_slice(b"-1\n");
        } else {
            let mut buf = [b'\n'; 20];
            let (mut i, mut x) = (19, a as u64);
            loop { i -= 1; buf[i] = b'0' + (x % 10) as u8; x /= 10; if x == 0 { break; } }
            out.extend_from_slice(&buf[i..]);
        }
    }
    std::fs::write("/out/distances.txt", out).expect("write output");
}

/// Dijkstra from s stopping when t is popped; -1 if unreachable, None if distance overflows D.
/// dist holds distance+1 (0 = unseen), restored to zero via touched. Hot loop uses get_unchecked and software
/// prefetch to eliminate bounds checks and hide memory latency on random dist[v] reads.
#[inline(never)]
fn search<D: Copy + Default + Into<u64> + TryFrom<u64>>(
    start: &[u32],
    adj: &[u64],
    s: u32,
    t: u32,
    dist: &mut [D],
    touched: &mut Vec<u32>,
    heap: &mut Dial,
) -> Option<i64> {
    dist[s as usize] = D::try_from(1).ok()?;
    touched.push(s);
    heap.push(0, s);
    let mut ans = Some(-1);
    'search: while let Some((d, u)) = heap.pop() {
        unsafe {
            if d + 1 > (*dist.get_unchecked(u as usize)).into() { continue; }
            if u == t { ans = Some(d as i64); break; }
            let es = *start.get_unchecked(u as usize) as usize;
            let ee = *start.get_unchecked(u as usize + 1) as usize;
            // Batch-prefetch the first few neighbors' distance entries to hide L3 latency.
            let pre = (ee - es).min(4);
            for pi in 0..pre {
                prefetch(dist.as_ptr().add((*adj.get_unchecked(es + pi) >> 32) as usize));
            }
            let mut ei = es;
            while ei < ee {
                let x = *adj.get_unchecked(ei);
                let v = (x >> 32) as usize;
                let nd = d + (x & 0xFFFF_FFFF);
                // Prefetch the neighbor 4 edges ahead while processing this one.
                if ei + pre < ee {
                    prefetch(dist.as_ptr().add((*adj.get_unchecked(ei + pre) >> 32) as usize));
                }
                let seen: u64 = (*dist.get_unchecked(v)).into();
                // Bitwise OR: both conditions evaluated, single branch to predict.
                if (seen == 0) | (nd + 1 < seen) {
                    let Ok(stored) = D::try_from(nd + 1) else { ans = None; break 'search; };
                    if seen == 0 { touched.push(v as u32); }
                    *dist.get_unchecked_mut(v) = stored;
                    heap.push(nd, v as u32);
                }
                ei += 1;
            }
        }
    }
    heap.clear();
    for v in touched.drain(..) { unsafe { *dist.get_unchecked_mut(v as usize) = D::default(); } }
    ans
}
