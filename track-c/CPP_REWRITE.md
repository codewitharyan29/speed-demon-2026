# Track C in C++ (branch `cloud/track-c-cpp`): a from-scratch implementation

Status: local only, **no board run**. The control is `9fb5c93` (Rust, board K 29.40-29.76); production `speed-demon` is
unchanged.

## Design

Constraints taken from the board's history: at most 2 random lines per record (every board win removed one), no long serial
dependency chains (Candidate A lost on the board), no dense-id indirection (M1-M3 lost), and as few phases and thread spawns as
possible.

| Part | Choice | Why |
|---|---|---|
| threads | one pool; three phases (regions.csv, records.csv, mask count) separated by `std::barrier` | spawn once, not three times (`pool_b`'s idea, here for free) |
| input | `pread` of 32 KiB blocks into a per-worker buffer | the board measured `pread` reading at about 6 ms for 202 MB; mmap was rejected on this board |
| stage 1 | AVX2 compares for `"`, `,`, LF over 64-byte words, carry-less multiply for the quote state, separator positions into an uninitialised array | branch-free, independent per word |
| stage 2 | five known separators per record, SWAR numbers, name keyed by its raw bytes (escaping is one to one), quote counts by popcount | no serial chain between records |
| names | 2^17 hash-indexed 64-byte entries (32-byte key block + 256-bit country mask), 1-byte tags, 16 compared at once, three batches of 16 in flight | one random entry line per record, as in the best board result (B) |
| regions | byte table region_id -> country index | one random line, as in board run 21 |
| fallback | any record that does not fit: the whole file again as one chunk with a 2^22 table | exact for any input valid per spec 2.1 |

Alternatives rejected on the evidence: hash-partitioned routing of records to owner threads (exchange memory and
synchronisation; the board showed sharing is not the main cost) and zero-copy mmap input (rejected on this board for Track A).

## Results (local: KVM, 4 vCPUs of a Xeon, no SMT; the board is an i5-13500 with 6 cores and 12 threads)

| | Control (Rust, `9fb5c93`) | C++ (`cx2`, board flags `x86-64-v3 -mpclmul`) |
|---|---:|---:|
| full seed 1, 4 threads (30-50 interleaved runs, three sets) | 67.77 / 71.81 / 69.48 ms | 67.78 / 72.47 / 72.47 ms (first cx1 build) |
| full, 2 threads (20 runs) | 115.74 ms | 116.89 ms |
| full, 1 thread (20 runs) | 215.18 ms | 218.86 ms |
| sample seed 1, 4 threads | 22.00 ms | 20.67 ms (-6%) |
| tiny seed 1, 4 threads (setup-dominated) | 6.05 ms | 5.18 ms (-14%) |
| instructions, full (valgrind, haswell build) | 1,785 M | 1,729 M (-3%) |
| mispredicts | 2.13 M | 1.89 M (-11%) |
| max RSS, 4 threads | 11.6 MB | 11.4 MB |
| **S** (clang-format LLVM non-blank non-comment + Dockerfile) | 629 | **494** |

In this VM, time on the full file is at parity. The S cut alone is worth (629/494)^0.15 = +3.7% K at equal T and M. Setup-heavy
inputs are 6-14% faster.

## Correctness

* Byte-identical to the independent reference on tiny, sample and full seed 1, adversarial, hostile-LF (single-chunk
  fallback), stress1-3 and twins (9/9), also with 200-byte blocks and 3,000-byte chunks (9/9), and with the board's exact flags
  (9/9).
* AddressSanitizer and UndefinedBehaviorSanitizer: no report on tiny, adversarial, hostile, twins, sample and stress1.
* An out-of-spec file with 2,757 countries (over the 256 limit) is rejected, by the control too.
* Concurrency: the region table is written at distinct ids before a barrier, and the name entries are published with
  release/acquire on key word 0. A reader whose acquire sees 0 compares nothing, so no entry is read while it is written.

## Expected board effect (estimate)

T about equal, possibly lower by the setup gain; M about equal; S 494. K ≈ 29.6 x 1.037 ≈ 30.7. This is not the substantial gain
the mission asks for; it is a smaller, equally fast base with the same architecture's limits.

## Reproduce

```bash
g++ -std=c++23 -O3 -march=x86-64-v3 -mpclmul -flto=auto -fno-exceptions -fno-rtti -fno-pie -no-pie -pthread -static-libgcc -static-libstdc++ track-c/src/main.cpp -o solution
```

```bash
clang-format --style=LLVM track-c/src/main.cpp | grep -vcE '^\s*(//|$)'
```

## Official board result (one authorized run, Nov 1 22:59 UTC)

| | Commit | Run | T | M | S | K |
|---|---|---|---:|---:|---:|---:|
| **C++ rewrite** | `6ec50ad` (main.cpp sha256 `c16f9f128a47134d...`) | **run-50cc84be09bb** (scored, all gates) | 36.34 ms | 13.92 MB | 494 | **30.54** |
| control (Rust, `9fb5c93`) | | three runs | 35.73-36.73 ms | 13.5-13.78 MB | 629 | 29.40-29.76 |

T at parity, as predicted locally (inside the control's range); M +0.1-0.4 MB; S -135 lines. K +2.6% over the control's best: the
gain is the source size, as estimated (+3.7% at equal M; the slightly higher M costs about 0.5%). Production was restored to the
control's Track C at `4e11c91` (track-c/ identical to `669e396`; src/main.rs sha256 `27c953167c289741`). The board's "best" for
Track C now records this run.

## Experiment ledger after the official run (local only from here; no further board runs)

Per-line profile of the C++ baseline (valgrind, full seed 1, 1,760 M instructions, about 530 per record): separator extraction
51/record (9.7%), the three numbers about 75 (14%), per-record validation about 40 (7%), quote counts about 28 (5%), the lookup path
about 120 (23%), the rest about 215. No single dominant avoidable cost. And the board has twice shown that instruction cuts barely
move T (R2: -14% instructions, no gain; Candidate A: fewer instructions, slower). So no instruction-diet variant was built.

| ID | Hypothesis | Source | Correct | Local T (4 threads, vs baseline in the same run) | RSS | S | K projection | Decision |
|---|---|---|---|---:|---:|---:|---|---|
| cx2 (= 6ec50ad) | baseline | `track-c/src/main.cpp` | 9/9, sanitizers | | 11.5 MB | 494 | **board 30.54** | baseline |
| C-R (crm2) | a region map that stays in L2 (64-id bitmap + rank, 16 B per block, plus a dense country byte per region, 356 KB instead of 1 MB); the rank is computed and the country prefetched in `warm()`, so `resolve()` has no added dependent load | `docs/track-c-cpp-candidates/crm2_region_map.cpp` | 9/9 | +1.7% / +3.5% (CPU equal); 2 threads +1% | **-0.5 MB** (HWM 11.57 vs 12.04) | 546 | -1.5 to -2% at local T; positive only if the board's L2 misses on the 1 MB table cost about 1.5 ms or more (untested; analogy with board run 21) | not submitted |

## cx3: compact rewrite, official result and promotion (Nov 1, 23:15 UTC)

| | Commit (on speed-demon) | Run | T | M | S | K |
|---|---|---|---:|---:|---:|---:|
| **cx3** (same algorithm as 6ec50ad in 434 lines; tag reads in one asm load, so TSan-clean) | `4452f30`, main.cpp sha256 `3195cf70ac013b61` | **run-c6a876ca3df0** (scored, all gates) | 35.47 ms | 13.95 MB | 434 | **31.51** |
| previous best (C++) | `6ec50ad` | run-50cc84be09bb | 36.34 ms | 13.92 MB | 494 | 30.54 |
| Rust control | `9fb5c93` | three runs | 35.73-36.73 ms | 13.5-13.78 MB | 629 | 29.40-29.76 |

Projected K was about 31.1 from the source-size effect at equal T and M; the official K is 31.51 (T came in 0.9 ms lower, inside
one run's noise). Validation before the run: 9/9 fixtures normal and 9/9 with 200-byte blocks and 3,000-byte chunks;
ASan/UBSan clean on 6 inputs; TSan clean on 6 inputs with tiny blocks (the earlier version had a benign data race in `warm()`'s
16-byte tag read, removed by doing it as one asm load); local T at parity or better (4 threads -1.8%, 2 threads -4.5%); RSS
equal. **Promotion:** `speed-demon` keeps cx3 as Track C (the best verified candidate). Recovery points: `4e11c91` (Rust
control), `6ec50ad` (first C++).

## cx5: one phase and the tail removed; official result and promotion (Nov 1, 23:26 UTC)

| | Commit (speed-demon) | Run | T | M | S | K |
|---|---|---|---:|---:|---:|---:|
| **cx5** | `66693b1`, main.cpp sha256 `e016cdf08682676b` | **run-ce3a1da5b3de** (scored, all gates) | **34.51 ms** | 14.14 MB | 432 | **31.88** |
| cx3 | `4452f30` | run-c6a876ca3df0 | 35.47 ms | 13.95 MB | 434 | 31.51 |

Changes against cx3:
* **G1:** a (name, country) pair is counted by the one worker whose `fetch_or` sets the bit (`lock bts`; GCC emits it only when
  the mask is one shared expression; the first form compiled to a `cmpxchg` loop, caught in the disassembly). This removes the
  counting phase (a rescan of the 8 MB table) and its barrier.
* **G2:** 128 KiB chunks over the last 15% of the file, so that 12 workers finish together.

Local before the run: -1.6% (4 threads), -1.7% (2 threads), -10% on sample; RSS equal; 27/27 fixtures (normal, 200-byte
blocks/3,000-byte chunks, board flags), ASan/UBSan/TSan clean on 6 inputs. Board: T -0.96 ms, M +0.19 MB (inside the board's
spread), K +1.2%. **Promoted:** speed-demon's Track C is cx5. Recovery points: `4452f30` (cx3), `6ec50ad`, `4e11c91` (Rust).
