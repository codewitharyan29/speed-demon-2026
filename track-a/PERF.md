# Track A: performance log

Board runs are identified by the run id the board shows. **Local numbers are not board numbers.** They come from a
Docker VM on a laptop (Ryzen 5 3500U, 4 cores / 8 threads, WSL2, ~700 MB/s page-cache writes, no PMU) with 8 worker
threads, `tools/hyper.sh a` (hyperfine, 40 runs, `taskset -c 0-7`) on the full seed-1 data from a Docker volume. They
only rank variants against each other; page faults, syscalls and file writes are inflated compared with the board box.
Noise is about +-10 ms, so A/B pairs are timed in one hyperfine session (`BIN=<old binary> tools/hyper.sh a 40`).

## Board runs

| Step | Change | Run | T (ms) | M (MB) | S | K | Points |
|---|---|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32050 | 228 | | 1.0 | 0 |
| 1 | Rust: mmap, per-chunk `HashMap<(hour, endpoint), Vec<u32>>`, merge and sort per hour-of-day shard | run-659d7a9038a2 | 132.2 | 100.1 | 170 | 15.54 | 59.57 |
| 2 | Rewrite: shared lock-free endpoint dictionary, 6-byte records per (chunk, hour), per-hour counting sort, `pread` input | run-ac6fd7774fa8 (9360f49) | 33.9 | 195.3 | 454 | 23.17 | 68.25 |
| 3 | Step 2 plus `MADV_NOHUGEPAGE` on the arena, hour-major arena layout, `MADV_DONTNEED` of each hour's records once summarised | run-a243658265aa (9c5ca97) | 26.4 | 30.3 | 461 | 38.05 | 79.02 |

Leader when step 2 was prepared (`tools/board_watch.py`): Cause Of Death, Rust, T 16.9 ms, M 18.6 MB, S 353, K 54.5.
At M 19.5 MB and S 353, K = 56 needs T <= 15.7 ms.

## Local measurements

| Variant | Result (ms, 8 threads) | Verdict |
|---|---|---|
| Step 1 baseline (per-chunk hash maps) | 360 mean; in-process: parse 150-200, merge+sort+format 135-150, reorder+copy 70-100 | replaced |
| Rewrite, input mmap'd (`v1-mmap`) | 103-117 mean, 62 in-process; M 36 MB | superseded by pread |
| Rewrite, input read with `pread` in 256 KiB blocks (E2d) | 90-99 mean, same session as the mmap build: 98.8 vs 117.3, i.e. 1.19x | **kept** |

Phase table of the first rewrite (mmap input, µs since process start, 8 threads; user/sys CPU is for the whole process):

| Phase | Time | Notes |
|---|---|---|
| input mapped, arena mapped, job built | 30, 105 | negligible |
| phase 1 done (parse, first / last worker) | 27 800 / 30 900 | user 128-150 ms CPU, sys 50-107 ms (page faults on the 239 MB mapping) |
| endpoint order sorted | +300 | one worker, the others wait |
| phase 2 done (first / last worker) | 47 300 / 62 200 | the last 15 ms is the single writer draining 10 MB through `write` |
| `munmap` of the 239 MB input mapping | +24 000 | page-table teardown; the kernel does this at exit anyway |
| `munmap` of the 2.6 GB sparse arena | +4 500 | |

The 24 ms teardown of the mapped input is what made `pread` win (E2d): it also removes the file page faults.

## Lessons from the board

- **Step 2 measured 195 MB where the same binary measured 37 MB locally.** The board kernel has transparent huge pages
  on (`always`), this VM has `madvise`. The arena is one sparse mapping with a 630 KB region per (chunk, hour), so
  every touched 2 MiB became a huge page. Reproduced locally by adding `madvise(arena, MADV_HUGEPAGE)` to the build:
  `memory.peak` 25 MB -> 197 MB. Fixed with `MADV_NOHUGEPAGE` on the arena (step 3). Local M is only trustworthy when
  the kernel's THP setting matches the board's.
