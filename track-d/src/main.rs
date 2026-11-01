#![allow(unsafe_op_in_unsafe_fn)]
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    __m256i, _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_set1_epi8,
};

const THREADS: usize = 12;
const CHUNKS: usize = 48;
const REGION: usize = 1 << 14;
const WINDOW: usize = 1 << 12;

#[cfg(unix)]
extern "C" {
    fn mmap(a: *mut u8, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut u8;
    fn munmap(a: *mut u8, l: usize) -> i32;
    fn madvise(a: *mut u8, l: usize, adv: i32) -> i32;
    fn posix_fadvise(fd: i32, o: i64, l: i64, adv: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn malloc_trim(pad: usize) -> i32;
}

#[cfg(unix)]
fn map_file(path: &str) -> (&'static [u8], usize, i32) {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::File::open(path).expect("open");
    let (len, fd) = (f.metadata().unwrap().len() as usize, f.as_raw_fd());
    std::mem::forget(f);
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, fd, 0) };
    unsafe {
        madvise(ptr, len, 2);
        posix_fadvise(fd, 0, len as i64, 2);
        (std::slice::from_raw_parts(ptr, len), len, fd)
    }
}

#[cfg(unix)]
unsafe fn unmap_file(s: &[u8], len: usize, fd: i32) {
    madvise(s.as_ptr() as *mut u8, len, 4);
    munmap(s.as_ptr() as *mut u8, len);
    posix_fadvise(fd, 0, len as i64, 4);
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

fn num(b: &[u8], p: &mut usize) -> u64 {
    while *p < b.len() && b[*p] < b'0' {
        *p += 1;
    }
    let mut x = 0u64;
    while *p < b.len() && b[*p] >= b'0' {
        x = x * 10 + (b[*p] - b'0') as u64;
        *p += 1;
    }
    x
}

#[inline(always)]
unsafe fn parse_swar(p: *const u8, len: usize) -> u32 {
    let r = ((p as *const u64).read_unaligned() << (8 * (8 - len))) & 0x0F0F0F0F0F0F0F0F;
    let x = (r.wrapping_mul(2561) >> 8) & 0x00FF00FF00FF00FF;
    let x = (x.wrapping_mul(6553601) >> 16) & 0x0000FFFF0000FFFF;
    (x.wrapping_mul(42949672960001) >> 32) as u32
}

#[inline(always)]
unsafe fn parse_line(chunk: &[u8], p: &mut usize) -> (u32, u32, u32) {
    while *p < chunk.len() && chunk[*p] < b'0' {
        *p += 1;
    }
    #[cfg(target_arch = "x86_64")]
    if *p + 32 <= chunk.len() {
        let ptr = chunk.as_ptr().add(*p);
        let v256 = _mm256_loadu_si256(ptr as *const __m256i);
        let mask = _mm256_movemask_epi8(_mm256_cmpgt_epi8(_mm256_set1_epi8(0x30), v256)) as u32;
        let (p0, m1) = (mask.trailing_zeros() as usize, mask & (mask - 1));
        let (p1, p2) = (
            m1.trailing_zeros() as usize,
            (m1 & (m1 - 1)).trailing_zeros() as usize,
        );
        let valid = p2 < 32 && p0 < 8 && p1 - p0 <= 8 && p2 - p1 <= 8;
        if valid && *ptr.add(p0) == b' ' && *ptr.add(p1) == b' ' && *ptr.add(p2) == b'\n' {
            *p += p2 + 1;
            let (u, v) = (
                parse_swar(ptr, p0),
                parse_swar(ptr.add(p0 + 1), p1 - p0 - 1),
            );
            return (u, v, parse_swar(ptr.add(p1 + 1), p2 - p1 - 1));
        }
    }
    let u = num(chunk, p) as u32;
    *p += 1;
    let v = num(chunk, p) as u32;
    *p += 1;
    (u, v, num(chunk, p) as u32)
}

fn par<R: Send>(jobs: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut all: Vec<(usize, R)> = std::thread::scope(|sc| {
        let h: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let mut got = Vec::new();
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            break;
                        }
                        got.push((j, f(j)));
                    }
                    got
                })
            })
            .collect();
        h.into_iter().flat_map(|w| w.join().unwrap()).collect()
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
        }
    }

    #[inline(always)]
    unsafe fn push(&mut self, key: u32, node: u32) {
        let b = (key as usize) & (WINDOW - 1);
        let old_h = *self.head.get_unchecked(b);
        if old_h == u32::MAX {
            *self.bitmap.get_unchecked_mut(b >> 6) |= 1u64 << (b & 63);
            self.summary |= 1u64 << (b >> 6);
        }
        let packed = (node as u64) | ((old_h as u64) << 32);
        let new_h = if self.free_head != u32::MAX {
            let s = self.free_head as usize;
            self.free_head = (*self.pool.get_unchecked(s) >> 32) as u32;
            *self.pool.get_unchecked_mut(s) = packed;
            s as u32
        } else {
            let s = self.pool.len() as u32;
            self.pool.push(packed);
            s
        };
        *self.head.get_unchecked_mut(b) = new_h;
        self.len += 1;
    }

    #[inline(always)]
    unsafe fn pop(&mut self) -> Option<(u32, u32)> {
        loop {
            let b = (self.at as usize) & (WINDOW - 1);
            let h = *self.head.get_unchecked(b);
            if h != u32::MAX {
                self.len -= 1;
                let entry = *self.pool.get_unchecked(h as usize);
                let (node, nxt) = (entry as u32, (entry >> 32) as u32);
                *self.head.get_unchecked_mut(b) = nxt;
                *self.pool.get_unchecked_mut(h as usize) = (self.free_head as u64) << 32;
                self.free_head = h;
                if nxt == u32::MAX {
                    let bit = *self.bitmap.get_unchecked(b >> 6) & !(1u64 << (b & 63));
                    *self.bitmap.get_unchecked_mut(b >> 6) = bit;
                    if bit == 0 {
                        self.summary &= !(1u64 << (b >> 6));
                    }
                }
                return Some((self.at, node));
            }
            if self.len == 0 {
                return None;
            }
            let start_b = (self.at as usize) & (WINDOW - 1);
            let w_idx = start_b >> 6;
            let cur_w = *self.bitmap.get_unchecked(w_idx) >> (start_b & 63);
            if cur_w != 0 {
                self.at += cur_w.trailing_zeros();
                continue;
            }
            let rot = self.summary.rotate_right(((w_idx + 1) & 63) as u32);
            if rot != 0 {
                let nw = (w_idx + rot.trailing_zeros() as usize + 1) & 63;
                let nb = (nw << 6) | (*self.bitmap.get_unchecked(nw)).trailing_zeros() as usize;
                self.at += (nb.wrapping_sub(start_b) & (WINDOW - 1)) as u32;
                continue;
            }
            return None;
        }
    }

    #[inline(always)]
    unsafe fn clear(&mut self) {
        let mut s = self.summary;
        while s != 0 {
            let w_idx = s.trailing_zeros() as usize;
            let mut word = *self.bitmap.get_unchecked(w_idx);
            while word != 0 {
                let bit_idx = word.trailing_zeros() as usize;
                let b = (w_idx << 6) | bit_idx;
                *self.head.get_unchecked_mut(b) = u32::MAX;
                word &= word - 1;
            }
            *self.bitmap.get_unchecked_mut(w_idx) = 0;
            s &= s - 1;
        }
        (self.summary, self.at, self.len, self.free_head) = (0, 0, 0, u32::MAX);
        self.pool.clear();
    }
}

