use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

const THREADS: usize = 12;
const CHUNKS: usize = 48;

/// Token bytes: lower-cased letters and digits map to themselves, every other byte (including all >= 0x80) to 0.
static LOWER: [u8; 256] = {
    let mut t = [0; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        t[i] = match c {
            b'a'..=b'z' | b'0'..=b'9' => c,
            b'A'..=b'Z' => c + 32,
            _ => 0,
        };
        i += 1;
    }
    t
};

/// Maps a file read-only. The mapping is never unmapped, so the slice is 'static.
fn map(path: &str) -> &'static [u8] {
    let f = File::open(path).expect("open input");
    let len = f.metadata().unwrap().len() as usize;
    // SAFETY: PROT_READ (1), MAP_PRIVATE (2) on a file we just opened; the length is the file's.
    unsafe {
        std::slice::from_raw_parts(mmap(std::ptr::null_mut(), len, 1, 2, f.as_raw_fd(), 0), len)
    }
}

/// Runs `f(state, job)` for every job in 0..jobs on THREADS threads that pull jobs from one shared counter,
/// so uneven jobs balance themselves. `init` builds each thread's private state. Results come back in job order.
fn par<S, R: Send>(
    jobs: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut all: Vec<(usize, R)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    let (mut state, mut got) = (init(), Vec::new());
                    loop {
                        let j = next.fetch_add(1, Relaxed);
                        if j >= jobs {
                            return got;
                        }
                        got.push((j, f(&mut state, j)));
                    }
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

/// Fx-style hash step over one byte; the table index uses the high bits, which this mixes best.
fn step(h: u64, b: u8) -> u64 {
    (h.rotate_left(5) ^ b as u64).wrapping_mul(0x517cc1b727220a95)
}

/// Every distinct term the queries mention, in an open-addressing table (slot = word index + 1, 0 = empty).
struct Terms {
    words: Vec<&'static [u8]>,
    slots: Vec<u32>,
    bits: u32,
}

impl Terms {
    /// The word's id, or the empty slot where it would go.
    fn find(&self, h: u64, w: &[u8]) -> Result<u32, usize> {
        let mask = self.slots.len() - 1;
        let mut i = (h >> (64 - self.bits)) as usize;
        loop {
            match self.slots[i] {
                0 => return Err(i),
                s if self.words[s as usize - 1] == w => return Ok(s - 1),
                _ => i = (i + 1) & mask,
            }
        }
    }

    fn intern(&mut self, w: &'static [u8]) -> u32 {
        match self.find(w.iter().fold(0, |h, &b| step(h, b)), w) {
            Ok(id) => id,
            Err(slot) => {
                self.words.push(w);
                self.slots[slot] = self.words.len() as u32;
                self.words.len() as u32 - 1
            }
        }
    }
}

/// A parsed query: term ids, and the operator byte (b'A', b'O' or b'N') between each neighbouring pair.
struct Query {
    phrase: bool,
    terms: Vec<u32>,
    ops: Vec<u8>,
}

/// Merges sorted `a` with sorted `b` under `op`: AND keeps ids in both, OR in either, NOT in `a` only.
fn combine(op: u8, a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            if op != b'A' {
                out.push(a[i]);
            }
            i += 1;
        } else if a[i] > b[j] {
            if op == b'O' {
                out.push(b[j]);
            }
            j += 1;
        } else {
            if op != b'N' {
                out.push(a[i]);
            }
            i += 1;
            j += 1;
        }
    }
    if op != b'A' {
        out.extend_from_slice(&a[i..]);
    }
    if op == b'O' {
        out.extend_from_slice(&b[j..]);
    }
}

/// True if the tokens of `text` contain `words` at consecutive positions.
fn has_phrase(text: &[u8], words: &[&[u8]], spans: &mut Vec<(usize, usize)>) -> bool {
    spans.clear();
    let mut i = 0;
    while i < text.len() {
        if LOWER[text[i] as usize] == 0 {
            i += 1;
            continue;
        }
        let from = i;
        while i < text.len() && LOWER[text[i] as usize] != 0 {
            i += 1;
        }
        spans.push((from, i));
    }
    spans.windows(words.len()).any(|win| {
        win.iter().zip(words).all(|(&(a, b), w)| {
            b - a == w.len()
                && text[a..b]
                    .iter()
                    .zip(w.iter())
                    .all(|(&c, &d)| LOWER[c as usize] == d)
        })
    })
}

