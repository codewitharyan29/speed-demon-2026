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
| 4 | Round 2: median-first selection, 4-byte records (second word for latencies of 2^20-1 or more), 64 KB read blocks | run-b4312cc919af (0e8d5bf) | 26.2 | 22.7 | 469 | 40.34 | 80.29 |
| - | Rejected: step 4 as one `mmap` of the input (demand faults, parsed in place, no read buffers) | run-1d657eb26aa6 (f7be96c) | 29.8 | 22.4 | 457 | 38.06 | 79.02 |
| 5 | Round 3: speculative streaming (an hour is summarised and its records released as soon as the chunks to come should not hold it), 1 MiB chunks, fixed mmap threshold, exit after the last write | run-2e2c1b8cc858 (76db73c) | 25.66 | 18.81 | 499 | 41.92 | 81.12 |
| - | Rejected: a reader thread per parser feeding it 64 KB blocks through channels | run-44ff9ba108dd (ab591ca) | 30.42 | 21.19 | 555 | 37.01 | 78.41 |
| - | Compact dictionary (inserts under a lock), one state word per chunk, static non-PIE stripped binary | run-03f20e92eb73 (54e9b0f) | 26.46 | 19.49 | 488 | 41.13 | 80.71 |
| - | Rejected: write the summary out and drop it from the page cache every MiB | run-4c1e6ffe7694 (2e26d0a) | 26.10 | 19.53 | 501 | 41.24 | 80.77 |
| 6 | Shared pool of summary buffers, 32 KB read blocks | run-9f0f12713f79 (5bf1c87) | 26.16 | 18.66 | 491 | 41.70 | 81.00 |
| - | Rejected: two thirds of the available threads (8 of 12), run as part of 8da2c58 | | 32.10 | | | 38.73 | |
| 7 | Step 6 plus vector steps for long endpoints (final) | run-677620696279 (a6199d0) | 26.11 | 18.02 | 496 | 41.96 | 81.14 |

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

## Round 2, board diagnostics (not solutions; both fail the correctness gate but report T and M)

| Build | Run | T (ms) | M (MB) |
|---|---|---|---|
| 0e8d5bf, the full program (12 worker threads on the board) | run-b4312cc919af | 26.18 | 22.74 |
| 767c3f5: exits right after the parse phase (startup + read + parse) | run-cb78e6db6cd8 | 18.39 | 16.74 |
| aecba07: formats every hour but skips only the output write | run-b4ae7464cc65 | 25.07 | 22.63 |
| d1110d4: reads every block with `pread`, no parse, then exits | run-a13d2d78189f | 8.42 | 10.00 |

On the board (12 worker threads) T = 26.2 ms splits as: startup + `pread` of the whole input + exit 8.4 ms (32%), parse 10.0 ms
(38%), phase 2 (counting sort, percentiles, formatting) 6.7 ms (26%), output write 1.1 ms (4%). Parse + read together are 70%. The output file in the page cache is not what sets the peak (22.63 MB without the write, 22.74 with it). The local split
is the same shape (parse-only 58.5 ms of 89), so the Docker VM is a fair guide to where the user CPU goes even though it
overstates the kernel part.

