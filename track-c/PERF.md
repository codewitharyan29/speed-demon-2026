# Track C: performance log

Every row that has a run ID was measured by the live board (one run, full public seed 1, on the scoring machine). Local numbers come
from a Docker VM on a Windows laptop (Ryzen 5 3500U, 8 hardware threads, no `perf`) and are used to eliminate ideas and to count
instructions, never as results. The VM overstates memory latency and kernel work and has a different cache hierarchy from the
scoring i5-13500, so a local gain in a memory-bound part did not always transfer (see "lookups" below).

K = (14.44 s / T)^0.5 x (367 MB / M)^0.2 x (S_ref / S)^0.15 x (1 / D)^0.15; the board's numbers imply S_ref of about 107 lines.
D = 1 throughout (no dependencies). Output hash on every row below: the board's gate passed ("scored") unless the row says
"diagnostic".

## Board runs

| # | Commit | Change | Run ID | T (ms) | M (MB) | S | K |
|---|---|---|---|---|---|---|---|
| 0 | 767c3f5 | baseline (`tc-baseline`): mmap, quote-count pass, per-thread name tables keyed by a 64-bit hash | run-c3e9263072bb | 177.6 | 141.1 | 372 | 9.07 |
| 1 | 339e494 | rewrite: chunk starts found from the bytes after the cut, shared name table with exact comparison, one cache line per name, batched lookups with prefetch | run-81128db24eab | 68.7 | 20.0 | 602 | 20.05 |
| 2 | a1294ea | two-stage structural parser (SIMD separators outside quotes, records from known boundaries) | run-0878eeb5e3f6 | 71.1 | 21.4 | 532 | 19.81 |
| 3 | 57f74f4 | name table without glibc's memset of over-aligned memory; entry's first line prefetched for writing | run-fc8293439083 | 63.0 | 21.4 | 532 | 21.04 |
| 4 | ed50651 | regions.csv read in pieces, one set of totals and buffers per worker | run-e208486e3e44 | 69.7 | 16.1 | 552 | 21.07 |
| 5 | b3ed76e | same code again, to measure board noise | run-fa5e80722b45 | 69.65 | 16.1 | 552 | 21.09 |
| 6 | b285c86 | two-level name table (compact slots + one line per name by id) | run-98aaae621765 | 60.5 | 37.5 | 588 | 18.91 |
| 7 | 66197b3 | `PR_SET_THP_DISABLE` (the board has THP `always`; whole 2 MiB pages were backed) | run-420c8153005f | 60.5 | 16.2 | 592 | 22.35 |
| 8 | 2e630c3 | instruction diet 1: branchless separator extraction, inlined helpers, unchecked reads in the record parser | run-0a728d74e635 | 52.9 | 16.3 | 613 | 23.76 |
| 9 | fa128ea | instruction diet 2: numbers from the separators' digit counts, one quote popcount per record, single-multiply hash | run-8167df85a65b | 55.0 | 16.3 | 636 | 23.15 |
| 10 | 0d1a0c6 | three batches in flight (slots prefetched, then entries, then resolved) | run-67bec4623d2f | 52.5 | 16.5 | 644 | 23.62 |
| 11 | d1bc253 | batches of 32 + lazily zeroed tables | run-0682687bde57 | 54.3 | 16.5 | 652 | 23.16 |
| 12 | 5806282 | back to batches of 16; dead code removed | run-88a3e5416bb7 | 52.6 | 16.2 | 604 | **23.90** |
| 13 | c9c09c8 | 16 KiB blocks (a block and its separators in L1) | run-b11384dacf43 | 56.0 | 14.6 | 604 | 23.66 |
| 14 | cb4a342 | 128 KiB blocks | run-de6e49033d9e | 61.2 | 18.9 | 604 | 21.50 |
| 15 | 33ea307 | region table and name slots as lazily zeroed pages, alone | run-e1b6b13c4d77 | 54.9 | 16.2 | 612 | 23.34 |
| 16 | f8c5b1c | source of row 12 again (repeat) | run-7d972f222c6a | 52.9 | 16.2 | 604 | 23.73 |
| 17 | 704be8c | region table only as lazily zeroed pages, 48 pieces | run-2a43e35c614d | 53.16 | 16.3 | 608 | 23.70 |
| 18 | c0a86ae | source of row 12 once more (final HEAD) | run-df1808806b72 | 55.77 | 16.4 | 604 | 23.14 |
| 19 | 5586502 | name slots as u32 (tag above the id bits): 1 MB instead of 2 MB | run-8836fdcebdbb | 55.81 | 15.3 | 606 | 23.44 |
| 20 | 95582d0 | branch-free name comparison (`word`), entry found while warming used as a hint | run-c30f1d80a49f | 52.85 | 15.5 | 605 | **24.05** |

Kept: 1, 3, 4, 7, 8, 10, 12 (13-15 reverted: 64 KiB blocks and eagerly written zeros are the best measured). Rejected or reverted: 2 (equal to 1 within noise, kept for the instruction counts it enabled), 6 (alone: M
37.5 MB until THP was disabled, then kept as part of 7), 9 (fewer instructions but 2.1 ms slower on the board, cause not found), 11. Rows 5 and 16 show the board's run-to-run noise: 0.04% on one pair, 6% (52.6, 52.9 and 55.8 ms for the same source, rows 12, 16, 18) on the other, so differences below about 3 ms are not distinguishable.
Row 19: M fell by 0.9 MB as expected, T is inside the noise (52.6-55.8 ms for identical code). Row 20 is the final source; its T is inside the noise of row 12 too, so its gain is the 0.7 MB of M and a lower misprediction count (cachegrind on the full file: 10.0 M to 5.4 M mispredicts, ~1,250 M instructions), not a measured T gain. Rows 16-17 are not improvements; the final source is the one of row 12.

