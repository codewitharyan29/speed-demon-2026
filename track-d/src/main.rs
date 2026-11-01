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
fn map_file(path: &str) -> (&'static [u8], usize, i32) {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8;
    }
    let f = std::fs::File::open(path).expect("open");
    let (len, fd) = (f.metadata().unwrap().len() as usize, f.as_raw_fd());
    std::mem::forget(f);
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, fd, 0) };
    (unsafe { std::slice::from_raw_parts(ptr, len) }, len, fd)
}

#[cfg(unix)]
unsafe fn unmap_file(s: &[u8], len: usize, fd: i32) {
    extern "C" {
        fn munmap(a: *mut u8, l: usize) -> i32;
        fn posix_fadvise(fd: i32, o: i64, l: i64, adv: i32) -> i32;
        fn close(fd: i32) -> i32;
    }
    posix_fadvise(fd, 0, len as i64, 4);
    munmap(s.as_ptr() as *mut u8, len);
    close(fd);
}

#[cfg(not(unix))]
fn map_file(path: &str) -> (&'static [u8], usize, i32) {
    let b = std::fs::read(path).expect("read");
    let len = b.len();
    (Box::leak(b.into_boxed_slice()), len, -1)
}

#[cfg(not(unix))]
unsafe fn unmap_file(_s: &[u8], _l: usize, _f: i32) {}

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
unsafe fn parse_swar(p: *const u8, len: usize) -> u32 {
    let raw = (p as *const u64).read_unaligned();
    let x = (raw << (8 * (8 - len)) & 0x0F0F0F0F0F0F0F0F).wrapping_mul(2561) >> 8
        & 0x00FF00FF00FF00FF;
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
        let mask = _mm256_movemask_epi8(_mm256_cmpgt_epi8(
            _mm256_set1_epi8(0x30),
            _mm256_loadu_si256(chunk.as_ptr().add(*p) as *const __m256i),
        )) as u32;
        let (p0, m1) = (mask.trailing_zeros() as usize, mask & (mask - 1));
        let (p1, p2) = (
            m1.trailing_zeros() as usize,
            (m1 & (m1 - 1)).trailing_zeros() as usize,
        );
        if p2 < 32
            && p0 < 8
            && (p1 - p0 - 1) < 8
            && (p2 - p1 - 1) < 8
            && *chunk.get_unchecked(*p + p0) == b' '
            && *chunk.get_unchecked(*p + p1) == b' '
            && *chunk.get_unchecked(*p + p2) == b'\n'
        {
            let b = chunk.as_ptr().add(*p);
            let res = (
                parse_swar(b, p0),
                parse_swar(b.add(p0 + 1), p1 - p0 - 1),
                parse_swar(b.add(p1 + 1), p2 - p1 - 1),
            );
            *p += p2 + 1;
            return res;
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
    touched: Vec<u16>,
}

impl PoolDial {
    fn new() -> Self {
        PoolDial {
            head: [u32::MAX; WINDOW],
            bitmap: [0u64; WINDOW / 64],
            summary: 0,
            pool: Vec::with_capacity(8192),
            free_head: u32::MAX,
            at: 0,
            len: 0,
            far: BinaryHeap::new(),
            far_min: u32::MAX,
            touched: Vec::with_capacity(1024),
        }
    }

    #[inline(always)]
    fn push(&mut self, key: u32, node: u32) {
        if key < self.at + WINDOW as u32 {
            let b = (key as usize) & (WINDOW - 1);
            let old_h = unsafe { *self.head.get_unchecked(b) };
            if old_h == u32::MAX {
                unsafe {
                    *self.bitmap.get_unchecked_mut(b >> 6) |= 1u64 << (b & 63);
                }
                self.summary |= 1u64 << (b >> 6);
                self.touched.push(b as u16);
            }
            let packed = (node as u64) | ((old_h as u64) << 32);
            let new_h = if self.free_head != u32::MAX {
                let s = self.free_head as usize;
                unsafe {
                    self.free_head = (*self.pool.get_unchecked(s) >> 32) as u32;
                    *self.pool.get_unchecked_mut(s) = packed;
                }
                s as u32
            } else {
                let s = self.pool.len() as u32;
                self.pool.push(packed);
                s
            };
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
                let (node, nxt) = (entry as u32, (entry >> 32) as u32);
                unsafe {
                    *self.head.get_unchecked_mut(b) = nxt;
                    *self.pool.get_unchecked_mut(h as usize) = (self.free_head as u64) << 32;
                }
                self.free_head = h;
                if nxt == u32::MAX {
                    unsafe {
                        let bit = *self.bitmap.get_unchecked(b >> 6) & !(1u64 << (b & 63));
                        *self.bitmap.get_unchecked_mut(b >> 6) = bit;
                        if bit == 0 {
                            self.summary &= !(1u64 << (b >> 6));
                        }
                    }
                }
                return Some((self.at, node));
            }
            if self.far_min == self.at {
                let p = self.far.pop().map(|r| r.0);
                self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                return p;
            }
            if self.len == 0 {
                return if self.far_min < u32::MAX {
                    self.at = self.far_min;
                    let p = self.far.pop().map(|r| r.0);
                    self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                    p
                } else {
                    None
                };
            }
            let (start_b, w_idx) = (
                (self.at as usize) & (WINDOW - 1),
                ((self.at as usize) & (WINDOW - 1)) >> 6,
            );
            let cur_w = unsafe { *self.bitmap.get_unchecked(w_idx) } >> (start_b & 63);
            if cur_w != 0 {
                self.at += cur_w.trailing_zeros();
                continue;
            }
            let rot = self.summary.rotate_right(((w_idx + 1) & 63) as u32);
            if rot != 0 {
                let nw = (w_idx + rot.trailing_zeros() as usize + 1) & 63;
                let nb = (nw << 6)
                    | (unsafe { *self.bitmap.get_unchecked(nw) }.trailing_zeros() as usize);
                self.at += (nb.wrapping_sub(start_b) & (WINDOW - 1)) as u32;
                continue;
            }
            if self.far_min < u32::MAX {
                self.at = self.far_min;
                let p = self.far.pop().map(|r| r.0);
                self.far_min = self.far.peek().map_or(u32::MAX, |r| r.0 .0);
                return p;
            }
            return None;
        }
    }

    #[inline(always)]
    fn clear(&mut self) {
        for &b in &self.touched {
            unsafe {
                *self.head.get_unchecked_mut(b as usize) = u32::MAX;
                *self.bitmap.get_unchecked_mut((b as usize) >> 6) = 0;
            }
        }
        self.touched.clear();
        (self.summary, self.at, self.len, self.far_min, self.free_head) =
            (0, 0, 0, u32::MAX, u32::MAX);
        self.pool.clear();
        self.far.clear();
    }
}

