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
