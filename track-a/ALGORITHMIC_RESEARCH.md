# Track A: algorithmic research (branch `exp/a-algo`; A1 untouched)

Method: no counters exist on the board CPU, so this is a model built from the input's measured distribution and from the board and
VM measurements already in `PERF.md`, plus one new benchmark on the real group-size/latency distribution. Nothing here changes
`src/main.rs`; A1 (`89573db`, best run `run-40f68e04b5c6`, T 23.71 ms, M 18.35 MB, S 508, K 43.71) is the control.

Scale of K: K = (32.05 s / T)^0.5 x (227.98 MB / M)^0.2 x (56 / S)^0.15. K = 50 at M 18.35 MB, S 508 needs **T = 18.2 ms**.

## 1. Quantitative model of A1 (seed 1, `tools/a_stats.py`)

| Quantity | Value |
|---|---|
| lines / bytes | 3,100,000 / 238,836,234 (77.0 bytes per line) |
| endpoints / hours / populated groups | 1,842 / 72 / 126,908 |
| endpoint length | mean 33.7, max 125 |
| latency digits | 4: 47.6%, 5: 36.1%, 6: 9.7%, 3: 5.1%, 7: 1.2%, the rest under 0.1% (1.2% need the second record word) |
| lines in an hour behind the newest hour seen | 13.1% |
| group sizes | 1: 8.8% of groups; 2: 10.0%; 3-8: 43.8%; 9-32: 28.0%; 33-128: 7.1%; 129-1024: 2.1%; over 1024: 0.3% (331 groups, up to 9,223) |
| records by group size | 1-8: 10.4%; 9-32: 17.6%; 33-128: 17.3%; 129-1024: 25.9%; over 1024: 28.8% |

Work per line, from the code (`scan`, `summarise`):

| Step | Count per line | Total |
|---|---|---|
| bytes read from the page cache (`pread`, 32 KiB blocks, L1/L2-resident buffer) | 77 | 239 MB, once |
| AVX2 compares for spaces | 2-4 loads of 32 bytes | about 7 M |
| endpoint hash (4 AES blocks + 3 more for endpoints over 64 bytes) | 1 | 3.1 M |
| dictionary probes (hash table of 4,096 slots = 48 KB, L1/L2-resident) | 1 | 3.1 M |
| latency conversions (8-digit SWAR) | 1 | 3.1 M |
| record bytes written at parse | 4 (8 for 1.2%) | 12.6 MB |
| record bytes read by the histogram pass, then by the scatter pass | 4 + 4 | 25 MB read |
| record bytes written by the scatter, read by the selection | 4 + 4 | 25 MB |
| comparisons for p50/p95/p99 | insertion sort up to 32, chained `select_nth` above (about 4.5 n) | about 10 M for the 72% of records in groups over 32, plus about 8 M for the rest |
| output | 10 MB through one lock, in hour order | 127 k lines |
| allocations | one output `Vec` per hour, scratch buffers reused | about 100 |

DRAM traffic is dominated by the input: 239 MB read once. Everything else (12.6 MB records, 12.6 MB sorted values, 10 MB output,
48 KB dictionary) is small or cache-resident. The board's read-only run (`d1110d4`, run-a13d2d78189f) took 8.42 ms for startup plus
the 239 MB, i.e. about 28 GB/s: that is the memory-bandwidth floor of this design, whatever the compute does.

Compute budget on the board: T 23.7 ms - 8.4 ms floor = about 15 ms, which `PERF.md` splits as parse about 10, summaries about 6.7,
final write about 1. For K = 50 the compute would have to fall from 15 to 9.8 ms (-35%).

Single-thread CPU split of A1 (VM, from PERF.md round 3): `pread` 32 ms, scan 77, histogram 8.5, scatter 8, percentiles about 20 (25.4 ms in the new
benchmark below, on a different CPU), formatting about 23, `madvise` 3: about 190 ms.

## 2. Parsing: is there redundant work?

A1 inspects each byte of a line at most once: two 32-byte compares find the spaces (method, endpoint, status, latency), the 8 bytes
"MM-DDTHH" are read once per change of timestamp key, the endpoint is read by the hash only, the status by one byte, the latency by
one 8-byte load. There is no second pass over the line and no copy. What is left is the hash (about 7 AES rounds, pipelined across
lines) and the probe, which the earlier ablation put at 40% of the parse CPU when it compared keys and at roughly half of that now.

| Idea | Work removed | Verdict |
|---|---|---|
| fuse the histogram into the parse (count per (chunk, hour, endpoint) while parsing) | one 4-byte read per record in L2 (about 8.5 ms of 190 single-thread) | adds the same increment to the parse and a 530 KB table per worker (72 x 1,842 x 4 bytes) that does not fit L1; no net gain expected |
| cheaper hash (first and last 16 bytes) | about 10 instructions per line | not allowed: A1's hash must cover the whole endpoint because it is the only identity check; endpoints share long heads and tails (the stress inputs are built that way) |
| skip the second word for long latencies | 1.2% of records | already minimal |
| process two lines at once | latency of the probe | tested before this round (see PERF.md): 93.0 vs 88.5 ms; the dictionary never misses the cache |