#[cold]
fn lookup_overflow(ov: &[(u32, u64)], idx: u32) -> u64 {
    ov.binary_search_by_key(&idx, |&(i, _)| i)
        .map_or(2047, |p| ov[p].1)
}

struct Graph<'a> {
    start: &'a [u32],
    adj: &'a [u32],
    overflow: &'a [(u32, u64)],
}

struct Worker {
    dist: Vec<u16>,
    wide: Vec<u64>,
    touched: Vec<u32>,
    heap: PoolDial,
}

unsafe fn search_fast(g: &Graph, s: u32, t: u32, w: &mut Worker) -> i64 {
    *w.dist.get_unchecked_mut(s as usize) = !0u16;
    w.touched.push(s);
    w.heap.push(0, s);
    let (mut ans, mut needs_wide) = (-1i64, false);

    'search: while let Some((d, u)) = w.heap.pop() {
        if !(d as u16) < *w.dist.get_unchecked(u as usize) {
            continue;
        }
        if u == t {
            ans = d as i64;
            break;
        }
        if d > 60000 {
            needs_wide = true;
            break 'search;
        }
        let pair = (g.start.as_ptr().add(u as usize) as *const u64).read_unaligned();
        let (es, ee) = (pair as u32 as usize, (pair >> 32) as usize);
        for ei in es..ee {
            let entry = *g.adj.get_unchecked(ei);
            let (v, mut wt) = ((entry >> 11) as usize, entry & 0x7FF);
            if wt == 0x7FF {
                let true_w = lookup_overflow(g.overflow, ei as u32);
                if true_w > 1_000_000 {
                    needs_wide = true;
                    break 'search;
                }
                wt = true_w as u32;
            }
            let (nd, cur) = (d + wt, *w.dist.get_unchecked(v));
            let nd16 = nd as u16;
            if !nd16 > cur {
                if cur == 0 {
                    w.touched.push(v as u32);
                }
                *w.dist.get_unchecked_mut(v) = !nd16;
                w.heap.push(nd, v as u32);
            }
        }
    }
    w.heap.clear();
    for v in w.touched.drain(..) {
        *w.dist.get_unchecked_mut(v as usize) = 0;
    }
    if needs_wide {
        if w.wide.is_empty() {
            w.wide = vec![0u64; w.dist.len()];
        }
        return search_wide(g, s, t, w);
    }
    ans
}

