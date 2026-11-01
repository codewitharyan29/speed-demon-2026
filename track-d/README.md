# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
`PERF.md` has every step with its board run, and the experiments that did not work.

## Approach

**L3-resident 20 MB packed CSR.** On the scoring CPU (Intel Core i5-13500 with 24 MB shared L3 cache), the standard 8-byte edge CSR (40 MB) spills out of cache, throttling 12 hardware threads with DRAM stalls.
99.93% of edge weights are < 2048, and node IDs fit in 21 bits (< 2,097,152).
Each edge is packed into a single 4-byte `u32`: `(target << 11) | weight`.
This slashes the CSR edge array from 40 MB to **19.07 MB**, fitting the entire graph resident inside the CPU L3 cache.
For the 0.07% of edges with weight ≥ 2047 (3,480 edges in 5M), a sentinel `0x7FF` is stored, and the exact weight is looked up in a tiny sorted `overflow` array via a `#[cold]` function.

**Hybrid AVX2 Triplet Delimiter Parser.**
`graph.txt` is mapped with `mmap` and split into 48 newline-aligned chunks across 12 threads.
32-byte vector loads (`_mm256_loadu_si256`) and delimiter comparisons (`_mm256_cmpgt_epi8` with `0x30` + `_mm256_movemask_epi8`) identify delimiter offsets ($u$, $v$, $w$) via BMI1 `blsr` and `tzcnt` in parallel.
Decouples address calculation from arithmetic, bypassing Golden Cove Port 1 oversubscription and Port 5 vector shuffle contention. Parsing is 31.2% faster than scalar SWAR.

**Two-Pass Streaming Build (Peak Memory < 43 MB).**
Pass 1 counts degrees, populates a 218 KB `has_in` bitset, and builds a compact 20 MB unweighted `adj0: Vec<AtomicU32>`.
BFS region clustering (spatial regions of 32,768 nodes) runs directly on `adj0`.
Both `adj0` and `start0` are dropped before Pass 2, reducing live heap to ~15.7 MB.
Pass 2 streams `graph.txt` directly into final packed `adj: Vec<AtomicU32>` (20 MB) in ~8 ms.
Zero-copy transmute (`Vec::from_raw_parts`) avoids duplicate buffer allocations.

**Demand-Zero Inverted Distance Kernel (`!d`).**
Distances are stored as `inv_d = !d = u32::MAX - d`. Unvisited nodes are `0`.
Distance arrays are allocated via `vec![0u32; n]` (`alloc_zeroed`), mapping directly to the Linux kernel's demand-zero `empty_zero_page` without faulting pages into physical RAM.
Only touched nodes (~1.5 MB per thread) are faulted into memory, slashing query physical RAM from 84 MB to ~15 MB across all 12 threads.
Relaxation simplifies to a single branchless comparison `if inv_nd > dist[v]` (1 instruction).
Distances are restored to `0` via `touched.drain(..)`.

**16 KB L1D-Resident `PoolDial` (`WINDOW = 4096`).**
The circular bucket ring is sized to $2^{12} = 4,096$ buckets, dropping `head` from 64 KB to **16 KB** to fit 100% inside each core's L1D cache.
The multi-level summary table is compressed into **a single 64-bit integer (`u64`)**.
The next non-empty bucket is found in 2 cycles via hardware `summary.rotate_right(...)` + `tzcnt`. Zero loops, zero branches, zero allocations.

**Batch Upfront Prefetching & Direct Write.**
Outgoing edge neighbor distances are prefetched upfront into Golden Cove's 32 Line Fill Buffers (LFBs) at node expansion.
Query worker threads write answers directly into `answers[orig_idx]`, eliminating intermediate thread vectors and post-join sorting.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`. AVX2 and prefetch intrinsics are from `std::arch::x86_64`.
