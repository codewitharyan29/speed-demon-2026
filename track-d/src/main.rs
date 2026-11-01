#![allow(unsafe_op_in_unsafe_fn)]
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};

#[cfg(unix)]
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn malloc_trim(pad: usize) -> i32;
    fn _exit(status: i32) -> !;
}

#[cfg(unix)]
unsafe fn alloc_anon(bytes: usize) -> *mut u8 {
    mmap(core::ptr::null_mut(), bytes, 3, 0x22, -1, 0)
}
#[cfg(unix)]
unsafe fn free_anon(ptr: *mut u8, bytes: usize) {
    munmap(ptr, bytes);
}
#[cfg(not(unix))]
unsafe fn alloc_anon(bytes: usize) -> *mut u8 {
    std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align_unchecked(bytes, 4096))
}
#[cfg(not(unix))]
unsafe fn free_anon(ptr: *mut u8, bytes: usize) {
    std::alloc::dealloc(ptr, std::alloc::Layout::from_size_align_unchecked(bytes, 4096));
}

#[cfg(unix)]
unsafe fn map_file(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open");
    let len = f.metadata().unwrap().len() as usize;
    use std::os::unix::io::AsRawFd;
    let ptr = mmap(core::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0);
    core::slice::from_raw_parts(ptr, len)
}
#[cfg(not(unix))]
unsafe fn map_file(path: &str) -> &'static [u8] {
    Box::leak(std::fs::read(path).expect("read").into_boxed_slice())
}

#[inline(always)]
unsafe fn parse_u32(b: &[u8], i: &mut usize) -> u32 {
    while *i < b.len() && *b.get_unchecked(*i) < b'0' { *i += 1; }
    let mut x = (*b.get_unchecked(*i) - b'0') as u32;
    *i += 1;
    while *i < b.len() && *b.get_unchecked(*i) >= b'0' {
        x = x * 10 + (*b.get_unchecked(*i) - b'0') as u32;
        *i += 1;
    }
    x
}

#[inline(always)]
unsafe fn skip_line(b: &[u8], mut i: usize) -> usize {
    while i + 8 <= b.len() {
        let w = (b.as_ptr().add(i) as *const u64).read_unaligned() ^ 0x0A0A_0A0A_0A0A_0A0A;
        let m = w.wrapping_sub(0x0101_0101_0101_0101) & !w & 0x8080_8080_8080_8080;
        if m != 0 {
            return i + (m.trailing_zeros() >> 3) as usize + 1;
        }
        i += 8;
    }
    while i < b.len() && *b.get_unchecked(i) != b'\n' { i += 1; }
    i + 1
}

const WINDOW: usize = 4096;

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
            pool: Vec::with_capacity(16384),
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
            if self.len == 0 { return None; }
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

struct Worker {
    dist: Vec<u16>,
    wide: Vec<u64>,
    touched: Vec<u32>,
    heap: PoolDial,
}

#[cold]
fn lookup_overflow(ov: &[(u32, u64)], idx: u32) -> u64 {
    ov.binary_search_by_key(&idx, |&(i, _)| i).map_or(2047, |p| ov[p].1)
}

