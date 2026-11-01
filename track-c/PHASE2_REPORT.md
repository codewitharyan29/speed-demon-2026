# Track C phase 2: board decomposition, memory candidates, name-stage redesign (Nov 1, 16:45 UTC)

Labels: **measured (board)**, **local** (KVM, 4 vCPUs, no SMT), **estimate**, **hypothesis**. No new scored board run; the best
official K is still 29.76 (`9fb5c93`).

## 0. Repository safety

* `speed-demon` is being driven by the other session (`session_01Hjs26WsNrtCqoeh9uTGGrs`, on the user's computer). Its
  `tools/c_diag_run.sh` runs the five diagnostics and pushes a restore of candidate B's source after each one, then a final
  restore. This session messaged it, got a reply, and agreed **not to push to `speed-demon`**. That session will verify at the
  end that `track-c/src/main.rs` equals `9fb5c93`.
* All of this session's work is on `cloud/track-c-extreme-rewrite`. `9fb5c93`, `7c64950` and `736e57a` are unchanged and
  recoverable.

## 1. Board decomposition of the control (measured)

| Diagnostic | Run | T | M | Reported by |
|---|---|---:|---:|---|
| control `9fb5c93` | run-ec41ba8a226a (best), two others | 35.73-36.73 ms | 13.5-13.78 MB | feed |
| `tc6_nolookup` (no name lookup or mask; region and totals stay) | run-0b3959a3cbab | 24.70 ms | 9.96 MB | other session |
| `tc6_noresolve` (no name, region or totals) | via `d123e7a` (run-c22361965e83) | 21.91 ms | 7.61 MB | feed |
| `d5_read` (only `pread` of every block, plus setup) | run-7cc802c10d14 | 8.52 ms | 7.64 MB | other session |
| `tc6_nomask`, `d4_fixed` | pending | | | |

Increments (measured differences, each with about ±1 ms of board noise):

| Part | T | M |
|---|---:|---:|
| read + setup (process, regions.csv, tables, threads, output) | **8.5 ms** | 7.6 MB |
| parse (stages 1 and 2, with their prefetches) | **13.4 ms** | about 0 |
| region lookup + totals | **2.8 ms** | +2.3 MB |
| name lookup + mask writes | **11.3 ms** | +3.8 MB |

The leader's whole run is 17.4 ms. Read + setup and parse together (21.9 ms) are already above that. Splitting read from setup
needs `d4_fixed`, which is still pending.

K of hypothetical combinations (formula of `harness/scoring.py`):

| Scenario | T | M | S | K |
|---|---:|---:|---:|---:|
| control | 36.2 | 13.8 | 629 | 29.6 |
| name stage free | 24.7 | 9.96 | 629 | 38.2 |
| name stage halved, -1.9 MB | 30.6 | 11.9 | 640 | 33.0 |
| plus parse halved | 24.0 | 11.9 | 640 | 37.3 |
| plus read + setup from 8.5 to 6 ms | 21.5 | 11.9 | 640 | 39.4 |
| leader | 17.4 | 9.32 | 451 | 48.5 |

## 2. Parsing research

| Design | Status | Result |
|---|---|---|
| two-stage structural parser (control) | board | 13.4 ms parse |
| R2: fewer instructions in stage 2 | board | no gain |
| A1/A1d: one sequential pass per record (rejected; not repeated) | local | -14% instructions, +23% time: each field waits on the previous scan |
| chunk sizes 256 KB / 512 KB / 1 MB | local | 81.3 / 79.1 / 78.5 ms, 1 MB best |
| safe chunk boundaries (spec 2.2) | analysis | an LF inside quotes is never followed by a digit 1-9 or a BOM, so LF + [1-9 or BOM] is a record start; chunks are cut there, every chunk's last record must end where the next starts, and any failure reparses the whole file as one chunk (tested with hostile-LF) |
| multi-record stage 2 (4 records decoded together from the separator array, to overlap their dependency chains) | **next** | not built yet |

## 3. Memory architecture (all local; correctness 9/9 fixtures plus tiny-block and overflow-checked builds for M1)

| Candidate | Layout | max RSS | Time (4 threads, same run as control) | Decision |
|---|---|---:|---:|---|
| control | direct table: 2^17 x 64 B entries, 128 KB tags, 16-wide tag compare | 11.5 MB | 69-72 ms | |
| M1 | dense ids: 512 KB u32 index (8-wide), keys 16 B, masks 32 B, separate | **8.8 MB (-2.7)** | +19% | rejected |
| M2 | dense 48 B entries (key 16 + mask 32 together) | 8.8 MB | +19% | rejected |
| M3 | 1 MB index (load 0.38), 24 B key, 56 B entries | 9.8 MB (-1.7) | +16% | rejected |

Instrumented M2 on full seed 1 shows where the time goes: 269 k hint misses (8%: 100 k first sightings plus window overflow at
load 0.77, then 261 k scalar searches) and 407 k long-key compares (12% of names exceed 15 raw bytes, so they go to the spill
area). M3 fixes both, and is still +16%. The direct design reaches the entry from a 1-byte tag; every dense-id design adds an id
step and a wider index. At -1 MB = +1.5% K and +1 ms = -1.4% K, none breaks even: M3 is about +2.6% K from memory against about
-7% K from time.

Memory not tied to names (measured): parse-only runs already measure 7.6 MB, which is container overhead plus the region table,
buffers and runtime. The leader's total is 9.3 MB.

## 4. Name lookup redesign

* The locked mask writes migrate a cache line on 88.4% of writes, with a median gap of 67 k records between writes to one line
  (no contention; measured from the input). `tc6_nomask` (pending) will give the lock and migration share of the 11.3 ms.
* O2 (owner-written masks over rings, no locked instruction; `docs/track-c-locality/o2_owner_masks.patch`) is the design for
  that share. It is +11% locally, where it cannot show its benefit. Run it on the board only if `tc6_nomask` shows the lock
  share at 3 ms or more.

## 5. Rust and C++

The leaderboard shows no language effect: Rust ranks 1, 3, 5 and 6, C++ 2 and 4. clang and rustc share LLVM and the same
intrinsics. A C++ rewrite would keep the algorithm and could only change S. Lines would be counted after clang-format, as the
other session found for Track D (`ddfe81a` notes). It was not built: no measured bottleneck here is a language bottleneck.

## 6. Experiment registry (this phase)

| ID | Hypothesis | Patch | Correct | Local | Board | Decision |
|---|---|---|---|---|---|---|
| M1 | dense ids save about 2.7 MB of M | `docs/track-c-memory/m1_dense_ids.patch` | 27/27 | +19% T, -2.7 MB | | rejected |
| M2 | key and mask in one entry recover the extra line | `m2_dense_ids.patch` | 9/9 | +19%, -2.7 MB | | rejected |
| M3 | larger index and 24 B key remove misses and spills | `m3_dense_ids.patch` | 9/9 | +16%, -1.7 MB | | rejected |
| D-series | board decomposition | `docs/track-c-board-diagnostics/` | diagnostics | | 3 of 5 measured | evidence above |

## 7. Next, by expected value

1. When `tc6_nomask` arrives: lock share >= 3 ms, so run O2 on the board (valid output); otherwise drop the ownership idea.
2. Multi-record stage 2: decode 4 records per iteration from the separator array, so that the dependency chains of 4 records
   overlap. It targets the 13.4 ms parse without A1's serial chain.
3. `d4_fixed` splits the 8.5 ms into read and setup. Setup above 2 ms makes one persistent thread pool and fewer phases worth
   about 1-2 ms.