unsafe fn search_wide(g: &Graph, s: u32, t: u32, w: &mut Worker) -> i64 {
    let mut heap = BinaryHeap::new();
    *w.wide.get_unchecked_mut(s as usize) = !0u64;
    w.touched.push(s);
    heap.push(Reverse((0u64, s)));
    let mut ans = -1i64;
    while let Some(Reverse((d, u))) = heap.pop() {
        if !d < *w.wide.get_unchecked(u as usize) {
            continue;
        }
        if u == t {
            ans = d as i64;
            break;
        }
        let pair = (g.start.as_ptr().add(u as usize) as *const u64).read_unaligned();
        let (es, ee) = (pair as u32 as usize, (pair >> 32) as usize);
        for ei in es..ee {
            let entry = *g.adj.get_unchecked(ei);
            let (v, mut wt) = ((entry >> 11) as usize, (entry & 0x7FF) as u64);
            if wt == 0x7FF {
                wt = lookup_overflow(g.overflow, ei as u32);
            }
            let (nd, cur) = (d + wt, *w.wide.get_unchecked(v));
            if !nd > cur {
                if cur == 0 {
                    w.touched.push(v as u32);
                }
                *w.wide.get_unchecked_mut(v) = !nd;
                heap.push(Reverse((nd, v as u32)));
            }
        }
    }
    for v in w.touched.drain(..) {
        *w.wide.get_unchecked_mut(v as usize) = 0;
    }
    ans
}

