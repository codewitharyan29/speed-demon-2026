use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

/// Maps a file read-only. The mapping is never unmapped, so the slice is 'static.
fn map(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    // SAFETY: PROT_READ (1), MAP_PRIVATE (2) on a file we just opened; the length is the file's.
    unsafe { std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len) }
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

fn main() {
    let g = map("/data/graph.txt");
    let mut p = 0;
    let n = num(g, &mut p) as usize;
    let m = num(g, &mut p) as usize;
    let mut edges = Vec::with_capacity(m);
    let mut start = vec![0u32; n + 2];
    let mut has_in = vec![false; n];
    for _ in 0..m {
        let (u, v, w) = (num(g, &mut p) as u32, num(g, &mut p) as u32, num(g, &mut p) as u32);
        if u != v {
            edges.push((u, v, w));
            start[u as usize + 2] += 1;
            has_in[v as usize] = true;
        }
    }
    // Counting sort by tail: after the prefix sum, start[u + 1] is where u's edges begin; placing advances it
    // to their end, so afterwards u's edges are start[u]..start[u + 1].
    for i in 2..n + 2 {
        start[i] += start[i - 1];
    }
    let mut adj = vec![(0u32, 0u32); edges.len()];
    for &(u, v, w) in &edges {
        let slot = &mut start[u as usize + 1];
        adj[*slot as usize] = (v, w);
        *slot += 1;
    }

    let q = map("/data/queries.txt");
    let mut p = 0;
    let queries: Vec<_> = (0..q.iter().filter(|&&c| c == b'\n').count())
        .map(|_| (num(q, &mut p) as u32, num(q, &mut p) as u32))
        .collect();

    let next = AtomicUsize::new(0);
    let mut answers = vec![-1i64; queries.len()];
    let parts: Vec<Vec<(usize, i64)>> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..12)
            .map(|_| {
                sc.spawn(|| {
                    let mut dist = vec![u64::MAX; n];
                    let mut touched = Vec::new();
                    let mut heap = BinaryHeap::new();
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= queries.len() {
                            return done;
                        }
                        let (s, t) = queries[i];
                        let a = if s == t {
                            0
                        } else if !has_in[t as usize] {
                            -1
                        } else {
                            search(&start, &adj, s, t, &mut dist, &mut touched, &mut heap)
                        };
                        done.push((i, a));
                    }
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    for (i, a) in parts.into_iter().flatten() {
        answers[i] = a;
    }

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
/// `dist` is all-MAX on entry and restored on exit via `touched`, so a query costs only its own search.
fn search(
    start: &[u32],
    adj: &[(u32, u32)],
    s: u32,
    t: u32,
    dist: &mut [u64],
    touched: &mut Vec<u32>,
    heap: &mut BinaryHeap<Reverse<(u64, u32)>>,
) -> i64 {
    dist[s as usize] = 0;
    touched.push(s);
    heap.push(Reverse((0, s)));
    let mut ans = -1;
    while let Some(Reverse((d, u))) = heap.pop() {
        if d > dist[u as usize] {
            continue;
        }
        if u == t {
            ans = d as i64;
            break;
        }
        for &(v, w) in &adj[start[u as usize] as usize..start[u as usize + 1] as usize] {
            let nd = d + w as u64;
            if nd < dist[v as usize] {
                if dist[v as usize] == u64::MAX {
                    touched.push(v);
                }
                dist[v as usize] = nd;
                heap.push(Reverse((nd, v)));
            }
        }
    }
    heap.clear();
    for v in touched.drain(..) {
        dist[v as usize] = u64::MAX;
    }
    ans
}
