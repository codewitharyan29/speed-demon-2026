// usage: rustc --edition 2021 -O -C target-cpu=native a_pctl_bench.rs && ./a_pctl_bench groups.bin   (groups.bin from tools/a_stats.py)
// Times exact nearest-rank p50/p95/p99 over the real group-size and latency distribution of a Track A log, per algorithm.
use std::time::Instant;

fn at(n: usize, p: usize) -> usize {
    (p * n + 99) / 100 - 1
}

fn insertion(v: &mut [u32]) -> (u32, u32, u32) {
    let n = v.len();
    for i in 1..n {
        let mut j = i;
        while j > 0 && v[j - 1] > v[j] {
            v.swap(j - 1, j);
            j -= 1;
        }
    }
    (v[at(n, 50)], v[at(n, 95)], v[at(n, 99)])
}

fn chain(v: &mut [u32]) -> (u32, u32, u32) {
    let (k1, k2, k3) = (at(v.len(), 50), at(v.len(), 95), at(v.len(), 99));
    let (_, p50, upper) = v.select_nth_unstable(k1);
    let p50 = *p50;
    if k2 == k1 {
        return (p50, p50, p50);
    }
    let (_, p95, upper) = upper.select_nth_unstable(k2 - k1 - 1);
    let p95 = *p95;
    if k3 == k2 {
        return (p50, p95, p95);
    }
    (p50, p95, *upper.select_nth_unstable(k3 - k2 - 1).1)
}

/// n^2 branch-free ranking: the element whose (value, index) rank is k is the k-th smallest.
fn ranks(v: &mut [u32]) -> (u32, u32, u32) {
    let n = v.len();
    let want = [at(n, 50), at(n, 95), at(n, 99)];
    let mut out = [0u32; 3];
    for i in 0..n {
        let mut r = 0;
        for j in 0..n {
            r += (v[j] < v[i] || (v[j] == v[i] && j < i)) as usize;
        }
        for k in 0..3 {
            if r == want[k] {
                out[k] = v[i];
            }
        }
    }
    (out[0], out[1], out[2])
}

fn sorted(v: &mut [u32]) -> (u32, u32, u32) {
    v.sort_unstable();
    let n = v.len();
    (v[at(n, 50)], v[at(n, 95)], v[at(n, 99)])
}

fn main() {
    let bytes = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let words: Vec<u32> = bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let (mut groups, mut i) = (vec![], 0);
    while i < words.len() {
        let n = words[i] as usize;
        groups.push(&words[i + 1..i + 1 + n]);
        i += 1 + n;
    }
    let total: usize = groups.iter().map(|g| g.len()).sum();
    println!("{} groups, {} records", groups.len(), total);
    let mut scratch = vec![0u32; groups.iter().map(|g| g.len()).max().unwrap()];
    type F = fn(&mut [u32]) -> (u32, u32, u32);
    let variants: Vec<(&str, Box<dyn Fn(&mut [u32]) -> (u32, u32, u32)>)> = vec![
        ("current: insertion <=32, chain above", Box::new(|v| if v.len() <= 32 { insertion(v) } else { chain(v) })),
        ("insertion <=16, chain above", Box::new(|v| if v.len() <= 16 { insertion(v) } else { chain(v) })),
        ("insertion <=8, chain above", Box::new(|v| if v.len() <= 8 { insertion(v) } else { chain(v) })),
        ("insertion <=64, chain above", Box::new(|v| if v.len() <= 64 { insertion(v) } else { chain(v) })),
        ("ranks <=8, insertion <=32, chain above", Box::new(|v| if v.len() <= 8 { ranks(v) } else if v.len() <= 32 { insertion(v) } else { chain(v) })),
        ("ranks <=16, insertion <=32, chain above", Box::new(|v| if v.len() <= 16 { ranks(v) } else if v.len() <= 32 { insertion(v) } else { chain(v) })),
        ("sort_unstable everywhere", Box::new(|v| sorted(v))),
        ("chain everywhere (n >= 3)", Box::new(|v| if v.len() < 3 { insertion(v) } else { chain(v) })),
    ];
    let _ = std::marker::PhantomData::<F>;
    let mut reference = None;
    for (name, f) in &variants {
        let mut best = f64::MAX;
        let mut sum = 0u64;
        for _ in 0..7 {
            sum = 0;
            let t = Instant::now();
            for g in &groups {
                let s = &mut scratch[..g.len()];
                s.copy_from_slice(g);
                let (a, b, c) = f(s);
                sum = sum.wrapping_add(a as u64 * 3 + b as u64 * 5 + c as u64 * 7);
            }
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
        }
        let ok = *reference.get_or_insert(sum) == sum;
        println!("{:<44} {:7.2} ms  {:5.2} ns/record  {}", name, best, best * 1e6 / total as f64, if ok { "same answers" } else { "DIFFERENT ANSWERS" });
    }
}