fn put(out: &mut Vec<u8>, mut x: u64) {
    let mut buf = [0u8; 20];
    let mut i = 20;
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

fn main() {
    // Intern every term of every query.
    let q = map("/data/queries.txt");
    let tokens = q.iter().filter(|&&c| c == b' ' || c == b'\n').count();
    let bits = (tokens * 2).next_power_of_two().trailing_zeros();
    let mut terms = Terms {
        words: Vec::new(),
        slots: vec![0; 1 << bits],
        bits,
    };
    let queries: Vec<Query> = q
        .split(|&c| c == b'\n')
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.split(|&c| c == b' ');
            let phrase = parts.next().unwrap() == b"PHRASE";
            let (mut ids, mut ops) = (Vec::new(), Vec::new());
            for (k, w) in parts.enumerate() {
                if !phrase && k % 2 == 1 {
                    ops.push(w[0]);
                } else {
                    let w = w.strip_prefix(b"\"").unwrap_or(w);
                    ids.push(terms.intern(w.strip_suffix(b"\"").unwrap_or(w)));
                }
            }
            Query {
                phrase,
                terms: ids,
                ops,
            }
        })
        .collect();
    let nterms = terms.words.len();

    // Scan the corpus in newline-aligned chunks. A chunk yields (term, doc) pairs for the query terms only,
    // each once per document, in document order, plus the byte offset of each of its documents.
    let docs = map("/data/docs.txt");
    let cut = |i: usize| match i {
        0 => 0,
        CHUNKS => docs.len(),
        _ => {
            let at = docs.len() * i / CHUNKS - 1;
            at + docs[at..].iter().position(|&c| c == b'\n').unwrap() + 1
        }
    };
    let scanned = par(
        CHUNKS,
        || vec![0u32; nterms],
        |last, c| {
            let (mut i, end) = (cut(c), cut(c + 1));
            let (mut pairs, mut offs) = (Vec::new(), Vec::new());
            while i < end {
                offs.push(i as u32);
                let mut id = 0u32;
                while docs[i] != b'\t' {
                    id = id * 10 + (docs[i] - b'0') as u32;
                    i += 1;
                }
                i += 1;
                while docs[i] != b'\n' {
                    if LOWER[docs[i] as usize] == 0 {
                        i += 1;
                        continue;
                    }
                    let (mut h, mut word, mut len) = (0, [0u8; 32], 0);
                    while LOWER[docs[i] as usize] != 0 {
                        let b = LOWER[docs[i] as usize];
                        h = step(h, b);
                        if len < 32 {
                            word[len] = b;
                        }
                        len += 1;
                        i += 1;
                    }
                    if len <= 32 {
                        if let Ok(t) = terms.find(h, &word[..len]) {
                            if last[t as usize] != id + 1 {
                                last[t as usize] = id + 1;
                                pairs.push((t, id));
                            }
                        }
                    }
                }
                i += 1;
            }
            (pairs, offs)
        },
    );

    // Posting lists in one array. Per chunk and term we know the count, so each chunk scatters into its own
    // disjoint slots and every list comes out sorted without a sort.
    let mut place = par(
        CHUNKS,
        || (),
        |_, c| {
            let mut row = vec![0u32; nterms];
            for &(t, _) in &scanned[c].0 {
                row[t as usize] += 1;
            }
            row
        },
    );
    let mut start = vec![0u32; nterms + 1];
    for t in 0..nterms {
        let mut at = start[t];
        for row in place.iter_mut() {
            (row[t], at) = (at, at + row[t]);
        }
        start[t + 1] = at;
    }
    let post: Vec<AtomicU32> = (0..start[nterms]).map(|_| AtomicU32::new(0)).collect();
    par(
        CHUNKS,
        || (),
        |_, c| {
            let mut at = place[c].clone();
            for &(t, d) in &scanned[c].0 {
                post[at[t as usize] as usize].store(d, Relaxed);
                at[t as usize] += 1;
            }
        },
    );
    // SAFETY: AtomicU32 has the layout of u32, and the threads that wrote it have all been joined.
    let post: &[u32] =
        unsafe { std::slice::from_raw_parts(post.as_ptr() as *const u32, post.len()) };
    let doc_off: Vec<u32> = scanned.iter().flat_map(|s| s.1.iter().copied()).collect();
    let list = |t: u32| &post[start[t as usize] as usize..start[t as usize + 1] as usize];

    // Answer the queries, 64 per job.
    let lines = par(
        queries.len().div_ceil(64),
        || (Vec::new(), Vec::new(), Vec::new()),
        |(cur, tmp, spans), j| {
            let mut out = Vec::new();
            for query in &queries[j * 64..((j + 1) * 64).min(queries.len())] {
                cur.clear();
                if query.phrase {
                    let mut order = query.terms.clone();
                    order.sort_by_key(|&t| list(t).len());
                    cur.extend_from_slice(list(order[0]));
                    for &t in &order[1..] {
                        tmp.clear();
                        combine(b'A', cur, list(t), tmp);
                        std::mem::swap(cur, tmp);
                    }
                    let words: Vec<&[u8]> = query
                        .terms
                        .iter()
                        .map(|&t| terms.words[t as usize])
                        .collect();
                    cur.retain(|&d| {
                        let text = &docs[doc_off[d as usize] as usize..];
                        let tab = text.iter().position(|&c| c == b'\t').unwrap();
                        let len = text.iter().position(|&c| c == b'\n').unwrap();
                        has_phrase(&text[tab + 1..len], &words, spans)
                    });
                } else {
                    cur.extend_from_slice(list(query.terms[0]));
                    for (k, &op) in query.ops.iter().enumerate() {
                        tmp.clear();
                        combine(op, cur, list(query.terms[k + 1]), tmp);
                        std::mem::swap(cur, tmp);
                    }
                }
                put(&mut out, cur.len() as u64);
                out.push(b' ');
                put(&mut out, cur.iter().map(|&d| d as u64).sum());
                for &d in cur.iter().take(10) {
                    out.push(b' ');
                    put(&mut out, d as u64);
                }
                out.push(b'\n');
            }
            out
        },
    );
    std::fs::write("/out/results.txt", lines.concat()).expect("write output");
}
