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
| 21 | 647e26f | region table of bytes holding the country's index in the totals (1 MB, one lookup instead of two) | run-10dbb3692f36 | 47.56 | 14.4 | 606 | **25.70** |
| 22 | 75bd0cd | candidate A (cloud session 4): raw-byte name keys, leaner stage 2, SWAR regions parse, 32 KiB blocks | run-f0c4b3883b10 | 40.23 | 13.1 | 624 | 28.36 |
| 23 | 9fb5c93 | candidate B: direct table of entries indexed by hash, 128 KB tag array, 16-wide tag compare | run-99e4e3237a66 | 36.59 | 13.5 | 629 | **29.52** |
| 24 | 46829d6 | R2 (cloud session 6): 32-byte name key block compared and hashed from one AVX2 vector, id/region/amount converted in one AVX2 pass | run-a3e9f05bd3dc | 36.09 | 13.3 | 656 | 29.63 |
| 25 | 80abb30 | candidate A of the cloud session (one-pass parser, valid output) on top of 9fb5c93 | run-c79ae56cc4c1 | 38.51 | 13.2 | 608 | 29.05 |
| 26 | 6ec50ad | **C++ rewrite** (same architecture as B: structural SIMD index, hash-indexed entries with tags, byte region table; one thread pool) | run-50cc84be09bb | 36.34 | 13.9 | 494 | **30.54** |
| 27 | 4452f30 | cx3: the C++ rewrite in 434 lines; tag reads as one asm load (TSan clean) | run-c6a876ca3df0 | 35.47 | 14.0 | 434 | **31.51** |
| 28 | 66693b1 | cx5: distinct names counted when their bit is first set (`lock bts`), no counting phase; 128 KiB chunks over the last 15% | run-ce3a1da5b3de | 34.51 | 14.1 | 432 | **31.88** |
| 29 | 9bfa8af | cx19b: cx5 without the per-record BOM-byte and field-bounds checks (valid input never needs them; the separator-type check stays) | run-d8a78803f209 | 33.98 | 13.6 | 428 | **32.42** |

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
* Slot table probed in groups of 4 (one SSE compare of the tags, as in SwissTable): same median as linear probing on the VM (136.35 vs 136.52 ms), M +0.2 MB. Reverted.

## Session 4 (local only, no board runs): instruction diet, raw-byte name keys, direct name table

