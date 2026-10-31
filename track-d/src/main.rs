use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const REGION: usize = 1 << 15;
const WINDOW: usize = 1 << 14;

#[cfg(unix)]
fn map(path: &str) -> &'static [u8] {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    }
    let f = std::fs::File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    unsafe {
        std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len)
    }
}

#[cfg(not(unix))]
fn map(path: &str) -> &'static [u8] {
    let bytes = std::fs::read(path).expect("read file");
    Box::leak(bytes.into_boxed_slice())
}

#[inline(always)]
unsafe fn prefetch<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    _mm_prefetch(p as *const i8, _MM_HINT_T0);
}

fn num(b: &[u8], p: &mut usize) -> u64 {
    while *p < b.len() && unsafe { *b.get_unchecked(*p) } < b'0' {
        *p += 1;
    }
    let mut x = 0u64;
    while *p < b.len() && unsafe { *b.get_unchecked(*p) } >= b'0' {
        x = x * 10 + (unsafe { *b.get_unchecked(*p) } - b'0') as u64;
        *p += 1;
    }
    x
}

#[inline(always)]
unsafe fn parse_fast_u32(b: &[u8], p: &mut usize) -> u32 {
    if *p + 8 <= b.len() {
        let raw = (b.as_ptr().add(*p) as *const u64).read_unaligned();
        let len = ((raw.wrapping_sub(0x3030303030303030) & 0x8080808080808080).trailing_zeros() / 8)
            as usize;
        if len < 8 {
            let x = raw << (8 * (8 - len));
            let x = ((x & 0x0F0F0F0F0F0F0F0F).wrapping_mul(2561) >> 8) & 0x00FF00FF00FF00FF;
            let x = (x.wrapping_mul(6553601) >> 16) & 0x0000FFFF0000FFFF;
            *p += len + 1;
            return (x.wrapping_mul(42949672960001) >> 32) as u32;
        }
    }
    let x = num(b, p) as u32;
    *p += 1;
    x
}

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

struct PoolDial {
    head: [u32; WINDOW],
    bitmap: [u64; WINDOW / 64],
    summary: [u64; 4],
    pool_node: Vec<u32>,
    pool_next: Vec<u32>,
    free_head: u32,
    at: u32,
    len: usize,
    far: BinaryHeap<Reverse<(u32, u32)>>,
    far_min: u32,
    touched_buckets: Vec<u16>,
}

impl PoolDial {
    fn new() -> Self {
        PoolDial {
            head: [u32::MAX; WINDOW],
            bitmap: [0u64; WINDOW / 64],
            summary: [0u64; 4],
            pool_node: Vec::with_capacity(16384),
            pool_next: Vec::with_capacity(16384),
            free_head: u32::MAX,
            at: 0,
            len: 0,
            far: BinaryHeap::new(),
            far_min: u32::MAX,
            touched_buckets: Vec::with_capacity(2048),
        }
    }

    #[inline(always)]
    fn alloc_slot(&mut self, node: u32, next: u32) -> u32 {
        if self.free_head != u32::MAX {
            let slot = self.free_head as usize;
            unsafe {
                self.free_head = *self.pool_next.get_unchecked(slot);
                *self.pool_node.get_unchecked_mut(slot) = node;
                *self.pool_next.get_unchecked_mut(slot) = next;
            }
            slot as u32
        } else {
            let slot = self.pool_node.len() as u32;
            self.pool_node.push(node);
            self.pool_next.push(next);
            slot
        }
    }

    #[inline(always)]
    fn push(&mut self, key: u32, node: u32) {
        if key < self.at + WINDOW as u32 {
            let b = (key as usize) & (WINDOW - 1);
            let old_h = unsafe { *self.head.get_unchecked(b) };
            if old_h == u32::MAX {
                let w = b >> 6;
                unsafe {
                    *self.bitmap.get_unchecked_mut(w) |= 1 << (b & 63);
                    *self.summary.get_unchecked_mut(w >> 6) |= 1 << (w & 63);
                }
                self.touched_buckets.push(b as u16);
            }
            let new_h = self.alloc_slot(node, old_h);
            unsafe {
                *self.head.get_unchecked_mut(b) = new_h;
            }
            self.len += 1;
        } else {
            self.far.push(Reverse((key, node)));
            self.far_min = self.far_min.min(key);
        }
    }