Step 4 (one read-only `mmap` of the input, demand faults, parsed in place, a zero-filled anonymous mapping behind the end of the file so
the parser's look-ahead is safe) was 14% slower on the board (29.8 vs 26.2 ms) and saved almost no memory (22.4 vs 22.7 MB), as
it was 35% slower in the VM: the page-table work for 239 MB (faults, then the unmap at exit) costs more than the copy it removes. Kept `pread`.
The first board comparison was clear enough (+14% against +-6% noise) that a second run of the mmap build was not spent; the
revert's run is a second `pread` data point.

Where the parse's user CPU goes (parse-only build, `tools/ab.sh` 20 rounds, user ms, medians; each pair is from one session):

| Parse-only variant | user ms |
|---|---|
| full | 181.5 / 184.5 |
| dictionary probe replaced by a bare hash | 108.5 |
| ... and no digit conversion | 109.5 |
| ... and no hash (id = 0) | 95 |
| `key_eq` skipped (incorrect, ablation only) | 149.5 |
| 8192 dictionary slots instead of 4096 | 190.5 (no gain) |

So the dictionary lookup is about 40% of the parse CPU (the exact key compare about 19%, the hash 7%) and the digit conversion
is free.

## Round 3: toward the leader (K 55, T 18.5 ms, M 16.3 MB, S 294)

What the board says about this program, in order of what each experiment taught:

- **The log is not sorted, but it is nearly so.** Seed 1 is several interleaved ordered streams: 26k lines carry an earlier
  timestamp than their predecessor, 13% of the lines are in an hour behind the newest one, never by more than one hour (the
  largest lag in time is 70 minutes). So an hour can be finished once the oldest unfinished chunk starts two hours later;
  every chunk checks that guess when it ends (its earliest hour must not be below the next hour to hand out) and a wrong
  guess redoes the whole input without it. Shuffled and sorted stress inputs, the sentinel inputs and the page-multiple
  file all match the Python reference through both paths.
- **Streaming cut M by 4 MB (22.9 to 18.8) and did not move T (25.7 both).** The board has 12 threads on 6 cores, so T is bound
  by the total CPU work, not by phase barriers: overlapping the summaries with the parse hides nothing that was idle.
- **The board's CPU work is about 190 single-thread-ms spread over 12 SMT threads.** A single thread in the Docker VM takes
  190 ms for the whole program (`pread` 32, scan 77, histogram 8.5, scatter 8, percentiles ~20, the rest of the formatting ~23,
  `madvise` 3), and 190 / (12 x ~0.6) is the board's 26 ms. The leader's 18.5 ms means about 35% less work than this.
  `tools/ab.sh` with `CPUS=0` measures that single-thread cost; 8-thread wall time in the VM is dominated by VM artefacts
  (kernel work, a slow serialised file write, expensive faults).
- **Read and parse do not overlap.** On the board a read-only run takes 8.4 ms (239 MB at about 30 GB/s, close to memory
  bandwidth) and read + parse 18.4 ms, i.e. the two add up, although a thread stalled on memory should leave its SMT sibling
  free. A reader thread per parser, handing 64 KB blocks over through channels, was 18% slower (30.4 ms): the futex wake-ups
  and the doubled thread count cost more than the overlap gave.
- **mmap, again, cannot win.** A read-only mapping with software prefetch was 40% slower in the VM, and a helper thread
  populating the page tables ahead of the workers was 2x slower (populating 239 MB of page tables through one thread runs at
  about 2 GB/s here), so the page-table work costs about what the copy does.
- **The page cache of the output counts, and cannot be dropped on the board.** On a real filesystem the peak falls from 17.4 to
  9.4 MB when the summary is written out and dropped every MiB (`sync_file_range` + `posix_fadvise(DONTNEED)`), but the
  board's M did not change (19.53 vs 19.49 MB), so `/out` there behaves like tmpfs. Reverted.
- **Fewer threads is slower, a lot.** Two thirds of the available threads (8 of 12) gave T 32.1 ms, M not read (commit 4c2d9b2, run
  on the board as part of 8da2c58): the board keeps gaining from every SMT thread, so the limit is the total work, not contention.
  Locally 6 threads cost 8% more than 8 in the VM.
- **More threads is slower too.** 1.5x the available threads (18 of 12): T 30.2 ms, K 37.56 (commit e4f8cdf). 12 threads on 12 hardware threads is the optimum.
- **A from-scratch minimum-lines rewrite (S 304) is far slower.** Per-worker `HashMap`s merged into a sorted `BTreeMap`, two
  barrier phases, no streaming and no fallback: T 43.2 ms, M 25.2 MB, S 304, K 32.84 (run-19997323681c, b19bc7a). In the VM
  the stages are parse 35 ms, a serial merge of 8 ms, and phase 2 35 ms (id remapping, in-order writes); the hash map costs
  +13% single-thread CPU. The lines it saved (x1.076 on K) are worth far less than the time and memory it lost.
- **mmap with each worker populating its own chunk (`MADV_POPULATE_READ`), parsed in place with prefetch:** T 30.33 ms, K 38.83
  (e271777); 76% slower in the VM. Reverted: `pread` stays.
- **PGO** (profile generated in the pinned LLVM-tools image on the full public data, accepted by the board-style build): T 26.50 ms,
  K 41.30 (081c800) against 26.11 ms and K 41.96 without. Reverted.
- **Rules checked against the official context (speeddemon.dev, the track spec, FAQ):** the scoring box is an i5-13500 with turbo off,
  containers pinned to the 12 threads of the six P-cores; M is the cgroup `memory.peak` and includes what is written; the spec
  promises the log is *not* sorted (eight shards, chunks interleaved) and **no bound on how far a line can be out of order**, so
  the streaming guess must stay verified per chunk with a full fallback, as it is. S is `scc` over the whole `track-a/` folder after
  `rustfmt` defaults (reproduced locally: 499 against the board's 496).
- **Writing the summary with `O_DIRECT` (page-aligned stage, whole-page blocks, padded and truncated at the end):** on a real filesystem
  in the VM the peak fell from 16-24 to 8-9 MB with the exact file size and hash, but on the board M was 19.43 MB (18.02 without),
  T 26.83 ms, S 549, K 40.16 (run-7ffb7d851748, 9811e6f). Together with the earlier `fadvise` run this shows the board charges the
  output to the cgroup however it is written (its `/out` behaves like tmpfs), so the output's 10 MB are a fixed part of M. Reverted.
- **What does not help (local, single thread unless stated):** a plain `memcmp` for the key compare (+7% time), a batch of 16
  lookups with prefetched slots (+3.5%), `sort_unstable` for the percentiles, PGO with the pinned compiler (no change, as in
  Tracks B and D), 128 vs 64 KB blocks vs 32 KB (equal time, 32 KB lowers M by 0.4 MB), 512 KB or 256 KB chunks and a lag of
  1 hour (no change in M), a static non-PIE binary (equal within noise).
- **What S is.** The board's S is the number of non-comment, non-blank lines of `rustfmt`'s output, plus 6 (checked on four
  commits: 455/463/493 formatted lines against S 461/469/499). `tools/lines.sh` predicts it. Reaching S < 294 would need about
  40% fewer lines than this design has; each 10% fewer lines is worth only 1.5% of K.

## Round 2, step 2: 4-byte records (local, `tools/ab.sh` 20 rounds, `tools/mem.sh` fresh container, tmpfs output)

A record is one `u32`: latency in 20 bits, endpoint id in 11 (the spec allows 1,842 endpoints), 5xx flag in the top bit. A latency of
`2^20 - 1` or more is stored as that sentinel and the full latency follows in a second word, so a region holds at most 8 bytes per
line and the scheme is correct for any latency distribution. 1.2% of seed 1's lines take the second word. Both phase-2 loops walk
the words with one data-dependent stride (`each_record`). Checked byte for byte against the Python reference on shuffled inputs
(150k lines, 1,800 endpoints) with every latency = 4294967295, = 1048575 (the sentinel), = 1048574, in 1048573..1048577, uniform
random u32, all 0, and a 50/50 mix of 0 and 4294967295 (`tools/stress_a.py <seed> <lines> shuffled <mode>`, `tools/stress_check.sh`),
and on the tiny and stress-4 inputs; full seed 1 hash unchanged.

| Variant | wall ms | user ms | sys ms | memory.peak MB |
|---|---|---|---|---|
| `sel` (step 1), 256 KB read blocks | 96 | 306 | 306 | 25.5 |
| 4-byte records, 256 KB blocks | 94 | 298 | 303 | 20.4 |
| 4-byte records, 128 KB blocks | 91.5 (90) | 299 | 258 | 19.4 |
| 4-byte records, 64 KB blocks | 92.5 (88) | 290 | 256 | 19.0 |
| 4-byte records, 32 KB blocks | (89) | (285) | (246) | 18.9 |

(Numbers in parentheses come from the second 20-round session, which also timed 128 KB at 90 ms.) Records shrink the peak by 5 MB and
cost nothing. Smaller read blocks lower both sys time (less cache pressure, and the 8 buffers shrink from 2 MB to 0.5 MB) and the
peak; 64 KB is kept, 32 KB is not measurably better. The Docker VM has 512 KB of L2 per core shared by two threads; the board's
cache sizes differ, so this one needs the board's word.

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