## 3. Percentile selection (new benchmark, `tools/a_pctl_bench.rs`)

Real distribution: the 126,908 groups of seed 1 with their real latencies in input order (`tools/a_stats.py` writes them). Exact
nearest-rank values; all variants return identical answers. Best of 7 passes, single thread, includes the copy of each group:

| Variant | ms | ns per record |
|---|---:|---:|
| **current: insertion sort up to 32, chained `select_nth_unstable` above** | **25.4** | **8.20** |
| insertion up to 16 | 25.5 | 8.23 |
| chain everywhere from n = 3 | 26.2 | 8.45 |
| insertion up to 8 | 26.5 | 8.55 |
| insertion up to 64 | 26.7 | 8.60 |
| branch-free n^2 ranking up to 8, insertion up to 32 | 31.1 | 10.03 |
| branch-free n^2 ranking up to 16, insertion up to 32 | 32.7 | 10.55 |
| `sort_unstable` of every group | 39.7 | 12.81 |

The current algorithm and threshold are the best of those tried, and the whole step is 8 ns per record, about 13% of single-thread CPU.
Even a perfect 2x here would be about 6% of CPU and about 1.4 ms of T (+3% K), below board noise. Histogram or radix selection was
not built: 63% of groups have at most 8 records and only 0.3% have more than 1,024, so the group sizes that dominate record count
(9 to 1,024) are too small for a 256-bucket or 65,536-bucket pass to beat `select_nth` (a 2,048-record group costs about 9,000 comparisons, less than one pass
over 65,536 counters).

## 4. Alternative architectures

| Architecture | What changes | Estimate |
|---|---|---|
| A1 (control) | parse to per-(chunk, hour) regions, per-hour counting sort, select, format | T 23.7 |
| Two passes: count, then parse straight into final positions | removes histogram and scatter (16.5 ms of 190 single-thread CPU) but reads and parses the input twice (+77 ms scan, +32 ms `pread`) | much worse |
| Per-worker aggregation into per-(hour, endpoint) latency vectors | removes the scatter; needs a merge across 12 workers, vectors grow by reallocation | measured worse: the minimum-line HashMap rewrite (`b19bc7a`) was 43 ms |
| Streaming histogram of latencies per group (fixed buckets) | exact percentiles need every value; exact bucket boundaries need a second pass | not exact in one pass |
| mmap the input | removes the copy, adds page-table work | rejected on the board three times |
| Smaller M: write output as it is produced | the board charges the 10 MB output to the cgroup however it is written | rejected on the board (O_DIRECT, fadvise) |

No architecture above beats A1 on both T and M. The bound argument is the strongest result: the 239 MB read costs 8.4 ms of 23.7 and
is not reducible by any algorithm that reads the input once, so K = 50 needs the 15 ms of compute to fall to 9.8 ms. The single largest
term (the scan, 77 of 190 ms of single-thread CPU) is already one pass with no copies.

## 5. Experiments in this round

| # | Hypothesis | Result | Decision |
|---|---|---|---|
| 1 | a different selection algorithm or threshold is faster on the real group sizes | table in section 3: current is best, ranking and sorting variants 22-56% slower | rejected |
| 2 | two-line interleaving hides dictionary latency (previous round) | 93.0 vs 88.5 ms | rejected |
| 3 | two-digit table for numbers (previous round) | single-thread user CPU 110 vs 101 ms, 8-thread equal | rejected |
| 4 | exact key comparison (`track-a2`) | 80.5 vs 77.5 ms median, S +23 | not adopted; exact but slower |

## 6. Three highest-value remaining opportunities

1. **A cheaper but still complete endpoint identity.** The hash plus probe is still the largest single part of the scan. A scheme that
   identifies the endpoint from the bytes the scan already loaded (the 64-byte window used for the space compares) with fewer rounds,
   plus an exact check only on a tag mismatch, might cut the scan by 10-15%. Needs a board run to see: the VM cannot rank a gain this small.
2. **Fewer bytes per line moved by the kernel.** The 8.4 ms floor is the page-cache copy at about 28 GB/s. A read path with less
   copying (a populated mapping of huge pages for the file, if the board's page cache has them) is the only way to move the floor; the three
   mmap variants measured so far paid more in page-table work than they saved, so this is a hypothesis about page size, not a result.
3. **Startup and teardown.** Process start to first `pread` and the exit after the last write are inside the 8.4 ms floor; a leaner startup
   (no `Mutex`/`Vec` setup before the first read, spawn all 12 workers before the dictionary and arena are touched) could be worth about 1 ms.

Each of these is worth under 5% of K, and K = 50 needs +14%. I found no algorithm that gets there; the remaining distance is the sum of
several small parts, none of which a single experiment can reach without the board's own counters.