    #[inline(always)]
    fn pop(&mut self) -> Option<(u32, u32)> {
        loop {
            let b = (self.at as usize) & (WINDOW - 1);
            let h = unsafe { *self.head.get_unchecked(b) };
            if h != u32::MAX {
                self.len -= 1;
                let (node, nxt) = unsafe {
                    (
                        *self.pool_node.get_unchecked(h as usize),
                        *self.pool_next.get_unchecked(h as usize),
                    )
                };
                unsafe {
                    *self.head.get_unchecked_mut(b) = nxt;
                    *self.pool_next.get_unchecked_mut(h as usize) = self.free_head;
                }
                self.free_head = h;
                if nxt == u32::MAX {
                    let w = b >> 6;
                    unsafe {
                        let bit = *self.bitmap.get_unchecked(w) & !(1 << (b & 63));
                        *self.bitmap.get_unchecked_mut(w) = bit;
                        if bit == 0 {
                            *self.summary.get_unchecked_mut(w >> 6) &= !(1 << (w & 63));
                        }
                    }
                }
                return Some((self.at, node));
            }

            if self.far_min == self.at {
                let popped = self.far.pop().map(|r| r.0);
                self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                return popped;
            }

            if self.len == 0 {
                return if self.far_min < u32::MAX {
                    self.at = self.far_min;
                    let popped = self.far.pop().map(|r| r.0);
                    self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                    popped
                } else {
                    None
                };
            }

            let start_b = (self.at as usize) & (WINDOW - 1);
            let word_idx = start_b >> 6;
            let cur_word = unsafe { *self.bitmap.get_unchecked(word_idx) } >> (start_b & 63);
            if cur_word != 0 {
                self.at += cur_word.trailing_zeros();
                continue;
            }

            let sum_idx = word_idx >> 6;
            let sum_bit = ((word_idx + 1) & 63) as u32;
            let cur_sum = (unsafe { *self.summary.get_unchecked(sum_idx) } >> sum_bit) << sum_bit;

            let next_word = if cur_sum != 0 {
                (sum_idx << 6) | (cur_sum.trailing_zeros() as usize)
            } else {
                let mut found = None;
                for step in 1..4 {
                    let s_idx = (sum_idx + step) & 3;
                    let s_val = unsafe { *self.summary.get_unchecked(s_idx) };
                    if s_val != 0 {
                        found = Some((s_idx << 6) | (s_val.trailing_zeros() as usize));
                        break;
                    }
                }
                match found {
                    Some(w) => w,
                    None => {
                        if self.far_min < u32::MAX {
                            self.at = self.far_min;
                            let popped = self.far.pop().map(|r| r.0);
                            self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                            return popped;
                        }
                        return None;
                    }
                }
            };

            let next_b = (next_word << 6)
                | (unsafe { *self.bitmap.get_unchecked(next_word) }.trailing_zeros() as usize);
            self.at += if next_b >= start_b {
                next_b - start_b
            } else {
                WINDOW - start_b + next_b
            } as u32;
        }
    }

    #[inline(always)]
    fn clear(&mut self) {
        for &b in &self.touched_buckets {
            let idx = b as usize;
            unsafe {
                *self.head.get_unchecked_mut(idx) = u32::MAX;
                *self.bitmap.get_unchecked_mut(idx >> 6) = 0;
                *self.summary.get_unchecked_mut(idx >> 12) = 0;
            }
        }
        self.touched_buckets.clear();
        (self.at, self.len, self.far_min, self.free_head) = (0, 0, u32::MAX, u32::MAX);
        self.pool_node.clear();
        self.pool_next.clear();
        self.far.clear();
    }
}

#[inline(never)]
#[cold]
fn lookup_overflow(overflow: &[(u32, u64)], edge_idx: u32) -> u64 {
    overflow
        .binary_search_by_key(&edge_idx, |&(idx, _)| idx)
        .map_or(2047, |p| overflow[p].1)
}