fn main() {
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

    let cuts: Vec<usize> = (0..=CHUNKS).map(|i| cut(i)).collect();
    let degree: Vec<AtomicU32> = unsafe { std::mem::transmute(vec![0u32; n]) };
    let has_in: Vec<AtomicU64> = unsafe { std::mem::transmute(vec![0u64; (n + 63) / 64]) };

    par(CHUNKS, |c| {
        let (chunk, mut p) = (&g[cuts[c]..cuts[c + 1]], 0);
        while p < chunk.len() {
            let (u, v, _) = unsafe { parse_line(chunk, &mut p) };
            if u != v {
                degree[u as usize].fetch_add(1, Relaxed);
                let (bit, idx) = (1u64 << (v & 63), (v >> 6) as usize);
                if (has_in[idx].load(Relaxed) & bit) == 0 {
                    has_in[idx].fetch_or(bit, Relaxed);
                }
            }
        }
    });

    let (mut start0, mut cur) = (vec![0u32; n + 1], 0u32);
    for (d, s) in degree.iter().zip(&mut start0[..n]) {
        *s = cur;
        cur += d.swap(cur, Relaxed);
    }
    start0[n] = cur;
    let m = cur as usize;

    let edges0: Vec<AtomicU32> = unsafe { std::mem::transmute(vec![0u32; m]) };
    let of0_threads = par(CHUNKS, |c| {
        let (chunk, mut p, mut local_of) = (&g[cuts[c]..cuts[c + 1]], 0, Vec::new());
        while p < chunk.len() {
            let (u, v, w) = unsafe { parse_line(chunk, &mut p) };
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
        local_of
    });
    drop(degree);
    unsafe {
        unmap_file(g, g_len, g_fd);
    }

    let mut overflow0: Vec<(u32, u64)> = of0_threads.into_iter().flatten().collect();
    overflow0.sort_unstable_by_key(|&(i, _)| i);

    let edges0: Vec<u32> = unsafe { std::mem::transmute(edges0) };

    let claimed: Vec<AtomicU64> = unsafe { std::mem::transmute(vec![0u64; (n + 63) / 64]) };
    let (scan, order_cur) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let mut order = vec![0u32; n];
    let order_ptr = order.as_mut_ptr() as usize;

    par(THREADS, |_| {
        let out = order_ptr as *mut u32;
        let mut reg = Vec::with_capacity(REGION);
        let claim = |node: usize| {
            let (bit, idx) = (1u64 << (node & 63), node >> 6);
            (claimed[idx].load(Relaxed) & bit) == 0
                && (claimed[idx].fetch_or(bit, Relaxed) & bit) == 0
        };
        loop {
            let from = scan.fetch_add(1024, Relaxed);
            if from >= n {
                break;
            }
            for root in from..(from + 1024).min(n) {
                if !claim(root) {
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
                        if claim(v) {
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
    for i in 0..n {
        let u = order[i] as usize;
        new_id[u] = i as u32;
        first[i + 1] = first[i] + (start0[u + 1] - start0[u]);
    }
    drop(order);

    let mut adj = vec![0u32; m];
    let adj_ptr = adj.as_mut_ptr() as usize;

    let of_threads = par(CHUNKS, |c| {
        let (u_start, u_end) = (c * n / CHUNKS, (c + 1) * n / CHUNKS);
        let out_adj = adj_ptr as *mut u32;
        let mut local_of = Vec::new();

        for old_u in u_start..u_end {
            let new_u = new_id[old_u] as usize;
            let out_base = first[new_u] as usize;
            let (s, e) = (start0[old_u] as usize, start0[old_u + 1] as usize);
            for old_idx in s..e {
                let entry = edges0[old_idx];
                let (old_v, w) = ((entry >> 11) as usize, entry & 0x7FF);
                let nv = new_id[old_v];
                let out_idx = out_base + (old_idx - s);
                unsafe {
                    *out_adj.add(out_idx) = (nv << 11) | w;
                }
                if w == 0x7FF {
                    local_of.push((out_idx as u32, lookup_overflow(&overflow0, old_idx as u32)));
                }
            }
        }
        local_of
    });

    drop(edges0);
    drop(start0);
    drop(overflow0);

    let mut overflow: Vec<(u32, u64)> = of_threads.into_iter().flatten().collect();
    overflow.sort_unstable_by_key(|&(i, _)| i);
    let graph = Graph {
        start: &first,
        adj: &adj,
        overflow: &overflow,
    };

    let (q, q_len, q_fd) = map_file("/data/queries.txt");
    let (mut p, num_queries) = (0, q.iter().filter(|&&c| c == b'\n').count());
    let mut answers = vec![-1i64; num_queries];
    let mut active_queries = Vec::with_capacity(num_queries);
    for orig_idx in 0..num_queries {
        let (s, t_node) = (num(q, &mut p) as usize, num(q, &mut p) as usize);
        if s == t_node {
            answers[orig_idx] = 0;
        } else if (has_in[t_node >> 6].load(Relaxed) & (1u64 << (t_node & 63))) != 0 {
            let (s_new, t_new) = (new_id[s], new_id[t_node]);
            if first[s_new as usize + 1] > first[s_new as usize] {
                active_queries.push((s_new, t_new, orig_idx as u32));
            }
        }
    }
    unsafe {
        unmap_file(q, q_len, q_fd);
    }
    drop(has_in);
    drop(new_id);
    #[cfg(unix)]
    unsafe {
        malloc_trim(0);
    }

    active_queries.sort_unstable_by_key(|&(s_new, _, _)| s_new);

    let total_active = active_queries.len();
    let share = |t: usize| t * total_active / THREADS;
    let next: Vec<AtomicUsize> = (0..THREADS).map(|t| AtomicUsize::new(share(t))).collect();
    let ans_ptr = answers.as_mut_ptr() as usize;

    std::thread::scope(|sc| {
        for me in 0..THREADS {
            let (next, active_queries, graph) = (&next, &active_queries, &graph);
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn_scoped(sc, move || {
                    let mut w = Worker {
                        dist: vec![0u16; n],
                        wide: Vec::new(),
                        touched: Vec::with_capacity(32768),
                        heap: PoolDial::new(),
                    };
                    let ans_out = ans_ptr as *mut i64;

                    for t in (me..THREADS).chain(0..me) {
                        loop {
                            let j = next[t].fetch_add(1, Relaxed);
                            if j >= share(t + 1) {
                                break;
                            }
                            let (s_new, t_new, orig_idx) = active_queries[j];
                            let ans = unsafe { search_fast(graph, s_new, t_new, &mut w) };
                            unsafe {
                                *ans_out.add(orig_idx as usize) = ans;
                            }
                        }
                    }
                })
                .unwrap();
        }
    });

    let mut out = Vec::with_capacity(answers.len() * 8);
    for a in answers {
        if a < 0 {
            out.extend_from_slice(b"-1\n");
            continue;
        }
        let (mut buf, mut i, mut x) = ([b'\n'; 20], 19, a as u64);
        while {
            i -= 1;
            buf[i] = b'0' + (x % 10) as u8;
            x /= 10;
            x > 0
        } {}
        out.extend_from_slice(&buf[i..]);
    }
    std::fs::write("/out/distances.txt", out).expect("write output");
}