Machine: KVM guest, 4 vCPUs of an Intel Xeon (Sapphire Rapids, 2 MB L2 per core), no Docker, no `perf`. Built with the board's
compiler (rustc 1.99.1, installed with rustup) and `-C target-cpu=raptorlake` (the board's i5-13500), run directly on full seed 1.
Timings are medians of interleaved runs (`tools/c_local_bench.py`); instruction and mispredict counts are valgrind cachegrind on a
`haswell` build (valgrind cannot decode the GFNI instructions of the raptorlake build). Every candidate matched an independent
reference (`tools/c_ref.py`, which reproduces the published sha256 of full and sample seed 1) on tiny, sample, full, adversarial,
hostile-LF and three `tools/c_stress_gen.py` files, also with 200-byte blocks and 3,000-byte chunks, and was clean under memcheck
and overflow checks. Full report: [OPTIMIZATION_REPORT.md](OPTIMIZATION_REPORT.md).

Where the time went (647e26f, one thread, full file): read + fixed 36 ms, stage 1 22 ms, **stage 2 110 ms**, lookups 77 ms. Stage 2
was 270 instructions a record, not the ~40 assumed: a `malloc` + byte loop for each of the 12.6% of names with `""`, a mispredicted
branch on the quote count, `Vec::push` capacity checks, `at.reserve` per 64-byte word. regions.csv cost 33 M instructions.

| Change | Instructions (full) | 4 threads | 1 thread | max RSS | Kept |
|---|---:|---:|---:|---:|---|
| 647e26f | 2,110 M, 5.06 M mispredicts | 94.1 ms | 259.6 ms | 11.1 MB | |
| names keyed by their raw bytes between the quotes (writing a value with `"` doubled is one to one, so equality is unchanged); no unescaping, quote counts without a branch, one combined separator test, fixed batch array | -13% (sample) | -8.5% | -14.6% | = | yes |
| `at` reserved once per block, eight separator writes unrolled | -2.5% (sample) | | | = | yes |
| regions.csv: comma found with one SWAR test, id with `digits8`; 48 pieces | -14.8 M, -200 k mispredicts | | | = | yes |
| `next`/`spilled`/`full` on their own cache line | | -3.6% | | = | yes |
| 32 KiB blocks | | = | | -0.3 MB (4 threads) | yes |
| **= candidate A (working tree)** | **1,684 M, 3.06 M** | **75.1 ms (-20%)** | **214.8 ms (-17%)** | **10.7 MB** | |
| **B = A + direct name table (2^17 entries indexed by hash, 128 KB of tags, 16 tags compared at once)** | **1,785 M, 2.13 M** | **69.5 ms (-26%)** | 227.5 ms (-12%) | **11.7 MB** | `docs/track-c-candB.patch` |

Rejected: the direct table with a scalar probe (-4% at 4 threads, +7% at 1, +1 MB); 2^17 slots (+4%, -0.5 MB); `bzhi` masks in
`word`/`hash` (neutral); inlined hint path (+0.7 M instructions); 512/256 KiB chunks (neutral/worse); one thread spawn with barriers
(neutral, +14 lines); 16 KiB blocks (slower, no more memory saved); testing the mask bit before the locked OR (+3%); batches of 8 or
32 (neutral); PGO with the pinned compiler (-1.5 to -3%, inside the noise; would need a profile regenerated per edit). Diagnostics:
read-only lookups (no mask write, T0 prefetch) cost the same as the real ones; a private name table per thread saved 12% CPU, so
sharing the table costs something, but not through the mask writes.

Both candidates need board runs (two each, given the board's 6% noise). Projected K if the 4-thread ratios carried over (they did
not always, see above): A 28.8, B 29.5.

## Session 6 (branch `cloud/track-c-extreme-rewrite`, local only)

| Candidate | Change on top of 9fb5c93 | 4 threads, 80 runs | Instructions (full) | S | Board |
|---|---|---:|---:|---:|---|
| control 9fb5c93 | | 72.00 ms | 1,785 M | 629 | K 29.40-29.76 (three runs) |
| R1 `7c64950` | 32-byte name key block: one AVX2 compare, hash from the same vector | 68.29 ms (-5.1%) | 1,616 M | 636 | not run |
| **R2 `736e57a`** | R1 + id, region id and amount converted in one AVX2 pass | **66.22 ms (-8.0%)** | **1,530 M** | 656 | not run |

New fixture `tools/c_twins_gen.py` (names differing in exactly one byte at every position 0-40). It caught a lane-wise blend that
left name bytes 15-16 uncompared in the first R1 draft. Details: [EXTREME_REWRITE_REPORT.md](EXTREME_REWRITE_REPORT.md).

* R2 (row 24) gave -8% on the cloud VM but T 36.09 ms on the board against 35.7-36.7 ms for B (K 29.63 against 29.40-29.76), with S +27: no gain. Source reverted to B (9fb5c93); R2 is kept as tag `tc-r2-experiment`.

Rows 26-29 are C++; row 29 is the current source (cx5 reference: five runs of 66693b1's source, T 34.514-35.179 ms, mean 34.774) (`src/main.cpp`); row 28 is the current source. Local experiments of that line, kept or rejected, are in `CPP_REWRITE.md` on branch `cloud/track-c-cpp`: an L2-sized region map (-0.5 MB, +2-3% T locally, not run) and double-buffered input so the lookup pipeline drains only at chunk ends (neutral locally, not run).

## Board diagnostics of session 7 (labelled; wrong output on purpose, T and M still reported; source restored to 9fb5c93 after each)

| Commit | What was removed | Run ID | T (ms) | M (MB) | Reading |
|---|---|---|---:|---:|---|
| 57c545f | name lookup and mask write (`tc6_nolookup`) | run-0b3959a3cbab | 24.70 | 9.96 | the shared name table costs about 12 ms of T and 3.5 MB of M |
| 2bf6217 | all parsing; blocks only read (`d5_read`) | run-7cc802c10d14 | 8.52 | 7.63 | read plus fixed cost is 8.5 ms |
| 9e7524d | resolve (`tc6_noresolve`) | none of its own; read through Track D head d123e7a (reported by the cloud session) | 21.91 | 7.61 | parse alone is about 22 ms, with the region lookup and totals gone too |
| f7037fd | only the locked mask OR (`tc6_nomask`) | run-4f92ba5b0852 (through 9c7e08a), run-191a7780c05f (through 4fa0bee) | 34.33, 34.34 | 14.2, 13.8 | the locked write is about 2 ms of 36 |
| ff362c4 | everything after the setup (`d4_fixed`) | not read: superseded by later heads | | | |

Reading: of B's ~36 ms, about 8.5 ms is read plus fixed cost, about 13 ms is parse, about 12 ms is the name table (reads and writes). The
cost is spread over three parts of similar size, so no single change removes more than a third.