fn search_fast(
    start: &[u32],
    adj: &[u32],
    overflow: &[(u32, u64)],
    s: u32,
    t: u32,
    dist: &mut [u32],
    wide: &mut Vec<u64>,
    touched: &mut Vec<u32>,
    heap: &mut PoolDial,
) -> i64 {
    unsafe {
        *dist.get_unchecked_mut(s as usize) = 0;
    }
    touched.push(s);
    heap.push(0, s);
    let mut ans = -1i64;
    let mut needs_wide = false;

    'search: while let Some((d, u)) = heap.pop() {
        unsafe {
            if d > *dist.get_unchecked(u as usize) {
                continue;
            }
            if u == t {
                ans = d as i64;
                break;
            }
            let es = *start.get_unchecked(u as usize) as usize;
            let ee = *start.get_unchecked(u as usize + 1) as usize;

            for ei in es..ee {
                let entry = *adj.get_unchecked(ei);
                let v = (entry >> 11) as usize;
                let mut w = (entry & 0x7FF) as u64;

                if w == 0x7FF {
                    w = lookup_overflow(overflow, ei as u32);
                    if w > 1_000_000 {
                        needs_wide = true;
                        break 'search;
                    }
                }
                if ei + 1 < ee {
                    prefetch(
                        dist.as_ptr()
                            .add((*adj.get_unchecked(ei + 1) >> 11) as usize),
                    );
                }

                let nd = d + w as u32;
                let cur_d = *dist.get_unchecked(v);
                if nd < cur_d {
                    if cur_d == u32::MAX {
                        touched.push(v as u32);
                    }
                    *dist.get_unchecked_mut(v) = nd;
                    heap.push(nd, v as u32);
                }
            }
        }
    }

    heap.clear();
    for v in touched.drain(..) {
        unsafe {
            *dist.get_unchecked_mut(v as usize) = u32::MAX;
        }
    }
    if needs_wide {
        if wide.is_empty() {
            *wide = vec![u64::MAX; dist.len()];
        }
        return search_wide(start, adj, overflow, s, t, wide, touched);
    }
    ans
}

