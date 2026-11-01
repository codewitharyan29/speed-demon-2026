#![allow(unsafe_op_in_unsafe_fn)]
use core::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

#[cfg(unix)]
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn _exit(status: i32) -> !;
}

#[cfg(unix)]
unsafe fn map_file(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open");
    let len = f.metadata().unwrap().len() as usize;
    use std::os::unix::io::AsRawFd;
    let ptr = mmap(core::ptr::null_mut(), len, 1, 1, f.as_raw_fd(), 0);
    madvise(ptr, len, 2);
    core::slice::from_raw_parts(ptr, len)
}

#[cfg(not(unix))]
unsafe fn map_file(path: &str) -> &'static [u8] {
    Box::leak(std::fs::read(path).expect("read").into_boxed_slice())
}

unsafe fn huge_vec(len: usize) -> Vec<u32> {
    let mut v: Vec<u32> = Vec::with_capacity(len);
    #[cfg(unix)]
    madvise(v.as_mut_ptr() as *mut u8, len * 4, 14);
    v.set_len(len);
    v
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
    while i < b.len() && *b.get_unchecked(i) != b'\n' {
        i += 1;
    }
    i + 1
}

#[cold]
fn lookup_overflow(ov: &[(u32, u64)], idx: u32) -> u64 {
    ov.binary_search_by_key(&idx, |&(i, _)| i).map_or(2047, |p| ov[p].1)
}