## Board diagnostics (labelled; they print wrong output on purpose, T and M are still reported)

| Commit | What was removed | Run ID | T (ms) | M (MB) | What it says |
|---|---|---|---|---|---|
| 11636aa | all record parsing | run-75845de19794 | 7.8 | 17.8 | fixed cost (process, regions, tables, merge, output) is 7.8 ms; the 17.8 MB showed the 8 MB `memset` |
| 8e07aa7 | name lookups (on a1294ea) | run-3bde9512adb1 | 44.3 | 22.0 | lookups cost 27 ms of 71 |
| 53cf631 | the mask update | run-45ed55a80469 | 68.9 | 21.3 | not clean: the write prefetch still took ownership of the lines |
| 236ff02 | name lookups (on fa128ea) | run-ccda7465c22a | 31.5 | 8.9 | parse + join + aggregate is 31.5 ms; the name table is 7.4 MB of M |
| 95c2f57 | mask update and write prefetch | run-cadd19375f0c | 48.9 | 16.3 | reading the table costs 17 ms, the mask writes 6 ms |
| 0e5fb19 | everything after process start (exit at top of main) | run-8c4eea5206b0 | 0.72 | 8.14 | process floor: an empty process already has M = 8.1 MB |
| a8fcb8f | everything after the region table | run-914f499201c3 | 3.54 | 8.63 | regions.csv costs about 2.8 ms |
| 7449bd5 | sharing (a private table per worker) | run-7ff6ee6007c5 | 64.1 | 105.9 | inconclusive: 12 table builds dominate; no sign of a large sharing penalty |

## What the measurements say

* The first rewrite took T from 177.6 to 68.7 ms and M from 141 to 20 MB: no quote-count pass over 202 MB, no 12 per-thread name
  tables of 10 MB each, no serial merge over 12 x 262,144 slots, and exact name comparison instead of a 64-bit hash.
* Instructions per record (valgrind cachegrind on the sample, `tools/cg_c.sh`): 576 for the first structural parser, about 340 after
  the two diets. T followed the first diet (60.5 to 52.9 ms) and not the second (52.9 to 55.0 ms, noise-free), so below about 350
  instructions the run is limited by memory behaviour and branch mispredicts, which cachegrind models only roughly.
* The name lookups still cost about 20 ms of the 52 on the board although they are 40 instructions: 3.3 million random accesses to a
  7.4 MB structure that does not fit a core's 2 MB L2. A deeper prefetch pipeline gave 2.5 ms; two-level layout 9 ms (row 6 vs 3).
* Local (VM) measurements misled twice: batch size and prefetch distance made no difference there but the table layout did on the
  board; and the VM's THP setting hid a 20 MB peak that the board's `always` setting produced (rows 6 and 7).

## Transparent huge pages

The board has THP `always`. Without `PR_SET_THP_DISABLE` (rows 6 vs 7) M was 37.5 MB instead of 16.2 MB with otherwise the same code,
and I could not find the mapping that was inflated: with THP forced to `always` in the Docker VM, a smaps dump during the run
shows huge pages only in the dense name table (6 MB), while cgroup `memory.peak` was 21-27 MB against 14 MB with THP disabled
(small thread stacks, a sub-2 MiB spill area and `M_ARENA_MAX=1` changed nothing). A gain from huge pages (fewer TLB misses and page
faults on the 10 MB table) is therefore untested; the memory risk made it not worth a board run.

## Not done / ideas with their expected value

* Partition the name space by hash across workers so that each owns a table of about 0.5 MB that fits in L2 and is never shared;
  needs a record exchange between workers (about 32 bytes per record, in bounded rounds). Estimated -10 ms; the private-table
  diagnostic above did not isolate it.
* One persistent thread pool and barriers instead of three `thread::scope`s (36 spawns): about 1 ms.
* A region table of bytes instead of 16-bit codes: 1 MB of M.

## Session 3 (local only, no board runs): what was tried and rejected

* Transparent huge pages for the tables (`prctl` removed, THP `always` in the VM): no faster (median 200 vs 187 ms on the VM) and M 22.8 vs 15.0 MB. Rejected.
* 2^19 slots (load 0.19 instead of 0.38): slower (179 vs 172 ms) and +1.2 MB. Rejected.
* The process floor was already read from the board (run-8c4eea5206b0: exit at the top of `main`, T 0.72 ms, M 8.14 MB; run-914f499201c3: after the region table 3.54 ms). A static / no_std / raw-syscall start can therefore save at most about 0.7 ms (1.4% of T) and costs lines; not pursued.
* Not done: partitioning the name space across workers (needs a bounded-round record exchange; local VM has 4 MB of L3 against the board's 24 MB, so it cannot rank the idea); a persistent thread pool (about 1 ms).
* Tools: `tools/c_memcheck.sh` (debug assertions + overflow checks + valgrind memcheck; Rust sanitizers need nightly, not in the offline image) and `tools/c_ab.sh` (interleaved timing + memory.peak).
