# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
`PERF.md` has every step with its board run, and the experiments that did not work.

## Approach

**Parse.** `graph.txt` is mapped with `mmap` and cut into 48 newline-aligned chunks that 12 threads parse at once.
Numbers are read eight bytes at a time (SWAR): the first byte below `'0'` ends a number, and three multiplications turn
up to eight digits into an integer. Each chunk's edge list is sized from the chunk's length, so it never reallocates.
While parsing, each edge bumps an atomic out-degree for its tail and marks its head as having an in-edge; self-loops
are dropped.

**First CSR.** A prefix sum turns the degrees into offsets, and the degree array is reused as the write cursor: edges
are scattered into place with `fetch_add`, each packed as `head << 32 | weight`.

**Renumbering.** Node ids are a random relabelling of a road lattice, so a search over the original ids misses the
cache on every neighbour. Each thread repeatedly takes an unclaimed node and grows a breadth-first region of up to
32k nodes around it. Regions are numbered one after another; with their offsets known, every region writes its new
ids, slots and degrees in parallel. The CSR is then rebuilt in the new numbering, so a search walks memory that is
mostly contiguous. The final edge array is zero-copy transmuted from `Vec<AtomicU64>` to `Vec<u64>` to unblock LLVM
optimisations in the search loop.

**Queries.** Each query is a forward Dijkstra from `s` that stops when `t` is popped, with Dial's buckets: a
ring of 2^14 buckets, key k in bucket k % 2^14, popped exactly in key order by scanning forward. Main-body weights
are at most a few thousand, so every key in the queue lies within the window; the rare keys beyond it (edges of
near-maximal weight) wait in a binary heap. A thread's distance array holds distance + 1 as `u32` (0 means unseen): it is
zero-filled lazily by the kernel and restored through a list of touched nodes, so a query costs only its own search.
Four bytes per node halve the memory 12 threads keep resident; the few searches whose distances pass 2^32 (they cross
edges of near-maximal weight) run again with `u64`, through the same generic search. `s == t` and targets without
in-edges are answered without searching. Queries run in order of their source's new id, each thread working through
its own contiguous share before helping the others, so a thread's searches reuse the same part of the graph in cache.

**Hot-loop micro-optimisations.** The relaxation inner loop uses `get_unchecked` throughout to eliminate bounds checks
that LLVM cannot prove redundant (indices come from runtime graph data). Software prefetch (`_mm_prefetch`) issues
batch prefetches for the first four neighbours' distance entries before the loop, then a sliding-window prefetch four
edges ahead during the loop, hiding L3 latency on the random `dist[v]` reads that the cachegrind profile identified as
the main L1 misses. The relaxation test uses bitwise OR (`(seen == 0) | (nd + 1 < seen)`) instead of short-circuit OR
to give the branch predictor a single branch instead of two, targeting the main mispredicted branch from the profile.

Answers are written in input order with one `write`.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`. `_mm_prefetch` is from `std::arch::x86_64`.

## With another 24 hours

- A cheaper build: the scatter, the region growth and the rebuild still read and write the edge list three times;
  a parallel radix sort by source could replace the atomic scatter.
- A leaner search kernel: goal-directed search with cheap landmarks (ALT) if the build cost can be amortised.
- Profile-guided optimisation with sampling PGO (AutoFDO) instead of instrumentation, which paradoxically slowed
  the previous build.
