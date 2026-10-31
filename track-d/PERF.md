# Track D: performance log

Board runs are identified by the commit the board measured (the board lists runs by commit). Local numbers are the
in-process time of the full seed-1 dataset on a 12-CPU Docker container (`tools/ab.py`, interleaved runs, median of 9).
They are noisy in absolute terms (a Windows laptop VM, not the scoring box) and are only used to compare variants.

## Board runs

| Step | Change | Run (commit) | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | Baseline: sequential parse, CSR, early-exit Dijkstra with a binary heap, 12 threads over queries | fdc4472 | 1.310 | 289 | 44.275 (K 7.68, 158 lines) |
| 2 | Parallel parse and CSR, parallel region-growing renumbering, radix heap, lazy `dist`, `MAP_POPULATE` | bdfba23 | 0.399 | 272 | 55.446 (K 12.85, 292 lines) |
| 3 | Drop `MAP_POPULATE`; `-C target-cpu=native` | 5f206c6 | 0.389 | 275 | 55.695 (K 13.00, 289 lines) |
| 4 | The degree array is reused as the write cursor and dropped after the scatter | eb8a2d1 | 0.391 | 258 | 55.925 (K 13.14, 288 lines) |
| 5 | 4-byte distances with an 8-byte fallback, locality-ordered scheduling, pre-sized and SWAR-parsed edge lines | 90838c8 | 0.347 | 188 | 58.168 (K 14.57, 327 lines) |
| 6 | Region numbering, new ids and new offsets written in parallel; Dial's buckets instead of the radix heap | pending | | | |

## Local measurements behind step 2 (full data, in-process ms, 12 CPUs)

| Variant | Median | Verdict |
|---|---|---|
| Baseline (step 1) | ~2260 | Docker wall time, bind-mounted input, so not comparable with the rows below |
| Sequential BFS renumbering | ~1570 wall vs ~2260 baseline wall | kept: ids are a random relabelling of a lattice, so ordering by BFS makes a search touch contiguous memory |
| Parallel parse, counting sort with atomics | 732 | kept |
| Radix heap instead of `BinaryHeap` | search phase ~300 vs ~380 | kept |
| Lazy zero-filled `dist` (stores distance + 1) | 668 vs 732 with the two changes above; `memory.peak` 299 -> 244 (includes ~92 MB of input page cache locally) | kept: untouched pages cost no memory |
| `MAP_POPULATE` on the input mmaps | 710 vs 736 | kept: one token |
| `-C target-cpu=native` | 705 vs 736 | within noise, not adopted yet; re-test with PGO |
| Parallel region-growing renumbering, 2^15 nodes per region | 546 vs 715 | kept: the sequential BFS was 150 ms of the run |
| Region size 2^12 / 2^15 / 2^18 | 533 / 531 / 538 | insensitive |

## Phase split now (ms, local)

parse + counting sort ~98, region growing ~38, new ids ~13, CSR rebuild ~46, searches ~330.

## Not tried yet

- Bidirectional Dijkstra: would roughly halve settled nodes but needs a reverse CSR (+40 MB, +~45 ms to build) and a
  second `dist`; the estimated net gain is small against the memory and line-count cost. Needs a measurement.
- PGO (last, because any later edit invalidates the profile), `get_unchecked` in the search loop, query ordering.

## Notes from board run bdfba23

- T fell 1.310 -> 0.399 s (3.3x) while lines went 158 -> 292: the time term gained far more than the size term lost.
- M barely moved (289 -> 272 MB) although the per-thread `dist` arrays went from eager to lazy (-55 MB locally).
  The board's memory.peak therefore seems to include the input mmap pages (92 MB graph.txt), probably because of
  `MAP_POPULATE` faulting them in. Next experiment: drop `MAP_POPULATE`, or read the input in pieces, and compare M.

## Experiments after step 3 (local, in-process ms median, full data; not yet on the board)

| Variant | Result | Verdict |
|---|---|---|
| PGO: instrumented build and run on the full seed-1 data in the pinned rustc 1.99.1 image, `-Cprofile-use`, profile regenerated for the exact source measured | 639 -> 728 (+14%) | rejected: slower, with no profile-mismatch warning. Not investigated further; `tools/pgo.sh` reproduces it |
| `-C target-cpu=native` through `.cargo/config.toml` (the scoring box builds the track itself) | 659 -> 642 | adopted: small but consistent in both minimum and median, two lines |

## Notes from board run 5f206c6 and later local work

- Dropping `MAP_POPULATE` did not change M (272 -> 275 MB, inside noise of the other change), so the board does not
  charge the pre-faulted input pages; that guess from run bdfba23 was wrong. T 0.399 -> 0.389 s is about what the
  local test of `target-cpu=native` predicted (-2.6%).
- Sorting the queries by the new id of their source (so the 12 threads search neighbouring areas at once): 598 vs 597 ms
  median, no effect on this machine; reverted.
- Resident memory by phase (local, includes the ~92 MB mapped input): 156 MB after parsing, 208 after the scatter,
  219 after the CSR rebuild (peak), 194 after the searches. The peak is the rebuild, where both `adj0` and `adj` are
  live; the per-thread `dist` arrays are smaller than estimated. Reusing the degree array as the write cursor and
  dropping it after the scatter took local `memory.peak` 258 -> 245 MB and one line off the source.
- Bidirectional search, costed not built: it would roughly halve settled nodes (search ~330 of ~600 local ms) but needs a
  reverse CSR (+40 MB, ~45 ms) and a second `dist` per thread; the estimated net is about break-even on the score.