fn search_wide(
    start: &[u32],
    adj: &[u32],
    overflow: &[(u32, u64)],
    s: u32,
    t: u32,
    dist: &mut [u64],
    touched: &mut Vec<u32>,
) -> i64 {
    let mut heap = BinaryHeap::new();
    unsafe {
        *dist.get_unchecked_mut(s as usize) = 0;
    }
    touched.push(s);
    heap.push(Reverse((0u64, s)));
    let mut ans = -1i64;

    while let Some(Reverse((d, u))) = heap.pop() {
        unsafe {
            if d > *dist.get_unchecked(u as usize) {
                continue;
            }
            if u == t {
                ans = d as i64;
                break;
            }
            let es = *start.get_unchecked(u as usize) as usize;
            let ee = *start.get_unchecked(u as usize + 1) as usize;

            for ei in es..ee {
                let entry = *adj.get_unchecked(ei);
                let v = (entry >> 11) as usize;
                let mut w = (entry & 0x7FF) as u64;
                if w == 0x7FF {
                    w = lookup_overflow(overflow, ei as u32);
                }
                let nd = d + w;
                let cur_d = *dist.get_unchecked(v);
                if nd < cur_d {
                    if cur_d == u64::MAX {
                        touched.push(v as u32);
                    }
                    *dist.get_unchecked_mut(v) = nd;
                    heap.push(Reverse((nd, v as u32)));
                }
            }
        }
    }

    for v in touched.drain(..) {
        unsafe {
            *dist.get_unchecked_mut(v as usize) = u64::MAX;
        }
    }
    ans
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
        _ => {
            let at = body + (len - body) * i / CHUNKS - 1;
            at + g[at..].iter().position(|&c| c == b'\n').unwrap() + 1
        }
    };

    let degree: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let has_in: Vec<AtomicBool> = (0..n).map(|_| AtomicBool::new(false)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            let chunk = &g[cut(c)..cut(c + 1)];
            let mut p = 0;
            while p < chunk.len() {
                unsafe {
                    let u = parse_fast_u32(chunk, &mut p);
                    let v = parse_fast_u32(chunk, &mut p);
                    let _w = parse_fast_u32(chunk, &mut p);
                    if u != v {
                        degree[u as usize].fetch_add(1, Relaxed);
                        has_in[v as usize].store(true, Relaxed);
                    }
                }
            }
        },
    );

    let mut start0 = vec![0u32; n + 1];
    for i in 0..n {
        start0[i + 1] = start0[i] + degree[i].swap(start0[i], Relaxed);
    }
    let m = start0[n] as usize;

    let edges0: Vec<AtomicU64> = (0..m).map(|_| AtomicU64::new(0)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            let chunk = &g[cut(c)..cut(c + 1)];
            let mut p = 0;
            while p < chunk.len() {
                unsafe {
                    let u = parse_fast_u32(chunk, &mut p);
                    let v = parse_fast_u32(chunk, &mut p);
                    let w = parse_fast_u32(chunk, &mut p) as u64;
                    if u != v {
                        let slot = degree[u as usize].fetch_add(1, Relaxed) as usize;
                        edges0[slot].store(((v as u64) << 32) | (w & 0xFFFF_FFFF), Relaxed);
                    }
                }
            }
        },
    );

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
                        let s = start0[u] as usize;
                        let e = start0[u + 1] as usize;
                        for idx in s..e {
                            let edge = edges0[idx].load(Relaxed);
                            let v = (edge >> 32) as usize;
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

    let regions: Vec<Vec<u32>> = regions.into_iter().flatten().collect();
    let mut offset = vec![0; regions.len() + 1];
    for (r, region) in regions.iter().enumerate() {
        offset[r + 1] = offset[r] + region.len();
    }

    let new_id: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let new_deg: Vec<AtomicU32> = (0..n + 1).map(|_| AtomicU32::new(0)).collect();
    par(
        regions.len(),
        || (),
        |_, r| {
            for (k, &u) in regions[r].iter().enumerate() {
                let i = offset[r] + k;
                new_id[u as usize].store(i as u32, Relaxed);
                new_deg[i + 1].store(start0[u as usize + 1] - start0[u as usize], Relaxed);
            }
        },
    );
    drop(regions);

    let new_id = new_id
        .into_iter()
        .map(AtomicU32::into_inner)
        .collect::<Vec<u32>>();
    let mut first = new_deg
        .into_iter()
        .map(AtomicU32::into_inner)
        .collect::<Vec<u32>>();
    for i in 0..n {
        first[i + 1] += first[i];
    }

    let adj: Vec<AtomicU32> = (0..m).map(|_| AtomicU32::new(0)).collect();
    let overflows = par(
        THREADS,
        || Vec::new(),
        |local_overflow, t| {
            let u_start = t * n / THREADS;
            let u_end = (t + 1) * n / THREADS;
            for u in u_start..u_end {
                let nu = unsafe { *new_id.get_unchecked(u) } as usize;
                let es = unsafe { *start0.get_unchecked(u) } as usize;
                let ee = unsafe { *start0.get_unchecked(u + 1) } as usize;
                let mut out_idx = unsafe { *first.get_unchecked(nu) } as usize;

                for edge_idx in es..ee {
                    let edge = edges0[edge_idx].load(Relaxed);
                    let v = (edge >> 32) as usize;
                    let w = (edge & 0xFFFF_FFFF) as u64;
                    let nv = unsafe { *new_id.get_unchecked(v) };

                    if w < 2047 {
                        adj[out_idx].store((nv << 11) | (w as u32), Relaxed);
                    } else {
                        adj[out_idx].store((nv << 11) | 0x7FF, Relaxed);
                        local_overflow.push((out_idx as u32, w));
                    }
                    out_idx += 1;
                }
            }
            std::mem::take(local_overflow)
        },
    );

    drop(edges0);
    drop(start0);
    let mut overflow: Vec<(u32, u64)> = overflows.into_iter().flatten().collect();
    overflow.sort_unstable_by_key(|&(idx, _)| idx);
    let adj: Vec<u32> = adj.into_iter().map(AtomicU32::into_inner).collect();

    let q = map("/data/queries.txt");
    let mut p = 0;
    let queries: Vec<_> = (0..q.iter().filter(|&&c| c == b'\n').count())
        .map(|_| (num(q, &mut p) as usize, num(q, &mut p) as usize))
        .collect();

    let mut order: Vec<usize> = (0..queries.len()).collect();
    order.sort_unstable_by_key(|&i| new_id[queries[i].0]);

    let sorted = par(
        queries.len(),
        || (vec![u32::MAX; n], Vec::new(), Vec::new(), PoolDial::new()),
        |(dist, wide, touched, heap), j| {
            let (s, t) = queries[order[j]];
            if s == t {
                return 0;
            }
            if !has_in[t].load(Relaxed) {
                return -1;
            }
            let (s_new, t_new) = (new_id[s], new_id[t]);
            if first[s_new as usize + 1] == first[s_new as usize] {
                return -1;
            }
            search_fast(
                &first, &adj, &overflow, s_new, t_new, dist, wide, touched, heap,
            )
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
