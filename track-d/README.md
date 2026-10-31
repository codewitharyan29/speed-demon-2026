# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
`PERF.md` has every step with its board run, and the experiments that did not work.

## Approach

**L3-resident 20 MB packed CSR.** On the scoring CPU (Intel Core i5-13500 with 24 MB shared L3 cache), the standard 8-byte edge CSR (40 MB) spills out of cache, throttling 12 hardware threads with DRAM stalls.
99.93% of edge weights are < 2048, and node IDs fit in 21 bits (< 2,097,152).
Each edge is packed into a single 4-byte `u32`: `(target << 11) | weight`.
This slashes the CSR edge array from 40 MB to **19.07 MB**, fitting the entire graph resident inside the CPU L3 cache.
For the 0.07% of edges with weight ≥ 2047 (3,480 edges in 5M), a sentinel `0x7FF` is stored, and the exact weight is looked up in a tiny sorted `overflow` array via a `#[cold]` function.

**Zero-atomic in-memory renumbering & build.**
`graph.txt` is mapped with `mmap` and cut into 48 newline-aligned chunks parsed across 12 threads with an unsafe SWAR parser (8 bytes unaligned, 3 multiplications).
Pass 1 counts out-degrees and in-degree presence without tuple buffers.
Unrenumbered edges are stored in `edges0: Vec<AtomicU64>`, and the mapped text file is immediately released.
BFS region growing (spatial clusters of 32,768 nodes) computes cache-friendly `new_id` mappings.
Then, instead of re-reading and re-parsing 88 MB of text a 3rd time, each thread processes an exclusive range of nodes and packs `edges0` directly into `adj: Vec<AtomicU32>`.
Because each thread writes to disjoint slices of `adj`, no atomic fetch-adds on write cursors are performed.
`edges0` and original offsets are dropped before search begins, keeping peak build memory at ~67 MB.

**2-Level summary pooled Dial queue (`PoolDial`).**
Dial's 16,384 `Vec` heap containers are replaced with a flat linked-list memory pool (`head: [u32; 16384]` = 64 KB, `pool_node: Vec<u32>`, `pool_next: Vec<u32>`), eliminating all heap reallocations during Dijkstra.
A 2-level hierarchical bitset (256 `u64` words + 4 `u64` summary words) enables finding the next non-empty bucket in 2 CPU cycles using `tzcnt` (`trailing_zeros()`).
Only touched buckets are cleared on query completion.

**Lean 32-bit relaxation kernel.**
Distances are initialized to `u32::MAX` and cleared via `touched`.
Normal searches use pure 32-bit integer addition. The relaxation condition simplifies to a single comparison `if nd < cur_d` (2 instructions).
Software prefetch (`_mm_prefetch T0`) pulls neighbor distance entries into L1 cache.
Sources with zero out-degree and targets with no in-edges return immediately in O(1).
Answers are buffered and written to `/out/distances.txt`.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`. `_mm_prefetch` is from `std::arch::x86_64`.