#[inline(always)]
unsafe fn solve_fast(
    s: usize,
    t: usize,
    first: &[u32],
    adj: &[u32],
    dist: &mut [u16],
    heads: &mut [u32; 4096],
    touched: &mut [u32],
    pool: &mut [u64],
    ov: &[(u32, u64)],
) -> i64 {
    if s == t { return 0; }
    let (mut t_len, mut p_len, mut d, mut max_d, mut ans) = (1usize, 1usize, 0usize, 0usize, -1i64);
    *touched.get_unchecked_mut(0) = s as u32;
    *dist.get_unchecked_mut(s) = 0xFFFF;
    *pool.get_unchecked_mut(0) = ((u32::MAX as u64) << 32) | (s as u64);
    *heads.get_unchecked_mut(0) = 0;
    'search: loop {
        while *heads.get_unchecked(d & 4095) == u32::MAX {
            if d >= max_d { break 'search; }
            d += 1;
        }
        let b = d & 4095;
        let inv_d = !(d as u16);
        while *heads.get_unchecked(b) != u32::MAX {
            let mut idx = *heads.get_unchecked(b);
            *heads.get_unchecked_mut(b) = u32::MAX;
            while idx != u32::MAX {
                let entry = *pool.get_unchecked(idx as usize);
                let u = (entry & 0xFFFF_FFFF) as usize;
                idx = (entry >> 32) as u32;
                if u == t { ans = d as i64; break 'search; }
                if *dist.get_unchecked(u) != inv_d { continue; }
                if idx != u32::MAX {
                    let nu = (*pool.get_unchecked(idx as usize) & 0xFFFF_FFFF) as usize;
                    _mm_prefetch(adj.as_ptr().add(*first.get_unchecked(nu) as usize) as *const i8, _MM_HINT_T0);
                }
                let mut i = *first.get_unchecked(u) as usize;
                let end = *first.get_unchecked(u + 1) as usize;
                while i < end {
                    let e = *adj.get_unchecked(i);
                    let v = (e >> 11) as usize;
                    let mut wt = (e & 0x7FF) as u16;
                    if wt == 0x7FF {
                        let tw = lookup_overflow(ov, i as u32);
                        if tw >= 60000 {
                            ans = -2;
                            break 'search;
                        }
                        wt = tw as u16;
                    }
                    i += 1;
                    let inv_nd = inv_d.wrapping_sub(wt);
                    let cur = *dist.get_unchecked(v);
                    if inv_nd > cur {
                        let nd = d + (wt as usize);
                        if nd >= 63000 || p_len >= 65000 || t_len >= 65000 {
                            ans = -2;
                            break 'search;
                        }
                        *dist.get_unchecked_mut(v) = inv_nd;
                        *touched.get_unchecked_mut(t_len) = v as u32;
                        t_len += (cur == 0) as usize;
                        let nb = nd & 4095;
                        let old_h = *heads.get_unchecked(nb);
                        *heads.get_unchecked_mut(nb) = p_len as u32;
                        *pool.get_unchecked_mut(p_len) = ((old_h as u64) << 32) | (v as u64);
                        p_len += 1;
                        if nd > max_d { max_d = nd; }
                        _mm_prefetch(first.as_ptr().add(v) as *const i8, _MM_HINT_T0);
                    }
                }
            }
        }
        d += 1;
    }
    for i in 0..t_len {
        *dist.get_unchecked_mut(*touched.get_unchecked(i) as usize) = 0;
    }
    for b_idx in d..(d + 4096).min(max_d + 1) {
        *heads.get_unchecked_mut(b_idx & 4095) = u32::MAX;
    }
    ans
}

#[cold]
unsafe fn solve_fallback(s: usize, t: usize, n: usize, first: &[u32], adj: &[u32], ov: &[(u32, u64)]) -> i64 {
    let mut dist = vec![u64::MAX; n];
    let mut pq = BinaryHeap::new();
    dist[s] = 0;
    pq.push(Reverse((0u64, s as u32)));
    while let Some(Reverse((d, u32_id))) = pq.pop() {
        let u = u32_id as usize;
        if u == t { return d as i64; }
        if d > dist[u] { continue; }
        for i in (*first.get_unchecked(u) as usize)..(*first.get_unchecked(u + 1) as usize) {
            let e = *adj.get_unchecked(i);
            let v = (e >> 11) as usize;
            let wt = if (e & 0x7FF) == 0x7FF { lookup_overflow(ov, i as u32) } else { (e & 0x7FF) as u64 };
            let nd = d + wt;
            if nd < dist[v] {
                dist[v] = nd;
                pq.push(Reverse((nd, v as u32)));
            }
        }
    }
    -1
}

fn main() {
    unsafe {
        let g_bytes = map_file("/data/graph.txt");
        let q_bytes = map_file("/data/queries.txt");
        let mut pos = 0;
        let n = parse_u32(g_bytes, &mut pos) as usize;
        let m = parse_u32(g_bytes, &mut pos) as usize;
        let body = &g_bytes[pos..];

        const T: usize = 12;
        const QT: usize = 8;
        let mut bounds = [0usize; T + 1];
        bounds[T] = body.len();
        for t in 1..T {
            let mut p = (body.len() * t) / T;
            while p < body.len() && *body.get_unchecked(p) != b'\n' { p += 1; }
            bounds[t] = (p + 1).min(body.len());
        }

        let mut first = huge_vec(n + 2);
        let mut adj = huge_vec(m);

        let ws_u32_per_thread = ((n + 1) / 2) + 4096 + 65536 + 131072;
        let arena_len = (2 * n + 2 + m + (n + 31) / 32).max(QT * ws_u32_per_thread);
        let mut arena = huge_vec(arena_len);

        let start0_ptr = arena.as_mut_ptr() as *const AtomicU32;
        let edges0_ptr = arena.as_mut_ptr().add(n + 2);
        let new_id_ptr = edges0_ptr.add(m);
        let vis_ptr = new_id_ptr.add(n) as *const AtomicU32;

        let mut queries: Vec<(u32, u32, u32)> = Vec::with_capacity(10000);
        let mut qi = 0;
        while qi < q_bytes.len() {
            while qi < q_bytes.len() && *q_bytes.get_unchecked(qi) < b'0' { qi += 1; }
            if qi >= q_bytes.len() { break; }
            let a = parse_u32(q_bytes, &mut qi);
            let b = parse_u32(q_bytes, &mut qi);
            queries.push((a, b, queries.len() as u32));
            while qi < q_bytes.len() && *q_bytes.get_unchecked(qi) != b'\n' { qi += 1; }
            qi += 1;
        }

        let q_len = queries.len();
        let mut results = vec![0i64; q_len];
        let mut t_sums = [0u32; T + 1];
        let bfs_seed = AtomicUsize::new(0);
        let bfs_rank = AtomicUsize::new(0);
        let next_q = AtomicUsize::new(0);

        let s0_addr = start0_ptr as usize;
        let e0_addr = edges0_ptr as usize;
        let nid_addr = new_id_ptr as usize;
        let vis_addr = vis_ptr as usize;
        let f_addr = first.as_mut_ptr() as usize;
        let adj_addr = adj.as_mut_ptr() as usize;
        let arena_addr = arena.as_mut_ptr() as usize;
        let ts_addr = t_sums.as_mut_ptr() as usize;
        let q_addr = queries.as_mut_ptr() as usize;
        let res_addr = results.as_mut_ptr() as usize;

        let barrier = std::sync::Barrier::new(T);
        let mut of0_threads: Vec<Vec<(u32, u64)>> = (0..T).map(|_| Vec::new()).collect();
        let mut final_of_threads: Vec<Vec<(u32, u64)>> = (0..T).map(|_| Vec::new()).collect();
        let of0_ptrs: Vec<usize> = of0_threads.iter_mut().map(|v| v as *mut _ as usize).collect();
        let f_of_ptrs: Vec<usize> = final_of_threads.iter_mut().map(|v| v as *mut _ as usize).collect();

        let mut of0_all: Vec<(u32, u64)> = Vec::new();
        let mut final_overflow: Vec<(u32, u64)> = Vec::new();
        let of0_all_ptr = &mut of0_all as *mut Vec<(u32, u64)> as usize;
        let final_of_ptr = &mut final_overflow as *mut Vec<(u32, u64)> as usize;

        let (of_ptrs_ref, fof_ptrs_ref) = (&of0_ptrs, &f_of_ptrs);

        std::thread::scope(|s| {
            for tid in 0..T {
                let bar = &barrier;
                let seed_ref = &bfs_seed;
                let rank_ref = &bfs_rank;
                let nq_ref = &next_q;
                let of0_p = of_ptrs_ref[tid];
                let fof_p = fof_ptrs_ref[tid];

                s.spawn(move || {
                    let s0 = s0_addr as *mut u32;
                    let s0_at = s0_addr as *const AtomicU32;
                    let e0 = e0_addr as *mut u32;
                    let nid = nid_addr as *mut u32;
                    let vis = vis_addr as *const AtomicU32;
                    let f_ptr = f_addr as *mut u32;
                    let adj_p = adj_addr as *mut u32;
                    let ts = ts_addr as *mut u32;

                    // Zero start0, vis, and first
                    let v_start = ((n + 2) * tid) / T;
                    let v_end = ((n + 2) * (tid + 1)) / T;
                    core::ptr::write_bytes(s0.add(v_start), 0, v_end - v_start);
                    core::ptr::write_bytes(f_ptr.add(v_start), 0, v_end - v_start);
                    if tid == 0 {
                        core::ptr::write_bytes(vis as *mut u32, 0, (n + 31) / 32);
                    }
                    bar.wait();

                    // Pass 1: Parse only u and skip line
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

                    // Pass 2: Parse u, v, w into edges0
                    let mut i = 0;
                    let of0_vec = &mut *(of0_p as *mut Vec<(u32, u64)>);
                    while i < chunk.len() {
                        while i < chunk.len() && *chunk.get_unchecked(i) < b'0' { i += 1; }
                        if i >= chunk.len() { break; }
                        let u = parse_u32(chunk, &mut i) as usize;
                        let v = parse_u32(chunk, &mut i);
                        let w = parse_u32(chunk, &mut i) as u64;
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
                            let mask = 1u32 << (root & 31);
                            if ((*vis.add(root >> 5)).load(Relaxed) & mask) != 0 { continue; }
                            if ((*vis.add(root >> 5)).fetch_or(mask, Relaxed) & mask) != 0 { continue; }
                            let (mut head, mut tail) = (0usize, 1usize);
                            q_buf[0] = root as u32;
                            while head < tail && tail < 8192 {
                                let u = q_buf[head] as usize;
                                head += 1;
                                for ei in (*s0.add(u) as usize)..(*s0.add(u + 1) as usize) {
                                    let v = (*e0.add(ei) >> 11) as usize;
                                    let vm = 1u32 << (v & 31);
                                    if ((*vis.add(v >> 5)).load(Relaxed) & vm) == 0
                                        && ((*vis.add(v >> 5)).fetch_or(vm, Relaxed) & vm) == 0
                                    {
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
                            if ei + 8 < ee {
                                let pv = (*e0.add(ei + 8) >> 11) as usize;
                                _mm_prefetch(nid.add(pv) as *const i8, _MM_HINT_T0);
                            }
                            let e = *e0.add(ei);
                            let v_new = *nid.add((e >> 11) as usize);
                            let wt = e & 0x7FF;
                            *adj_p.add(dst) = (v_new << 11) | wt;
                            if wt == 0x7FF {
                                let tw = lookup_overflow(of0_ref, ei as u32);
                                fof_vec.push((dst as u32, tw));
                            }
                            dst += 1;
                        }
                    }
                    bar.wait();

                    // Combine final overflow and remap queries on thread 0
                    if tid == 0 {
                        let final_target = &mut *(final_of_ptr as *mut Vec<(u32, u64)>);
                        for &p in fof_ptrs_ref {
                            let v = &*( p as *const Vec<(u32, u64)> );
                            final_target.extend_from_slice(v);
                        }
                        final_target.sort_unstable_by_key(|&(idx, _)| idx);

                        let q_slice = core::slice::from_raw_parts_mut(q_addr as *mut (u32, u32, u32), q_len);
                        for q in q_slice.iter_mut() {
                            q.0 = *nid.add(q.0 as usize);
                            q.1 = *nid.add(q.1 as usize);
                        }
                        q_slice.sort_unstable_by_key(|q| q.0);
                    }
                    bar.wait();

                    // Setup Phase 2 workspace slice in reused arena (QT threads)
                    let is_worker = tid < QT;
                    let (mut dist_slice, mut touched_slice, mut pool_slice) = (None, None, None);
                    let mut heads_ptr = core::ptr::null_mut();
                    if is_worker {
                        let my_ws = (arena_addr as *mut u32).add(tid * ws_u32_per_thread);
                        let dist_u32_len = (n + 1) / 2;
                        core::ptr::write_bytes(my_ws, 0, dist_u32_len);
                        heads_ptr = my_ws.add(dist_u32_len) as *mut [u32; 4096];
                        (*heads_ptr).fill(u32::MAX);
                        let touched_ptr = my_ws.add(dist_u32_len + 4096);
                        let pool_ptr = touched_ptr.add(65536) as *mut u64;
                        dist_slice = Some(core::slice::from_raw_parts_mut(my_ws as *mut u16, n));
                        touched_slice = Some(core::slice::from_raw_parts_mut(touched_ptr, 65536));
                        pool_slice = Some(core::slice::from_raw_parts_mut(pool_ptr, 65536));
                    }
                    bar.wait();

                    // Step 7: Queries
                    if is_worker {
                        let f_slice = core::slice::from_raw_parts(f_ptr as *const u32, n + 1);
                        let adj_slice = core::slice::from_raw_parts(adj_p as *const u32, m);
                        let dist_slice = dist_slice.unwrap();
                        let touched_slice = touched_slice.unwrap();
                        let pool_slice = pool_slice.unwrap();
                        let q_slice = core::slice::from_raw_parts(q_addr as *const (u32, u32, u32), q_len);
                        let res_p = res_addr as *mut i64;
                        let ov_slice = &*(final_of_ptr as *const Vec<(u32, u64)>);

                        loop {
                            let qb = nq_ref.fetch_add(8, Relaxed);
                            if qb >= q_len { break; }
                            let qe = (qb + 8).min(q_len);
                            for qi in qb..qe {
                                let (qs, qt, orig_idx) = *q_slice.get_unchecked(qi);
                                let mut d = solve_fast(
                                    qs as usize,
                                    qt as usize,
                                    f_slice,
                                    adj_slice,
                                    dist_slice,
                                    &mut *heads_ptr,
                                    touched_slice,
                                    pool_slice,
                                    ov_slice,
                                );
                                if d == -2 {
                                    d = solve_fallback(qs as usize, qt as usize, n, f_slice, adj_slice, ov_slice);
                                }
                                *res_p.add(orig_idx as usize) = d;
                            }
                        }
                    }
                });
            }
        });

        // Fast output serialization
        let mut out = Vec::with_capacity(q_len * 10);
        let mut tmp = [0u8; 24];
        for &d in &results {
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

        #[cfg(unix)]
        _exit(0);
    }
}
