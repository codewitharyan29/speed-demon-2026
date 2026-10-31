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
| 2 | Rewrite: shared lock-free endpoint dictionary, 6-byte records per (chunk, hour), per-hour counting sort, `pread` input | (pending) | | | | | |

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

## Rejected or not yet measured

- Parallel `pread`/`pwrite` of the output: not tried, buffered writes take the inode lock on ext4 and tmpfs.
- `MAP_POPULATE` / `MADV_POPULATE_READ` on the input (E2 b, c): moot, the input is no longer mapped.
- Tooling note: the Docker VM clock is ~5 h behind, so apt rejects signatures there. hyperfine and strace were put in
  `tools/bench.Dockerfile` from downloaded files. cachegrind could not be set up without weakening apt verification.

## Checks

Full seed-1 output sha256 495ca096...: identical on 5 consecutive runs. Byte-identical to the original implementation on
the tiny and sample data and on four stress inputs from `tools/stress_a.py` (endpoints of 1..125 bytes with shared
heads and tails, 1..10 digit latencies up to 4294967295, 1,800 endpoints, shuffled and sorted timestamps).