## Step 5: memory first, then the build (local, in-process ms, median of 11-15, another session benchmarking on the same CPUs)

The board after step 4 put us 34th on this track: the top ten run in 0.12-0.20 s with 46-112 MB, we were at 0.391 s and
258 MB. Where the memory was: resident memory per phase showed ~85 MB of heap after the searches, but that was after the
per-thread `dist` arrays were freed. During the searches each of the 12 threads holds a 14 MB array that, over ~170
queries, ends up fully resident (167 MB). The build phase alone peaks at 134 MB (parsed edge list + first CSR).

| Change | Result | Verdict |
|---|---|---|
| `dist` from its own anonymous mapping, handed back with `madvise(MADV_DONTNEED)` after every search (it reads as zero again, which also replaces the reset loop) | `memory.peak` 245 -> 115 MB, but 377 -> 575 ms | reverted: page faults and TLB shootdowns across 12 threads cost far too much |
| The same only after searches touching over 2^14 / 2^16 nodes | 117 / 149 MB at 576 / 504 ms | reverted |
| `dist` as `u32` (distance + 1), the search generic over the entry type; a search whose distances pass 2^32 (those crossing near-maximal-weight edges) runs again with `u64`; parse buffers sized from the chunk length so they never double | `memory.peak` 245 -> 170 MB; 423 -> 370 ms (noisy) | kept |
| 8 / 6 threads instead of 12 | 355 / 460 ms vs 336; 139 / 124 MB vs 170 | rejected: on the score about even at 8 and worse at 6 |
| Queries sorted by their source's new id; each thread first works through its own contiguous share, then helps the others | 315 -> 298 ms; memory unchanged (a thread's large searches still cover most of its array) | kept: better cache reuse of the graph |
| Transparent huge pages (`MADV_HUGEPAGE`) for the CSR and `dist` | 296 -> 298 ms; 108 MB were in huge pages | reverted: TLB misses are not the cost |
| SWAR parsing of edge lines: eight bytes at a time, the first byte below '0' ends a number, three multiplications convert it (fast_float's method); longer numbers take the old loop | 303 -> 300 (min 280 -> 269) | kept |
| Bidirectional Dijkstra (reverse CSR built in parallel; the side with the smaller top key grows; stop when the tops add up to the best meeting) | 295 -> 446 ms; searches 154 -> 278 ms; 357 MB | reverted: on this road-like graph each relaxation also reads the other side's distance, an extra random miss per edge, which costs more than halving the settled nodes saves |

Profile of step 5 on the sample (cachegrind): number parsing was 26% of all instructions before the SWAR change; in the
searches the relax test (`seen == 0 || nd + 1 < seen`) is the main mispredicted branch and the `dist` loads the main
L1 misses.

## After step 5: bidirectional search, second try (research suggestion: interleaved distances)

| Change | Result | Verdict |
|---|---|---|
| Bidirectional Dijkstra with the forward and backward distance of a node side by side (`[u32; 2]`), so meeting the other side costs no extra cache miss | 320 -> 526 ms, `memory.peak` 315 MB | reverted |

Counting the work explains it: bidirectional search pops 15.4 M nodes and relaxes 41.6 M edges, against 24.9 M and
67.5 M for the forward search (38% less work), yet each relaxation costs ~98 ns against ~27 ns. The forward search
on the renumbered graph stays inside one 40 MB, cache-friendly adjacency; the backward half walks a second 40 MB
reverse graph, and the per-node state doubles, which together go far past the 24 MB L3. Other research suggestions
not taken: a partition overlay (CRP/MLD) needs a Dijkstra from every boundary node of every cell, 10^8-10^9 settled
nodes of preprocessing against ~24 M for all queries; a timestamped distance array shared by all threads is not
correct when 12 threads run different queries at once.

## Step 6: the sequential part of the renumbering

Phase times (warm, local): flattening the nested region lists 9-10 ms, writing new ids 5 ms, the new offsets 13-15 ms,
all on one thread. With a prefix sum over region sizes, every region knows where its nodes go and writes its new ids,
order slots and degrees in parallel; only the prefix sum of degrees stays sequential. Numbering 27 -> 14 ms
(end-to-end medians too noisy to resolve while the other session benchmarks). Verified on tiny seeds 1-6, sample
seeds 1-3, full seeds 1-2 and the worked example.

## Step 6 continued: Dial's buckets (research item "faster monotone priority queue", study guide "Dial's buckets")

The cachegrind profile put the radix heap's pushes and bucket redistribution at about 30% of the search's
instructions. Main-body weights are at most a few thousand, so the keys in a search's queue lie within a small window
above the key last popped.

| Change | Result | Verdict |
|---|---|---|
| Dial's buckets: a ring of 2^14 buckets, key k in bucket k % 2^14, popped exactly in order by scanning forward; keys beyond the window (edges of near-maximal weight) in the radix heap | 324 -> 294 ms | kept |
| The far keys in std's `BinaryHeap` instead of the radix heap, which is then removed (-43 lines) | 297 -> 305, then 307 vs 307 on a re-run | kept: same speed, 364 instead of 399 lines |
| Window of 2^16 buckets | 307 -> 328 ms | rejected: clearing a larger ring per query costs more |
| Per-thread busy time in the search phase (instrumented) | 175-186 ms of 189 | the threads are balanced; the cost is the kernel itself |
