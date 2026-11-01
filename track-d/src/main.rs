use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    __m256i, _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_set1_epi8,
    _mm_prefetch, _MM_HINT_T0,
};

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const REGION: usize = 1 << 15;
const WINDOW: usize = 1 << 12;

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
unsafe fn parse_swar_exact(p: *const u8, len: usize) -> u32 {
    let raw = (p as *const u64).read_unaligned();
    let x = raw << (8 * (8 - len));
    let x = ((x & 0x0F0F0F0F0F0F0F0F).wrapping_mul(2561) >> 8) & 0x00FF00FF00FF00FF;
    let x = (x.wrapping_mul(6553601) >> 16) & 0x0000FFFF0000FFFF;
    (x.wrapping_mul(42949672960001) >> 32) as u32
}

#[inline(always)]
unsafe fn parse_line(chunk: &[u8], p: &mut usize) -> (u32, u32, u32) {
    while *p < chunk.len() && *chunk.get_unchecked(*p) < b'0' {
        *p += 1;
    }
    #[cfg(target_arch = "x86_64")]
    if *p + 32 <= chunk.len() {
        let vchunk = _mm256_loadu_si256(chunk.as_ptr().add(*p) as *const __m256i);
        let mask = _mm256_movemask_epi8(_mm256_cmpgt_epi8(_mm256_set1_epi8(0x30), vchunk)) as u32;
        let p0 = mask.trailing_zeros() as usize;
        let m1 = mask & (mask - 1);
        let p1 = m1.trailing_zeros() as usize;
        let p2 = (m1 & (m1 - 1)).trailing_zeros() as usize;
        if p2 < 32
            && p0 < 8
            && (p1 - p0 - 1) < 8
            && (p2 - p1 - 1) < 8
            && *chunk.get_unchecked(*p + p0) == b' '
            && *chunk.get_unchecked(*p + p1) == b' '
            && *chunk.get_unchecked(*p + p2) == b'\n'
        {
            let base = chunk.as_ptr().add(*p);
            let u = parse_swar_exact(base, p0);
            let v = parse_swar_exact(base.add(p0 + 1), p1 - p0 - 1);
            let w = parse_swar_exact(base.add(p1 + 1), p2 - p1 - 1);
            *p += p2 + 1;
            return (u, v, w);
        }
    }
    let u = num(chunk, p) as u32;
    *p += 1;
    let v = num(chunk, p) as u32;
    *p += 1;
    let w = num(chunk, p) as u32;
    *p += 1;
    (u, v, w)
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

#[repr(align(64))]
struct PoolDial {
    head: [u32; WINDOW],
    bitmap: [u64; WINDOW / 64],
    summary: u64,
    pool: Vec<u64>,
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
            summary: 0u64,
            pool: Vec::with_capacity(8192),
            free_head: u32::MAX,
            at: 0,
            len: 0,
            far: BinaryHeap::new(),
            far_min: u32::MAX,
            touched_buckets: Vec::with_capacity(1024),
        }
    }

    #[inline(always)]
    fn alloc_slot(&mut self, node: u32, next: u32) -> u32 {
        let packed = (node as u64) | ((next as u64) << 32);
        if self.free_head != u32::MAX {
            let slot = self.free_head as usize;
            unsafe {
                let old_entry = *self.pool.get_unchecked(slot);
                self.free_head = (old_entry >> 32) as u32;
                *self.pool.get_unchecked_mut(slot) = packed;
            }
            slot as u32
        } else {
            let slot = self.pool.len() as u32;
            self.pool.push(packed);
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
                    *self.bitmap.get_unchecked_mut(w) |= 1u64 << (b & 63);
                }
                self.summary |= 1u64 << w;
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
                let entry = unsafe { *self.pool.get_unchecked(h as usize) };
                let node = entry as u32;
                let nxt = (entry >> 32) as u32;
                unsafe {
                    *self.head.get_unchecked_mut(b) = nxt;
                    *self.pool.get_unchecked_mut(h as usize) = (self.free_head as u64) << 32;
                }
                self.free_head = h;
                if nxt == u32::MAX {
                    let w = b >> 6;
                    unsafe {
                        let bit = *self.bitmap.get_unchecked(w) & !(1u64 << (b & 63));
                        *self.bitmap.get_unchecked_mut(w) = bit;
                        if bit == 0 {
                            self.summary &= !(1u64 << w);
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

            let next_shift = ((word_idx + 1) & 63) as u32;
            let rot = self.summary.rotate_right(next_shift);
            if rot != 0 {
                let delta_words = rot.trailing_zeros() as usize + 1;
                let next_word = (word_idx + delta_words) & 63;
                let next_b = (next_word << 6)
                    | (unsafe { *self.bitmap.get_unchecked(next_word) }.trailing_zeros() as usize);
                self.at += (next_b.wrapping_sub(start_b) & (WINDOW - 1)) as u32;
                continue;
            }

            if self.far_min < u32::MAX {
                self.at = self.far_min;
                let popped = self.far.pop().map(|r| r.0);
                self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                return popped;
            }
            return None;
        }
    }

    #[inline(always)]
    fn clear(&mut self) {
        for &b in &self.touched_buckets {
            let idx = b as usize;
            unsafe {
                *self.head.get_unchecked_mut(idx) = u32::MAX;
                *self.bitmap.get_unchecked_mut(idx >> 6) = 0;
            }
        }
        self.touched_buckets.clear();
        self.summary = 0;
        (self.at, self.len, self.far_min, self.free_head) = (0, 0, u32::MAX, u32::MAX);
        self.pool.clear();
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
        *dist.get_unchecked_mut(s as usize) = !0;
    }
    touched.push(s);
    heap.push(0, s);
    let mut ans = -1i64;
    let mut needs_wide = false;

    'search: while let Some((d, u)) = heap.pop() {
        unsafe {
            if !d < *dist.get_unchecked(u as usize) {
                continue;
            }
            if u == t {
                ans = d as i64;
                break;
            }
            let es = *start.get_unchecked(u as usize) as usize;
            let ee = *start.get_unchecked(u as usize + 1) as usize;

            for k in es..ee {
                prefetch(dist.as_ptr().add((*adj.get_unchecked(k) >> 11) as usize));
            }

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

                let nd = d + w as u32;
                let inv_nd = !nd;
                let cur_inv = *dist.get_unchecked(v);
                if inv_nd > cur_inv {
                    if cur_inv == 0 {
                        touched.push(v as u32);
                    }
                    *dist.get_unchecked_mut(v) = inv_nd;
                    heap.push(nd, v as u32);
                }
            }
        }
    }

    heap.clear();
    for v in touched.drain(..) {
        unsafe {
            *dist.get_unchecked_mut(v as usize) = 0;
        }
    }
    if needs_wide {
        if wide.is_empty() {
            *wide = vec![0u64; dist.len()];
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
        *dist.get_unchecked_mut(s as usize) = !0u64;
    }
    touched.push(s);
    heap.push(Reverse((0u64, s)));
    let mut ans = -1i64;

    while let Some(Reverse((d, u))) = heap.pop() {
        unsafe {
            if !d < *dist.get_unchecked(u as usize) {
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
                let inv_nd = !nd;
                let cur_inv = *dist.get_unchecked(v);
                if inv_nd > cur_inv {
                    if cur_inv == 0 {
                        touched.push(v as u32);
                    }
                    *dist.get_unchecked_mut(v) = inv_nd;
                    heap.push(Reverse((nd, v as u32)));
                }
            }
        }
    }

    for v in touched.drain(..) {
        unsafe {
            *dist.get_unchecked_mut(v as usize) = 0;
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

    let has_in: Vec<AtomicU64> = (0..(n + 63) / 64).map(|_| AtomicU64::new(0)).collect();
    let degree: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            let chunk = &g[cut(c)..cut(c + 1)];
            let mut p = 0;
            while p < chunk.len() {
                unsafe {
                    let (u, v, _) = parse_line(chunk, &mut p);
                    if u != v {
                        degree[u as usize].fetch_add(1, Relaxed);
                        has_in[(v as usize) >> 6].fetch_or(1u64 << ((v as usize) & 63), Relaxed);
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

    let adj0: Vec<AtomicU32> = (0..m).map(|_| AtomicU32::new(0)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            let chunk = &g[cut(c)..cut(c + 1)];
            let mut p = 0;
            while p < chunk.len() {
                unsafe {
                    let (u, v, _) = parse_line(chunk, &mut p);
                    if u != v {
                        let slot = degree[u as usize].fetch_add(1, Relaxed) as usize;
                        adj0[slot].store(v, Relaxed);
                    }
                }
            }
        },
    );
    drop(degree);

    let claimed: Vec<AtomicU64> = (0..(n + 63) / 64).map(|_| AtomicU64::new(0)).collect();
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
                    let bit = 1u64 << (root & 63);
                    if (claimed[root >> 6].fetch_or(bit, Relaxed) & bit) != 0 {
                        continue;
                    }
                    let (mut region, mut head) = (vec![root as u32], 0);
                    while head < region.len() && region.len() < REGION {
                        let u = region[head] as usize;
                        head += 1;
                        let s = start0[u] as usize;
                        let e = start0[u + 1] as usize;
                        for idx in s..e {
                            let v = adj0[idx].load(Relaxed) as usize;
                            let v_bit = 1u64 << (v & 63);
                            if (claimed[v >> 6].fetch_or(v_bit, Relaxed) & v_bit) == 0 {
                                region.push(v as u32);
                            }
                        }
                    }
                    regions.push(region);
                }
            }
        },
    );
    drop(claimed);

    let regions: Vec<Vec<u32>> = regions.into_iter().flatten().collect();
    let mut offset = vec![0; regions.len() + 1];
    for (r, region) in regions.iter().enumerate() {
        offset[r + 1] = offset[r] + region.len();
    }

    let new_id: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let mut first = vec![0u32; n + 1];
    let first_ptr = first.as_mut_ptr() as usize;
    par(
        regions.len(),
        || (),
        |_, r| {
            let f_ptr = first_ptr as *mut u32;
            for (k, &u) in regions[r].iter().enumerate() {
                let i = offset[r] + k;
                new_id[u as usize].store(i as u32, Relaxed);
                unsafe {
                    *f_ptr.add(i + 1) = start0[u as usize + 1] - start0[u as usize];
                }
            }
        },
    );
    drop(regions);
    drop(adj0);
    drop(start0);

    for i in 0..n {
        first[i + 1] += first[i];
    }
    let new_id: Vec<u32> = unsafe {
        let mut nid = std::mem::ManuallyDrop::new(new_id);
        Vec::from_raw_parts(nid.as_mut_ptr() as *mut u32, nid.len(), nid.capacity())
    };

    let cur_cursor: Vec<AtomicU32> = first[..n].iter().map(|&f| AtomicU32::new(f)).collect();
    let adj: Vec<AtomicU32> = (0..m).map(|_| AtomicU32::new(0)).collect();
    let overflows = par(
        CHUNKS,
        || Vec::new(),
        |local_of, c| {
            let chunk = &g[cut(c)..cut(c + 1)];
            let mut p = 0;
            while p < chunk.len() {
                unsafe {
                    let (u, v, w) = parse_line(chunk, &mut p);
                    if u != v {
                        let nu = *new_id.get_unchecked(u as usize) as usize;
                        let nv = *new_id.get_unchecked(v as usize);
                        let out_idx = cur_cursor[nu].fetch_add(1, Relaxed) as usize;
                        if w < 2047 {
                            adj[out_idx].store((nv << 11) | (w as u32), Relaxed);
                        } else {
                            adj[out_idx].store((nv << 11) | 0x7FF, Relaxed);
                            local_of.push((out_idx as u32, w as u64));
                        }
                    }
                }
            }
            std::mem::take(local_of)
        },
    );
    drop(cur_cursor);

    let mut overflow: Vec<(u32, u64)> = overflows.into_iter().flatten().collect();
    overflow.sort_unstable_by_key(|&(idx, _)| idx);
    let adj: Vec<u32> = unsafe {
        let mut a = std::mem::ManuallyDrop::new(adj);
        Vec::from_raw_parts(a.as_mut_ptr() as *mut u32, a.len(), a.capacity())
    };

    let q = map("/data/queries.txt");
    let mut p = 0;
    let queries: Vec<_> = (0..q.iter().filter(|&&c| c == b'\n').count())
        .map(|_| (num(q, &mut p) as usize, num(q, &mut p) as usize))
        .collect();

    let mut order: Vec<usize> = (0..queries.len()).collect();
    order.sort_unstable_by_key(|&i| new_id[queries[i].0]);

    let mut answers = vec![0i64; queries.len()];
    let ans_ptr = answers.as_mut_ptr() as usize;

    let share = |t: usize| t * queries.len() / THREADS;
    let next: Vec<AtomicUsize> = (0..THREADS).map(|t| AtomicUsize::new(share(t))).collect();
    std::thread::scope(|sc| {
        for me in 0..THREADS {
            let (next, order, queries, has_in, first, adj, overflow, new_id) = (
                &next, &order, &queries, &has_in, &first, &adj, &overflow, &new_id,
            );
            sc.spawn(move || {
                let mut dist = vec![0u32; n];
                let mut wide = Vec::new();
                let mut touched = Vec::new();
                let mut heap = PoolDial::new();
                let ans_out = ans_ptr as *mut i64;

                for t in (me..THREADS).chain(0..me) {
                    loop {
                        let j = next[t].fetch_add(1, Relaxed);
                        if j >= share(t + 1) {
                            break;
                        }
                        let orig_idx = order[j];
                        let (s, t_node) = queries[orig_idx];
                        let ans = if s == t_node {
                            0
                        } else if (has_in[t_node >> 6].load(Relaxed) & (1u64 << (t_node & 63))) == 0
                        {
                            -1
                        } else {
                            let (s_new, t_new) = (new_id[s], new_id[t_node]);
                            if first[s_new as usize + 1] == first[s_new as usize] {
                                -1
                            } else {
                                search_fast(
                                    first,
                                    adj,
                                    overflow,
                                    s_new,
                                    t_new,
                                    &mut dist,
                                    &mut wide,
                                    &mut touched,
                                    &mut heap,
                                )
                            }
                        };
                        unsafe {
                            *ans_out.add(orig_idx) = ans;
                        }
                    }
                }
            });
        }
    });

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