- Time did transfer: 132 ms -> 33.9 ms on the board for a local 360 -> ~95 ms (about 3.5x both ways).
- Releasing each hour's records right after it is summarised (hour-major layout, one `madvise(MADV_DONTNEED)` per hour)
  lowered the local `memory.peak` from 37 MB to 27 MB: the 10 MB output file's page cache counts, and now grows into
  the memory the records give back. Time was unchanged within noise (best of 40: 83.5 vs 82.3 ms).

- Step 3 also ran faster than step 2 (26.4 vs 33.9 ms): with huge pages off the kernel no longer zeroes a 2 MiB page
  for every touched region.

## Round 2, Step 0: profile of step 3 (9c5ca97), no code change

Setup: Docker VM, 8 CPUs (`taskset -c 0-7`, 8 threads), THP set to `always` to mirror the board kernel, output file removed before
every run, 20 runs per column, medians. Instrumentation was an uncommitted copy of `main.rs` (timestamps per thread, `getrusage`,
`/proc/self/status`), `strace -f -tt -T` for the syscall timeline. No `perf` in the VM, so no cycles/instructions.

**VM caveats.** The VM runs this binary in about 98 ms against 26 ms on the board (3.7x) and has 8 CPUs, not 12. It inflates
kernel work (pread, clone3, madvise, faults, writes) much more than CPU work, so rank by user CPU and treat every sys-side
conclusion as a hypothesis until a board run confirms it. Board noise is about +-6% (same code: 26.4 and 28.0 ms), so a single
board run cannot confirm a gain under about 8%. `tools/hyper.sh` overwrote an existing `summary.txt` in every run; `close` of a
truncated file then took 7 ms under strace (ext4 flushes on close). A fresh board run probably has no such file. `tools/hyper.sh`
now removes it before each run, and `tools/ab.sh` does interleaved A/B rounds with wall/user/sys.

| Event (ms since `main`) | output on tmpfs | output on Docker volume |
|---|---|---|
| 7 worker threads spawned (serial in main) | 4.4 | 4.9 |
| first / last thread finishes parse | 51.6 / 56.2 | 52.2 / 56.5 |
| endpoint sort (one thread, the rest idle) | 0.85 | 0.97 |
| first / last non-writer thread finishes phase 2 | 88.1 / 91.5 | 90.0 / 93.6 |
| everything written and joined | 97.6 | 108.6 |
| tail after the last non-writer thread (writer draining) | 5.4 | 15.0 |

| CPU | tmpfs | volume |
|---|---|---|
| user ms | 344 | 341 |
| sys ms | 305 | 323 |
| sys share | 48% | 50% |
| minor faults | 7.9k | 8.2k |

Per thread: phase 1 busy 54.5 ms, slowest/mean 1.07 (median over runs), `pread` 21.8 ms of it (40%), barrier wait after the parse
4.4 ms mean (16 max); phase 2 `summarise` 30.5 ms for about 10 hours, `madvise(MADV_DONTNEED)` 3.9 ms (72 calls of about 0.45 ms),
the writer thread 32-42 ms in `write`. `summarise` sub-steps summed over all threads: histogram 31 ms, scatter 33 ms,
percentile selection + formatting 155-160 ms, i.e. about 46% of all user CPU. Strace: 1084 `pread`, 7 `clone3`, 315 `sched_yield`
(the spin barrier), 72 `write`.

Memory (fresh container, `memory.peak` 28.9 MB; board 30.3 MB): records 3.1M x 6 B = 18.6 MB (RssAnon after the parse is 20.6-22.6),
read buffers 8 x 256 KB = 2.1, dictionary + scratch + runtime + stacks about 2, binary and libc 1.8, kernel (slab, stacks, page
tables) 1.9, output file in the page cache 10.1 at the end (2-3 MB of it at the peak, because each hour's records are released as
its lines are written). Latencies: 1.2% of the 3.1M are at or above 2^20 (36,447), 14.6% at or above 2^16.

