# Track C: the road to K > 40, and why it starts with board diagnostics

Status, Nov 1 14:56 UTC (freeze Nov 2 15:00 UTC): best official K is 29.76 (`9fb5c93`, run-ec41ba8a226a). No new official result
in this session.

## What K > 40 requires (organisers' `harness/scoring.py`, verified)

K = (14.44 s / T)^0.5 x min(367 MB / M, 50)^0.2 x min(107 / S, 1)^0.15 x (1 / D)^0.15. Check: T 36.09, M 13.3, S 656 gives
29.59 (the board said 29.63).

| Target K | T needed at M 13.5 MB, S 629 | T needed at M 10 MB, S 450 |
|---:|---:|---:|
| 32 | 31.1 ms | 38.7 ms |
| 35 | 26.0 ms | 32.4 ms |
| 38 | 22.0 ms | 27.5 ms |
| 40 | **19.9 ms** | **24.8 ms** |

K 40 needs T cut by about 45% at today's M and S, or by about 32% together with a 3.5 MB smaller M and 180 fewer lines.

## Why another rewrite chosen by local timings is not the next step

R2 cut 14% of the instructions and ran 8% faster in the local VM (4 vCPUs, no SMT). On the board it ran at T 36.09 ms against
35.73-36.73 for the control: **no gain**. Two conclusions follow:

1. The board's time is not dominated by instruction count in the parse and compare code. The other candidates are memory
   latency, coherence traffic on the shared name table (12 SMT threads on 6 cores), the kernel copy, or serial and fixed phases.
2. The local machine cannot rank ideas for the board. The only measurement that can is a board run, and this session has no
   access to the board: it builds only `speed-demon`, which this session must not push unverified code to, and which another
   session is actively using (`46829d6`, `c5b7898`).

## The local split (control `9fb5c93`, 4 threads, full seed 1, 20 interleaved runs)

| Diagnostic (patch in `docs/track-c-board-diagnostics/`, against `9fb5c93`) | What it removes | Local median | Increment |
|---|---|---:|---:|
| `d4_fixed` | everything after the region table, name table and chunk starts | 4.7 ms | fixed 4.7 |
| `d5_read` | parsing: every block is read with `pread`, nothing else | 15.9 ms | read 11.2 |
| `tc6_noresolve` | lookups, region and totals (parse and prefetches stay) | 41.5 ms | parse 25.6 |
| `tc6_nolookup` | the name lookup and mask write | 44.0 ms | region and totals 2.5 |
| `tc6_nomask` | the locked mask OR only | 65.5 ms | name lookup 21.5 |
| control | | 69.4 ms | mask write 3.9 |

## The proposed board diagnostics (five runs; each prints wrong output on purpose, as the earlier labelled diagnostics in PERF.md did)

Apply one patch to `9fb5c93`'s `track-c/src/main.rs`, push to `speed-demon`, read T and M, then restore with
`git checkout 9fb5c93 -- track-c/src/main.rs`. The order is by information value:

1. `tc6_nolookup`: how much of the board's 36 ms is the shared name table.
2. `tc6_noresolve`: parse-only time on the board.
3. `d5_read`: the read floor on the board.
4. `tc6_nomask`: the cost of the locked write and its coherence traffic.
5. `d4_fixed`: today's fixed cost (it was 7.8 ms in an old diagnostic, before the SWAR regions parser).

## What each outcome would lead to (research matrix)

| If the board shows | Bottleneck | Candidate design | Expected effect | Memory and S | Risk |
|---|---|---|---|---|---|
| name lookup (1) is 15 ms or more of 36 | shared 8 MB table, cross-core lines | partition names by hash across the 12 workers: each owns about 0.7 MB of entries (fits a P-core's L2, private, no locked OR); records exchanged in bounded rounds | lookups toward the private-L2 cost (locally a private table saved 12% CPU at 4 threads; the effect grows with sharers) | +1-2 MB of exchange buffers, about +100 lines | medium: round imbalance, termination |
| mask write (4) is 5 ms or more | coherence on the written lines | same partitioning, or masks in a separate per-owner array | up to the measured cost | small | low |
| parse-only (2) is 15 ms or more and read (3) much less | SMT throughput of stage 1 and 2 | not more instruction cuts (R2 showed they do not transfer); fewer bytes touched: stage 1 only for quotes in the note, a scalar scan from the note's end | uncertain | about 0 | medium |
| read (3) is 10 ms or more | kernel copy | `pread` of larger blocks into a reused buffer (fewer syscalls); `mmap` was rejected before | 1-3 ms at most | about 0 | low |
| fixed (5) is 5 ms or more | serial setup | one thread pool for all phases; smaller region pass | 1-2 ms | -1 MB if the region table is compacted | low |

Memory is the other lever: 13.5 MB to 10 MB alone is worth +6% K. The 8 MB entry table is the largest item. A design with 32-byte
entries (mask only), names in a separate key array touched only on a tag hit, would cut it to about 4 MB plus about 3 MB of keys.
Whether board M follows RSS is itself unverified (an empty process already measures 8.1 MB there).

## Research notes (what was consulted, and the limits)

speeddemon.dev, and with it any public competitor information, is not reachable through this environment's proxy. Competitor
repositories become public only at code freeze. The design ideas above come from known techniques: SwissTable/F14-style tag
filtering (already used: 16 tags compared at once), partitioned aggregation as in hash-partitioned group-by (radix partitioning
before aggregation, as in DuckDB and the cache-conscious hash-join literature), and simdjson-style structural indexing (already
used: carry-less multiply for the quote state). None of these is new to this code base except the partitioning, and the board
diagnostics above decide whether it is worth its 100 lines.

## Honest assessment

No measured result from this session supports K > 40, and none of the candidate designs comes with evidence of a 45% cut. The
largest single board-relevant unknown is the cost of the shared name table under 12 SMT threads. Five board runs would measure it
and the other parts; the partitioned design is the one to build if (1) and (4) together are a large share of the 36 ms.