unsafe fn search_fast(first: &[u32], adj: &[u32], ov: &[(u32, u64)], s: u32, t: u32, w: &mut Worker) -> i64 {
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
        let pair = (first.as_ptr().add(u as usize) as *const u64).read_unaligned();
        let (es, ee) = (pair as u32 as usize, (pair >> 32) as usize);
        for ei in es..ee {
            let entry = *adj.get_unchecked(ei);
            let (v, mut wt) = ((entry >> 11) as usize, entry & 0x7FF);
            if wt == 0x7FF {
                let true_w = lookup_overflow(ov, ei as u32);
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
        return search_wide(first, adj, ov, s, t, w);
    }
    ans
}

unsafe fn search_wide(first: &[u32], adj: &[u32], ov: &[(u32, u64)], s: u32, t: u32, w: &mut Worker) -> i64 {
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
        let pair = (first.as_ptr().add(u as usize) as *const u64).read_unaligned();
        let (es, ee) = (pair as u32 as usize, (pair >> 32) as usize);
        for ei in es..ee {
            let entry = *adj.get_unchecked(ei);
            let (v, mut wt) = ((entry >> 11) as usize, (entry & 0x7FF) as u64);
            if wt == 0x7FF {
                wt = lookup_overflow(ov, ei as u32);
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
    unsafe {
        let g_bytes = map_file("/data/graph.txt");
        let mut pos = 0;
        let n = parse_u32(g_bytes, &mut pos) as usize;
        let m = parse_u32(g_bytes, &mut pos) as usize;
        let body = &g_bytes[pos..];

        const T: usize = 12;
        let mut bounds = [0usize; T + 1];
        bounds[T] = body.len();
        for t in 1..T {
            let mut p = (body.len() * t) / T;
            while p < body.len() && *body.get_unchecked(p) != b'\n' { p += 1; }
            bounds[t] = (p + 1).min(body.len());
        }

        let s0_bytes = (n + 2) * 4;
        let e0_bytes = m * 4;
        let nid_bytes = n * 4;
        let has_in_bytes = ((n + 63) / 64) * 8;
        let vis_bytes = ((n + 63) / 64) * 8;
        let first_bytes = (n + 2) * 4;
        let adj_bytes = m * 4;

        let start0_p = alloc_anon(s0_bytes) as *mut u32;
        let edges0_p = alloc_anon(e0_bytes) as *mut u32;
        let new_id_p = alloc_anon(nid_bytes) as *mut u32;
        let has_in_p = alloc_anon(has_in_bytes) as *mut AtomicU64;
        let vis_p = alloc_anon(vis_bytes) as *mut AtomicU64;
        let first_p = alloc_anon(first_bytes) as *mut u32;
        let adj_p = alloc_anon(adj_bytes) as *mut u32;

        let mut t_sums = [0u32; T + 1];
        let bfs_seed = AtomicUsize::new(0);
        let bfs_rank = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(T);

        let mut of0_threads: Vec<Vec<(u32, u64)>> = (0..T).map(|_| Vec::new()).collect();
        let mut f_of_threads: Vec<Vec<(u32, u64)>> = (0..T).map(|_| Vec::new()).collect();
        let of0_ptrs: Vec<usize> = of0_threads.iter_mut().map(|v| v as *mut _ as usize).collect();
        let fof_ptrs: Vec<usize> = f_of_threads.iter_mut().map(|v| v as *mut _ as usize).collect();

        let mut of0_all: Vec<(u32, u64)> = Vec::new();
        let mut final_overflow: Vec<(u32, u64)> = Vec::new();
        let of0_all_ptr = &mut of0_all as *mut Vec<(u32, u64)> as usize;
        let final_of_ptr = &mut final_overflow as *mut Vec<(u32, u64)> as usize;

        let s0_addr = start0_p as usize;
        let e0_addr = edges0_p as usize;
        let nid_addr = new_id_p as usize;
        let has_in_addr = has_in_p as usize;
        let vis_addr = vis_p as usize;
        let first_addr = first_p as usize;
        let adj_addr = adj_p as usize;
        let ts_addr = t_sums.as_mut_ptr() as usize;

        let (of_ptrs_ref, fof_ptrs_ref) = (&of0_ptrs, &fof_ptrs);

        std::thread::scope(|s| {
            for tid in 0..T {
                let bar = &barrier;
                let seed_ref = &bfs_seed;
                let rank_ref = &bfs_rank;
                let of0_p = of_ptrs_ref[tid];
                let fof_p = fof_ptrs_ref[tid];

                std::thread::Builder::new()
                    .stack_size(256 * 1024)
                    .spawn_scoped(s, move || {
                        let s0 = s0_addr as *mut u32;
                        let s0_at = s0_addr as *const AtomicU32;
                        let e0 = e0_addr as *mut u32;
                        let nid = nid_addr as *mut u32;
                        let vis = vis_addr as *mut AtomicU64;
                        let has_in = has_in_addr as *mut AtomicU64;
                        let f_ptr = first_addr as *mut u32;
                        let adj_ptr = adj_addr as *mut u32;
                        let ts = ts_addr as *mut u32;

                        // Pass 1: Parse u and skip line
                        let chunk = &body[bounds[tid]..bounds[tid + 1]];
                        let mut i = 0;
                        while i < chunk.len() {
                            while i < chunk.len() && *chunk.get_unchecked(i) < b'0' { i += 1; }
                            if i >= chunk.len() { break; }
                            let u = parse_u32(chunk, &mut i) as usize;
                            (*s0_at.add(u + 2)).fetch_add(1, Relaxed);
                            i = skip_line(chunk, i);
                        }
                        bar.wait();

                        // Prefix Sum 1 over start0[2..n+2]
                        let p_start = 2 + (n * tid) / T;
                        let p_end = 2 + (n * (tid + 1)) / T;
                        let mut sum = 0u32;
                        for idx in p_start..p_end { sum += *s0.add(idx); }
                        *ts.add(tid + 1) = sum;
                        bar.wait();
                        if tid == 0 {
                            for k in 1..=T { *ts.add(k) += *ts.add(k - 1); }
                        }
                        bar.wait();
                        let mut acc = *ts.add(tid);
                        for idx in p_start..p_end {
                            acc += *s0.add(idx);
                            *s0.add(idx) = acc;
                        }
                        bar.wait();

                        // Pass 2: Parse u, v, w into edges0 and track has_in
                        let mut i = 0;
                        let of0_vec = &mut *(of0_p as *mut Vec<(u32, u64)>);
                        while i < chunk.len() {
                            while i < chunk.len() && *chunk.get_unchecked(i) < b'0' { i += 1; }
                            if i >= chunk.len() { break; }
                            let u = parse_u32(chunk, &mut i) as usize;
                            let v = parse_u32(chunk, &mut i);
                            let w = parse_u32(chunk, &mut i) as u64;
                            let (bit, idx) = (1u64 << (v & 63), (v >> 6) as usize);
                            if ((*has_in.add(idx)).load(Relaxed) & bit) == 0 {
                                (*has_in.add(idx)).fetch_or(bit, Relaxed);
                            }
                            let w_clamped = (w.min(2047)) as u32;
                            let slot = (*s0_at.add(u + 1)).fetch_add(1, Relaxed) as usize;
                            *e0.add(slot) = (v << 11) | w_clamped;
                            if w >= 2047 {
                                of0_vec.push((slot as u32, w));
                            }
                        }
                        bar.wait();

                        // Combine of0 on thread 0
                        if tid == 0 {
                            let of0_target = &mut *(of0_all_ptr as *mut Vec<(u32, u64)>);
                            for &p in of_ptrs_ref {
                                let v = &*( p as *const Vec<(u32, u64)> );
                                of0_target.extend_from_slice(v);
                            }
                            of0_target.sort_unstable_by_key(|&(idx, _)| idx);
                        }
                        bar.wait();

                        // Step 4: Region-growing BFS writing new_id and degree directly
                        let mut q_buf = [0u32; 8192];
                        loop {
                            let blk = seed_ref.fetch_add(1024, Relaxed);
                            if blk >= n { break; }
                            let blk_end = (blk + 1024).min(n);
                            for root in blk..blk_end {
                                let mask = 1u64 << (root & 63);
                                let cell = vis.add(root >> 6);
                                if ((*cell).load(Relaxed) & mask) != 0 { continue; }
                                if ((*cell).fetch_or(mask, Relaxed) & mask) != 0 { continue; }
                                let (mut head, mut tail) = (0usize, 1usize);
                                q_buf[0] = root as u32;
                                while head < tail && tail < 8192 {
                                    let u = q_buf[head] as usize;
                                    head += 1;
                                    let es = *s0.add(u) as usize;
                                    let ee = *s0.add(u + 1) as usize;
                                    for ei in es..ee {
                                        let v = (*e0.add(ei) >> 11) as usize;
                                        let vm = 1u64 << (v & 63);
                                        let vc = vis.add(v >> 6);
                                        if ((*vc).load(Relaxed) & vm) == 0 && ((*vc).fetch_or(vm, Relaxed) & vm) == 0 {
                                            q_buf[tail] = v as u32;
                                            tail += 1;
                                            if tail == 8192 { break; }
                                        }
                                    }
                                }
                                let base_r = rank_ref.fetch_add(tail, Relaxed);
                                for k in 0..tail {
                                    let u_old = q_buf[k] as usize;
                                    let u_new = base_r + k;
                                    *nid.add(u_old) = u_new as u32;
                                    *f_ptr.add(u_new + 1) = *s0.add(u_old + 1) - *s0.add(u_old);
                                }
                            }
                        }
                        bar.wait();

                        // Prefix Sum 2 over first[1..=n]
                        let f_start = 1 + (n * tid) / T;
                        let f_end = 1 + (n * (tid + 1)) / T;
                        let mut fsum = 0u32;
                        for idx in f_start..f_end { fsum += *f_ptr.add(idx); }
                        *ts.add(tid + 1) = fsum;
                        bar.wait();
                        if tid == 0 {
                            *ts.add(0) = 0;
                            for k in 1..=T { *ts.add(k) += *ts.add(k - 1); }
                        }
                        bar.wait();
                        let mut facc = *ts.add(tid);
                        for idx in f_start..f_end {
                            facc += *f_ptr.add(idx);
                            *f_ptr.add(idx) = facc;
                        }
                        bar.wait();

                        // Step 6: Scatter edges0 into adj with new_id prefetch and overflow tracking
                        let u_start = (n * tid) / T;
                        let u_end = (n * (tid + 1)) / T;
                        let fof_vec = &mut *(fof_p as *mut Vec<(u32, u64)>);
                        let of0_ref = &*(of0_all_ptr as *const Vec<(u32, u64)>);
                        for u_old in u_start..u_end {
                            let u_new = *nid.add(u_old) as usize;
                            let mut dst = *f_ptr.add(u_new) as usize;
                            let es = *s0.add(u_old) as usize;
                            let ee = *s0.add(u_old + 1) as usize;
                            for ei in es..ee {
                                #[cfg(target_arch = "x86_64")]
                                if ei + 8 < ee {
                                    let pv = (*e0.add(ei + 8) >> 11) as usize;
                                    _mm_prefetch(nid.add(pv) as *const i8, _MM_HINT_T0);
                                }
                                let e = *e0.add(ei);
                                let v_new = *nid.add((e >> 11) as usize);
                                let wt = e & 0x7FF;
                                *adj_ptr.add(dst) = (v_new << 11) | wt;
                                if wt == 0x7FF {
                                    let tw = lookup_overflow(of0_ref, ei as u32);
                                    fof_vec.push((dst as u32, tw));
                                }
                                dst += 1;
                            }
                        }
                        bar.wait();

                        // Combine final overflow on thread 0
                        if tid == 0 {
                            let final_target = &mut *(final_of_ptr as *mut Vec<(u32, u64)>);
                            for &p in fof_ptrs_ref {
                                let v = &*( p as *const Vec<(u32, u64)> );
                                final_target.extend_from_slice(v);
                            }
                            final_target.sort_unstable_by_key(|&(idx, _)| idx);
                        }
                    })
                    .unwrap();
            }
        });

        // Drop graph.txt page cache immediately
        #[cfg(unix)]
        madvise(g_bytes.as_ptr() as *mut u8, g_bytes.len(), 4);

        // Free build-only buffers
        free_anon(edges0_p as *mut u8, e0_bytes);
        free_anon(start0_p as *mut u8, s0_bytes);
        free_anon(vis_p as *mut u8, vis_bytes);

        // Parse and filter queries upfront
        let q_bytes = map_file("/data/queries.txt");
        let mut qi = 0;
        let mut active_queries: Vec<(u32, u32, u32)> = Vec::with_capacity(10000);
        let mut answers: Vec<i64> = Vec::with_capacity(10000);
        let mut num_queries = 0;

        while qi < q_bytes.len() {
            while qi < q_bytes.len() && *q_bytes.get_unchecked(qi) < b'0' { qi += 1; }
            if qi >= q_bytes.len() { break; }
            let s = parse_u32(q_bytes, &mut qi) as usize;
            let t = parse_u32(q_bytes, &mut qi) as usize;
            let orig_idx = num_queries as u32;
            num_queries += 1;
            answers.push(-1);
            if s == t {
                answers[orig_idx as usize] = 0;
            } else if ((*has_in_p.add(t >> 6)).load(Relaxed) & (1u64 << (t & 63))) != 0 {
                let s_new = *new_id_p.add(s);
                let t_new = *new_id_p.add(t);
                if *first_p.add(s_new as usize + 1) > *first_p.add(s_new as usize) {
                    active_queries.push((s_new, t_new, orig_idx));
                }
            }
            while qi < q_bytes.len() && *q_bytes.get_unchecked(qi) != b'\n' { qi += 1; }
            qi += 1;
        }

        // Free new_id and has_in before query execution
        free_anon(new_id_p as *mut u8, nid_bytes);
        free_anon(has_in_p as *mut u8, has_in_bytes);

        #[cfg(unix)]
        malloc_trim(0);

        active_queries.sort_unstable_by_key(|&(s_new, _, _)| s_new);

        // Execute queries with 8 worker threads
        const QT: usize = 8;
        let total_active = active_queries.len();
        let share = |t: usize| t * total_active / QT;
        let next: Vec<AtomicUsize> = (0..QT).map(|t| AtomicUsize::new(share(t))).collect();
        let ans_ptr = answers.as_mut_ptr() as usize;

        std::thread::scope(|sc| {
            for me in 0..QT {
                let (next, active_queries) = (&next, &active_queries);
                let first_slice = core::slice::from_raw_parts(first_p, n + 2);
                let adj_slice = core::slice::from_raw_parts(adj_p, m);
                let ov_slice = &final_overflow;
                std::thread::Builder::new()
                    .stack_size(256 * 1024)
                    .spawn_scoped(sc, move || {
                        let mut w = Worker {
                            dist: vec![0u16; n],
                            wide: Vec::new(),
                            touched: Vec::with_capacity(65536),
                            heap: PoolDial::new(),
                        };
                        let ans_out = ans_ptr as *mut i64;
                        for t in (me..QT).chain(0..me) {
                            loop {
                                let j = next[t].fetch_add(1, Relaxed);
                                if j >= share(t + 1) { break; }
                                let (s_new, t_new, orig_idx) = active_queries[j];
                                let ans = search_fast(first_slice, adj_slice, ov_slice, s_new, t_new, &mut w);
                                *ans_out.add(orig_idx as usize) = ans;
                            }
                        }
                    })
                    .unwrap();
            }
        });

        // Fast integer serialization
        let mut out = Vec::with_capacity(answers.len() * 10);
        let mut tmp = [0u8; 24];
        for &d in &answers {
            if d < 0 {
                out.extend_from_slice(b"-1\n");
            } else if d == 0 {
                out.extend_from_slice(b"0\n");
            } else {
                let mut x = d as u64;
                let mut k = 24;
                while x > 0 {
                    k -= 1;
                    tmp[k] = b'0' + (x % 10) as u8;
                    x /= 10;
                }
                out.extend_from_slice(&tmp[k..24]);
                out.push(b'\n');
            }
        }
        let mut f_out = File::create("/out/distances.txt").expect("create");
        f_out.write_all(&out).expect("write");
        drop(f_out);

        #[cfg(unix)]
        _exit(0);
    }
}