Ranking drawn from this: user-CPU first (percentile + formatting), then 4-byte records for M, then the serial and idle time.

## Round 2, step 1: percentile selection and formatting (local, `tools/ab.sh`, 20 interleaved rounds, medians, tmpfs output)

Output hash identical for every variant. Summed over threads the percentile step costs about 62 ms of CPU and the rest of the line
loop (formatting, key copies, the output `Vec`'s page faults) about 65 ms. Group sizes in seed 1: 63% of the 127k groups have at
most 8 records, 28% at most 32; 331 groups with more than 1024 records hold 29% of all records, 1,135 groups of 257-1024 another 17%.

| Variant | user ms | wall ms | Verdict |
|---|---|---|---|
| base (9c5ca97) | 340 (mean 331) | 99.5 | |
| `sort_unstable` of each group, then index the three ranks | 347 vs 326 for base (separate 20-round session) | 103 vs 97 | **rejected**, slower |
| 2-digit lookup table, one `reserve` per line + raw pointer writes, key stored with its trailing space (+17 lines) | 300 (mean 304) | 97 | rejected: -8% user, but +17 lines and wall unchanged |
| select the median first, then p95 inside the upper half, then p99 inside that (about 4.5n comparisons per large group instead of 7n) | 296 (mean 294) | 98 | **kept**, no extra lines |
| both | 287 (mean 291) | 98.5 | formatting adds only 1-3% user on top of the selection change |

Wall time did not move locally because the VM's wall time is bound by kernel work (pread, faults, writes), not by user CPU. The board
box spends little time in the kernel (T 26 ms x 12 threads is about the 330 ms of user CPU measured here), so the user-CPU
reduction is what the board run has to confirm.

## Rejected or inconclusive (local, 8 threads, interleaved hyperfine)

- **Input mmap'd, `MADV_POPULATE_READ` per worker chunk (E2c):** 161 vs 128 ms for `pread` in the same session (1.26x
  slower); plain mmap was 1.19x slower earlier. The 239 MB mapping costs ~24 ms to tear down in this VM. Not tried on
  the board, so on the real box the ranking is an assumption.
- **Chunk size 1 / 2 MiB instead of 4 MiB:** three paired sessions disagreed with each other (best-of-30 81-83 ms for
  all three in the quiet ones); no signal above the noise. With 12 workers 57 chunks quantise badly (4.75 per worker),
  so 2 MiB may still help on the board; not pushed.
- **Ablation of the parse (`pread` build, phase-1 wall):** replacing the dictionary probe with a bare hash saved ~4 ms
  of ~38, dropping the hash too saved ~2 ms more, and a build that only reads (no scan at all) still took 38-42 ms.
  Locally phase 1 is bound by the `pread` copy, so the dictionary is not where the time goes. (The runs drifted by up
  to 20% between sessions.)

## Not yet measured

- Parallel `pread`/`pwrite` of the output: not tried, buffered writes take the inode lock on ext4 and tmpfs.
- `MAP_POPULATE` / `MADV_POPULATE_READ` on the input (E2 b, c): moot, the input is no longer mapped.
- Tooling note: the Docker VM clock is ~5 h behind, so apt rejects signatures there. hyperfine and strace were put in
  `tools/bench.Dockerfile` from downloaded files. cachegrind could not be set up without weakening apt verification.

## Checks

Full seed-1 output sha256 495ca096...: identical on 5 consecutive runs. Byte-identical to the original implementation on
the tiny and sample data and on four stress inputs from `tools/stress_a.py` (endpoints of 1..125 bytes with shared
heads and tails, 1..10 digit latencies up to 4294967295, 1,800 endpoints, shuffled and sorted timestamps).