#[inline(never)]
#[cold]
fn lookup_overflow(overflow: &[(u32, u64)], edge_idx: u32) -> u64 {
    overflow
        .binary_search_by_key(&edge_idx, |&(i, _)| i)
        .map_or(2047, |p| overflow[p].1)
}

#[cold]
#[inline(never)]
fn handle_overflow(overflow: &[(u32, u64)], ei: usize, needs_wide: &mut bool) -> u64 {
    let w = lookup_overflow(overflow, ei as u32);
    if w > 1_000_000 {
        *needs_wide = true;
    }
    w
}

fn search_fast(
    start: &[u32],
    adj: &[u32],
    overflow: &[(u32, u64)],
    s: u32,
    t: u32,
    dist: &mut [u16],
    wide: &mut Vec<u64>,
    touched: &mut Vec<u32>,
    heap: &mut PoolDial,
) -> i64 {
    unsafe {
        *dist.get_unchecked_mut(s as usize) = !0u16;
    }
    touched.push(s);
    heap.push(0, s);
    let (mut ans, mut needs_wide) = (-1i64, false);

    'search: while let Some((d, u)) = heap.pop() {
        unsafe {
            if !(d as u16) < *dist.get_unchecked(u as usize) {
                continue;
            }
            if u == t {
                ans = d as i64;
                break;
            }
            let (es, ee) = (
                *start.get_unchecked(u as usize) as usize,
                *start.get_unchecked(u as usize + 1) as usize,
            );
            for k in es..ee {
                prefetch(dist.as_ptr().add((*adj.get_unchecked(k) >> 11) as usize));
            }
            for ei in es..ee {
                let entry = *adj.get_unchecked(ei);
                let (v, mut w) = ((entry >> 11) as usize, (entry & 0x7FF) as u64);
                if w == 0x7FF {
                    w = handle_overflow(overflow, ei, &mut needs_wide);
                    if needs_wide {
                        break 'search;
                    }
                }
                let nd = d + w as u32;
                if nd > 60000 {
                    needs_wide = true;
                    break 'search;
                }
                let (nd16, cur) = (nd as u16, *dist.get_unchecked(v));
                if !nd16 > cur {
                    if cur == 0 {
                        touched.push(v as u32);
                    }
                    *dist.get_unchecked_mut(v) = !nd16;
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
            let (es, ee) = (
                *start.get_unchecked(u as usize) as usize,
                *start.get_unchecked(u as usize + 1) as usize,
            );
            for ei in es..ee {
                let entry = *adj.get_unchecked(ei);
                let (v, mut w) = ((entry >> 11) as usize, (entry & 0x7FF) as u64);
                if w == 0x7FF {
                    w = lookup_overflow(overflow, ei as u32);
                }
                let (nd, cur) = (d + w, *dist.get_unchecked(v));
                if !nd > cur {
                    if cur == 0 {
                        touched.push(v as u32);
                    }
                    *dist.get_unchecked_mut(v) = !nd;
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
    let t0 = std::time::Instant::now();
    let (g, g_len, g_fd) = map_file("/data/graph.txt");
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

    par(CHUNKS, || (), |_, c| {
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
    });

    let mut start0 = vec![0u32; n + 1];
    for i in 0..n {
        start0[i + 1] = start0[i] + degree[i].swap(start0[i], Relaxed);
    }
    let m = start0[n] as usize;

    let edges0: Vec<AtomicU32> = (0..m).map(|_| AtomicU32::new(0)).collect();
    let of0_threads = par(CHUNKS, || Vec::new(), |local_of, c| {
        let chunk = &g[cut(c)..cut(c + 1)];
        let mut p = 0;
        while p < chunk.len() {
            unsafe {
                let (u, v, w) = parse_line(chunk, &mut p);
                if u != v {
                    let slot = degree[u as usize].fetch_add(1, Relaxed) as usize;
                    if w < 2047 {
                        edges0[slot].store((v << 11) | w, Relaxed);
                    } else {
                        edges0[slot].store((v << 11) | 0x7FF, Relaxed);
                        local_of.push((slot as u32, w as u64));
                    }
                }
            }
        }
        std::mem::take(local_of)
    });
    drop(degree);
    unsafe {
        unmap_file(g, g_len, g_fd);
    }

    let mut overflow0: Vec<(u32, u64)> = of0_threads.into_iter().flatten().collect();
    overflow0.sort_unstable_by_key(|&(i, _)| i);

    let edges0: Vec<u32> = unsafe {
        let mut e = std::mem::ManuallyDrop::new(edges0);
        Vec::from_raw_parts(e.as_mut_ptr() as *mut u32, e.len(), e.capacity())
    };

    let claimed: Vec<AtomicU64> = (0..(n + 63) / 64).map(|_| AtomicU64::new(0)).collect();
    let (scan, order_cur) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let mut order = vec![0u32; n];
    let order_ptr = order.as_mut_ptr() as usize;

    par(THREADS, || Vec::with_capacity(REGION), |reg, _| {
        let out = order_ptr as *mut u32;
        loop {
            let from = scan.fetch_add(1024, Relaxed);
            if from >= n {
                break;
            }
            for root in from..(from + 1024).min(n) {
                let bit = 1u64 << (root & 63);
                if (claimed[root >> 6].fetch_or(bit, Relaxed) & bit) != 0 {
                    continue;
                }
                reg.clear();
                reg.push(root as u32);
                let mut head = 0;
                while head < reg.len() && reg.len() < REGION {
                    let u = reg[head] as usize;
                    head += 1;
                    let (s, e) = (start0[u] as usize, start0[u + 1] as usize);
                    for idx in s..e {
                        let v = (edges0[idx] >> 11) as usize;
                        let v_bit = 1u64 << (v & 63);
                        if (claimed[v >> 6].fetch_or(v_bit, Relaxed) & v_bit) == 0 {
                            reg.push(v as u32);
                        }
                    }
                }
                let k = reg.len();
                let base = order_cur.fetch_add(k, Relaxed);
                unsafe {
                    std::ptr::copy_nonoverlapping(reg.as_ptr(), out.add(base), k);
                }
            }
        }
    });
    drop(claimed);

    let (mut new_id, mut first) = (vec![0u32; n], vec![0u32; n + 1]);
    let (nid_ptr, f_ptr) = (new_id.as_mut_ptr() as usize, first.as_mut_ptr() as usize);

    par(THREADS, || (), |_, t| {
        let (i_start, i_end) = (t * n / THREADS, (t + 1) * n / THREADS);
        let (nid, f) = (nid_ptr as *mut u32, f_ptr as *mut u32);
        for i in i_start..i_end {
            let u = order[i] as usize;
            unsafe {
                *nid.add(u) = i as u32;
                *f.add(i + 1) = start0[u + 1] - start0[u];
            }
        }
    });

    for i in 0..n {
        first[i + 1] += first[i];
    }

    let mut adj = vec![0u32; m];
    let adj_ptr = adj.as_mut_ptr() as usize;

    let of_threads = par(THREADS, || Vec::new(), |local_of, t| {
        let (i_start, i_end) = (t * n / THREADS, (t + 1) * n / THREADS);
        let out_adj = adj_ptr as *mut u32;
        let mut out_idx = first[i_start] as usize;

        for i in i_start..i_end {
            let old_u = order[i] as usize;
            let (s, e) = (start0[old_u] as usize, start0[old_u + 1] as usize);
            for old_idx in s..e {
                let entry = edges0[old_idx];
                let (old_v, w) = ((entry >> 11) as usize, entry & 0x7FF);
                let nv = new_id[old_v];
                unsafe {
                    *out_adj.add(out_idx) = (nv << 11) | w;
                }
                if w == 0x7FF {
                    let true_w = lookup_overflow(&overflow0, old_idx as u32);
                    local_of.push((out_idx as u32, true_w));
                }
                out_idx += 1;
            }
        }
        std::mem::take(local_of)
    });

    drop(edges0);
    drop(start0);
    drop(order);
    drop(overflow0);

    let mut overflow: Vec<(u32, u64)> = of_threads.into_iter().flatten().collect();
    overflow.sort_unstable_by_key(|&(i, _)| i);

    let (q, q_len, q_fd) = map_file("/data/queries.txt");
    let mut p = 0;
    let queries: Vec<_> = (0..q.iter().filter(|&&c| c == b'\n').count())
        .map(|_| (num(q, &mut p) as usize, num(q, &mut p) as usize))
        .collect();
    unsafe {
        unmap_file(q, q_len, q_fd);
    }

    let mut order_q: Vec<usize> = (0..queries.len()).collect();
    order_q.sort_unstable_by_key(|&i| new_id[queries[i].0]);

    let mut answers = vec![0i64; queries.len()];
    let ans_ptr = answers.as_mut_ptr() as usize;

    let share = |t: usize| t * queries.len() / THREADS;
    let next: Vec<AtomicUsize> = (0..THREADS).map(|t| AtomicUsize::new(share(t))).collect();
    std::thread::scope(|sc| {
        for me in 0..THREADS {
            let (next, order_q, queries, has_in, first, adj, overflow, new_id) = (
                &next, &order_q, &queries, &has_in, &first, &adj, &overflow, &new_id,
            );
            sc.spawn(move || {
                let mut dist = vec![0u16; n];
                let mut wide = Vec::new();
                let mut touched = Vec::with_capacity(4096);
                let mut heap = PoolDial::new();
                let ans_out = ans_ptr as *mut i64;

                for t in (me..THREADS).chain(0..me) {
                    loop {
                        let j = next[t].fetch_add(1, Relaxed);
                        if j >= share(t + 1) {
                            break;
                        }
                        let orig_idx = order_q[j];
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
    eprintln!("ELAPSED: {:?}", t0.elapsed());
    std::fs::write("/out/distances.txt", out).expect("write output");
}
