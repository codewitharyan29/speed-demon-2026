# Track C: optimisation report (session 4)

## Executive summary

* **Baseline:** commit `647e26f`, board run `run-10dbb3692f36`: K = 25.70, T = 47.56 ms, M = 14.4 MB, S = 606, D = 1. Its source
  is unchanged in `d736e45` (HEAD; later commits only touch PERF.md and other tracks), so `git show 647e26f:track-c/src/main.rs`
  recovers it at any time.
* **Best candidates** (uncommitted; nothing was pushed):
  * **A**: the working tree's `track-c/src/main.rs`. Same architecture as 647e26f, but: names keyed by their raw bytes (no
    unescaping), a leaner stage 2, a SWAR `regions.csv` parser, the name-table counters on their own cache line, 32 KiB blocks.
  * **B** = A + `docs/track-c-candB.patch` (`git apply docs/track-c-candB.patch`): the slot table is replaced by a direct table of
    entries indexed by hash, with a 128 KB tag array and a 16-wide tag compare.
* **Best measured results** (local VM, full seed 1, 80 interleaved rounds, 4 threads, rustc 1.99.1 as on the board):

  | | median | 1 thread | instructions | mispredicts | max RSS | S (est.) |
  |---|---:|---:|---:|---:|---:|---:|
  | 647e26f | 94.14 ms | 259.6 ms | 2,110 M | 5.06 M | 11.1 MB | 606 |
  | A | 75.06 ms (-20.3%) | 214.8 ms (-17.3%) | 1,684 M (-20%) | 3.06 M (-39%) | 10.7 MB | 624 |
  | B | 69.47 ms (-26.2%) | 227.5 ms (-12.4%) | 1,785 M (-15%) | 2.13 M (-58%) | 11.7 MB | 629 |

* **Board confirmation: none.** No board run was made (pushing was not allowed in this session). If the 4-thread ratios carried
  over unchanged, K would be about 28.8 for A and 29.5 for B (formula below). Past sessions saw local gains fail to carry over, so
  these are projections, not results.

## Scoring formula used

From `harness/scoring.py` of the organisers' repository and the board's own numbers (which match it to 0.1%):
K = (14.44 s / T)^0.5 x min(367 MB / M, 50)^0.2 x min(107 / S, 1)^0.15 x (1 / D)^0.15, where S is code lines after rustfmt (no blanks
or comments) and D = 1. Around the baseline, 1 ms of T is worth 1.05% of K, 1 MB of M 1.39%, and 10 lines of S 0.25%. So memory is
worth more than it looks: 1 MB is about 1.3 ms.

## Experiment table

"Local median" = median wall time on full seed 1 with 4 threads, interleaved against the reference in the same run (the reference's
median of that run in brackets; the VM drifts by about 10% between runs, so only same-run comparisons count). "Memory" = max RSS of
that run. "Source size" = predicted board S (rustfmt lines + 6, which gives 606 for 647e26f, as the board said). Correctness: "pass"
= identical to the independent reference on tiny, sample, full seed 1, adversarial and hostile-LF (and from E12 on, three stress
files). "diag" = diagnostic with wrong output on purpose. Board K is N/A throughout: no board runs.

| # | Experiment | Hypothesis | Correctness | Local median | Memory | Source size | Board K | Decision |
|---|---|---|---|---:|---:|---:|---:|---|
| D1 | no mask write (on 647e26f) | the locked OR is expensive | diag | 89.8 (93.3) | 11.2 | N/A | N/A | diagnostic: -4% |
| D2 | read-only lookups (no mask write, T0 prefetch) | lookups pay for lines bouncing between cores | diag | 90.2 (91.0) | 11.2 | N/A | N/A | refuted: no difference |
| D3 | stage ablations, 1 thread | which stage dominates | diag | read 36, stage 1 +22, stage 2 +110, lookups +77 ms | N/A | N/A | N/A | stage 2 is the largest part, not the lookups |
| D4 | private name table per thread (on E6) | sharing the table costs | diag | 78.3 (83.7), CPU -12% | 32.3 | N/A | N/A | sharing costs 12% CPU, but D2 says not via the writes |
| E1 | direct table, scalar probe (on 647e26f) | drop one dependent random access (1 MB slot table) | pass | 97.9 (102.2), 1 thread +7% | 12.1 (11.2) | 609 | N/A | rejected: mispredicted probe loop, +1 MB |
| E2 | `at` reserved once per block | `reserve` per 64-byte word is overhead | pass | instructions -1.8% | = | 617 | N/A | kept (in A) |
| E3 | fixed `[Rec; 16]` batch, arena for unescaped names | `Vec::push` and one `malloc` per `""` name | pass | 93.3 (95.3); 1 thread 228 (249) | = | 641 | N/A | superseded by E4 |
| E4 | names keyed by their raw bytes; quote counts without a branch; one separator test | unescaping is unnecessary because escaping is one to one; the quote-count branch is mispredicted 26% of the time | pass | 84.7 (92.6) -8.5%; 1 thread -14.6% | = | 610 | N/A | **kept (in A)** |
| E5 | eight separator writes unrolled | the variable-count loop costs | pass | instructions -2.6% | = | 614 | N/A | kept (in A) |
| E6 | SWAR regions.csv parser, 48 pieces | regions cost 33 M instructions, 200 k mispredicts; 16 pieces on 12 threads is unbalanced | pass | 81.8 (94.1); 1 thread 203 vs 222 for E4 | = | 622 | N/A | **kept (in A)** |
| E7 | name-table counters on their own cache line | lookups read the line that insertions write | pass | 75.9 (78.8) -3.6% | = | 625 | N/A | kept (in A) |
| E8 | E1 on top of E7 | as E1 | pass | 75.9 (79.2); 1 thread +12% | +0.9 | 628 | N/A | rejected |
| E9 | `bzhi` masks in `word` and `hash`, branch-free hash tail | fewer instructions | pass | 86.4 (87.8), instructions +0.7% | = | 623 | N/A | rejected: neutral |
| E10 | 2^17 slots (load 0.76) | -0.5 MB at little cost | pass | 85.1 (81.8) +4% | -0.5 | 625 | N/A | rejected: net K negative |
| E11 | hint path inlined, cold full search, unchecked indexing | fewer instructions | pass | instructions +0.4% | = | 632 | N/A | rejected |
| E12 | direct table + 16-wide SSE tag window (on E7) | E1 without the mispredicts | pass | 71.8 (79.2) -9.3%; 2 and 1 threads = | +1.0 | 630 | N/A | **kept as B** |
| E13 | 512 / 256 KiB chunks | less imbalance at the end | pass | 79.1 / 81.3 (78.5) | = | 625 | N/A | rejected |
| E14 | PGO, profile from the full file, rustc 1.99.1 | better layout and branches | pass | A: 78.8 (81.2); B: 75.4 (76.7) | = | 625 + profile | N/A | not adopted: inside the noise, profile must be regenerated after every edit (Track A's board PGO run showed no change) |
| E15 | region table as bitmap + ranks + country bytes (450 KB) | -0.55 MB (+0.8% K) | not built | N/A | N/A | about +25 lines (-0.6% K) | N/A | not built: estimated net about 0 |
| E16 | one thread spawn with barriers between phases | three rounds of 12 spawns cost about 1 ms | pass | 75.40 (75.42) | -0.2 | 639 | N/A | rejected: no local gain, +14 lines |
| E17 | 32 KiB blocks | smaller per-thread buffers | pass | 74.4 (75.9) / 81.8 (81.9) | -0.3 at 4 threads (about -0.9 at 12) | 625 | N/A | **kept (in A)** |
| E18 | 16 KiB blocks | more memory saved | pass | 76.7 (75.9) | -0.2 | 625 | N/A | rejected |
| E19 | test the mask bit before the locked OR (on B) | 10% of the ORs can be skipped | pass | 71.3 (69.2) +3% | = | 633 | N/A | rejected |
| E20 | batches of 8 / 32 (on B) | another prefetch distance | pass | 71.7 / 72.3 (71.3) | = | 629 | N/A | rejected |
| A | E2 + E4 + E5 + E6 + E7 + E17 | | pass + stress + tiny blocks/chunks + memcheck + overflow checks | **75.06 (94.14)** | **10.7** | **624** | N/A | best known-good candidate |
| B | A + E12 | | same | **69.47 (94.14)** | **11.7** | **629** | N/A | best experimental candidate |

## Technical analysis

### What was actually slow

Valgrind (instruction-exact) and stage ablations on 647e26f contradict two assumptions from earlier sessions:

1. **Stage 2 was not "40 instructions a record".** It was 270, plus 12% more for the 12.6% of records whose name contains `""`:
   each of those allocated a `Vec`, copied the name byte by byte with a branch per byte and freed it. A branch on "does the record
   have any `""`" was mispredicted for 26% of records. Single-threaded, stage 2 was the largest stage (110 ms of 265), ahead of the
   lookups (77 ms).
2. **The lookups do not pay for the mask writes.** A diagnostic without the locked OR and with read-only prefetches (D2) was as fast
   as the real code. But a private table per thread saved 12% CPU (D4). With 4 threads, lookups cost about twice their
   single-threaded CPU, while parsing scaled perfectly.

Fixed costs: regions.csv took 33 M instructions and 200 k mispredicts (`position` byte loop, `fold`, `all`); the board measured
about 2.8 ms for it.

### Raw-byte name keys (E4, the largest single gain)

The value of a quoted field is its content with every `""` replaced by `"`. The content is therefore always the value with every
`"` doubled. An unquoted field cannot contain a quote, so its content is also the value with every `"` doubled (there are none).
Doubling quotes is one to one, so two fields hold the same value **exactly** when their contents, as written, are the same bytes.
The table is now keyed by those bytes. Equality is still checked byte for byte; only the representation of the key changed (the
escaped form instead of the unescaped one). The unescaped length needed for `name_bytes` is the raw length minus the number of
pairs, already known from the quote popcounts. This removed the allocation, the copy loop, the batch's `owned` list and the
mispredicted branch. Whether a field is quoted is now "its quote count is not zero" instead of a byte comparison. That follows from
the grammar (an unquoted field has no `"`), and on well-formed input it is the same answer. The adversarial and stress files
(quoted and unquoted spellings of one value, `""""`, `"""x"`, long names with many pairs) cover it.

### Direct name table (E12, candidate B)

647e26f touches three random lines per record: slot (1 MB table), entry (6.4 MB) and region (1 MB). B indexes the 64-byte entries
directly by the hash (2^17 entries, load 0.76) and keeps one tag byte per entry in a 128 KB array that stays in L2. The probe compares
16 tags at once (SSE2 `pcmpeqb`) and takes the first match before the first free tag. The common lookup is then a tag line in L2 plus
one entry line. Equality is still exact (`same()` compares length and bytes; the tag only rejects). A window with no answer falls
back to the scalar probe. This is not the rejected SwissTable experiment: that one kept the slot-to-entry indirection and only
changed how slots were compared. B removes the indirection. Measured: -9.3% with 4 threads, unchanged with 1 and 2 threads,
+1.0 MB RSS (8 MB of entries are all touched, against 6.4 + 1 MB). E1, the same layout with a scalar probe, was 7-12% slower
single-threaded: the probe length at load 0.76 is unpredictable, and the 16-wide compare removes that branch. Why the gain appears
only at 4 threads is not explained. It fits D4 (sharing costs, and B touches fewer shared lines per record), but this machine has no
hardware counters to show it.

### Other kept changes

* **Stage 1 (E2, E5):** room for 64 separators per word reserved once per block; the eight unconditional writes unrolled, with a
  rarely taken branch for more than eight.
* **Stage 2 (E3/E4):** `[Rec; 16]` per batch with a counter, smaller `Rec` (u32 fields), one OR-reduced separator test and an
  unaligned 2-byte BOM test.
* **regions.csv (E6):** the comma is the lowest zero byte of `w ^ 0x2C2C...` for an 8-byte word `w` (exact for the lowest byte);
  ids of up to 8 digits go through `digits8`, longer ones through the old loop; 48 pieces instead of 16 (12 threads).
* **Counters (E7):** `next`, `spilled` and `full` moved into a 128-byte-aligned struct, so that inserting a name does not invalidate
  the line holding `slots`, `entries` and `idmask` that every lookup reads.
* **32 KiB blocks (E17):** same speed, about 75 KB less per worker (about 0.9 MB at 12 threads).

### Risks

* The fast path's assumptions are unchanged (spec 2.2: an LF inside quotes is never followed by a digit 1-9 or a BOM, at most 256
  countries, at most 2^17 names). Failing any of them still falls back to the single-chunk run.
* "Quoted" is decided from the quote count, so the code relies on unquoted fields having no `"`, which the grammar guarantees
  (spec 2.1 `unquoted`).
* B keeps 1/8 of the table free (`next >= 7/8` of the entries sets `full`), so every scalar search ends. A tag mirrored past the end
  can be stored a moment after its original; a search that misses it uses the scalar path, which is correct.
* `unsafe` code was added (fixed batch array, raw writes in stage 1). It is covered by memcheck (0 errors on tiny, adversarial,
  hostile and stress) and by overflow-checked builds on all fixtures.
* Portability: AVX2 + PCLMUL as before; B adds only SSE2. The binary is built with the board's `target-cpu=native`; locally it was
  built for `raptorlake`, the i5-13500's target.

## Reproduction

Environment: KVM guest, 4 vCPUs of an Intel Xeon @ 2.1 GHz (Sapphire Rapids: AVX-512, 2 MB L2 per core), 15 GB RAM, no Docker, no
`perf`. rustc 1.99.1 (`rustup toolchain install 1.99.1 --profile minimal -c llvm-tools`), the version in the board's pinned build
image, plus 1.97.0 for the early runs. The data comes from the organisers' generator (seed 1, full/sample/tiny; sha256 of
records.csv and regions.csv as in the spec). The organisers' Python reference could not be run in this session, so
`tools/c_ref.py` is an independent reference written from the spec. It gives `d8088b1c...` (full) and `9035fed3...` (sample), the
published hashes. The scripts expect fixtures in `/home/user/cdata/<name>` with `<name>.ref`, and they point `/data` (a symlink) at
the fixture and write `/out`.

Generate the extra fixtures and their reference outputs:

```bash
python3 -I tools/c_adversarial.py /home/user/cdata/adv
```

```bash
python3 -I tools/c_hostile_lf.py /home/user/cdata/hostile
```

```bash
for s in 1 2 3; do python3 -I tools/c_stress_gen.py /home/user/cdata/stress$s $s 200000; done
```

```bash
for d in tiny-1 sample-1 full-1 adv hostile stress1 stress2 stress3; do python3 -I tools/c_ref.py /home/user/cdata/$d /home/user/cdata/$d.ref; done
```

Build a variant for the board's CPU with the board's compiler (prints the predicted S minus 6):

```bash
sh tools/c_local_build.sh candA track-c/src/main.rs
```

Check it against the reference on all fixtures:

```bash
XTRA="stress1 stress2 stress3" sh tools/c_local_check.sh candA
```

Interleaved timing (medians, max RSS, CPU time); `taskset -c 0` for one thread:

```bash
python3 -I tools/c_local_bench.py 80 base candA candB
```

Instruction and mispredict counts under valgrind (haswell build):

```bash
DS=full-1 sh tools/c_icount.sh track-c/src/main.rs
```

Candidate B:

```bash
git apply docs/track-c-candB.patch
```

Tiny-block and tiny-chunk builds for the correctness runs were made by editing `BLOCK` to 200 or 4096 and the chunk argument of the
first `run` call to 3000 or 777. Memcheck: `CPU=haswell` build, then `valgrind --error-exitcode=9`.

## Recommendations (next three experiments)

1. **Board-validate A and B, two runs each.** Upside: the projected K is 28.8 (A) and 29.5 (B) against 25.70. Complexity: none.
   Correctness risk: low (all fixtures, stress, memcheck). Validation cost: four board runs. Confidence: high that A improves K,
   because its gain is fewer instructions and mispredicts single-threaded too; medium for B, whose gain is multi-thread only and
   costs 1 MB.
2. **If B wins on the board, cut its memory.** The 2^17 x 64 B direct table is 8 MB. A non-power-of-two size (for example 112 Ki
   entries via a multiply-shift home position, load 0.89, at most 16 probed tags) would save about 1 MB, which is worth about 1.4%
   K. Complexity: low. Correctness risk: low (the scalar fallback stays). Validation: local plus one board run. Confidence: medium;
   longer probes may cost back what the memory saves.
3. **Partition names by hash across the workers** (each owns a table of about 0.6 MB that fits a P-core's L2). D4 shows that sharing
   costs at least 12% CPU at 4 threads, probably more at 12. Upside: possibly 10-20% of T. Complexity: high (a bounded record
   exchange between workers, about 100 lines, so -2.5% K from S). Correctness risk: medium (termination, back-pressure). Validation:
   several board runs. Confidence: low to medium. Do this only after 1 and 2.

Not recommended: more instruction-level work on stage 2 (it is now about 200 instructions a record, mostly the three SWAR number
conversions, quote counts and separator tests; E9 and E11 found nothing), PGO, smaller blocks, and the single thread spawn (no
local signal, and they cost lines).

## What could not be tested

* The board machine itself (i5-13500, 12 threads on 6 P-cores with HT, 1.25 MB L2, 24 MB L3) and its memory metric: pushing was not
  allowed. Local gains in memory-bound code have not always carried over.
* Hardware counters (no `perf` in the VM), so cache-miss explanations, especially for B, are inferences.
* The organisers' Python reference and generator for more seeds: this session's permission policy blocked running downloaded code
  after the first data generation. The independent reference reproduces the published hashes, and the stress generator covers the
  name-keying risk.
* Docker (`check.py`): not available. The working tree was built the way the Dockerfile does (`cargo build --release --offline`
  with the folder's `.cargo` config, rustc 1.99.1) and its output checked.

## Final state

* **Best known-good commit:** `647e26f` (board K 25.70). No new commit was made.
* **Best experimental candidates:** A = the working tree's `track-c/src/main.rs` (uncommitted); B = A + `docs/track-c-candB.patch`.
* **Uncommitted changes in the working tree:** `track-c/src/main.rs` (A), `track-c/README.md`, `track-c/PERF.md`, this report,
  `docs/track-c-candB.patch`, and the new tools `tools/c_ref.py`, `tools/c_stress_gen.py`, `tools/c_local_bench.py`,
  `tools/c_local_build.sh`, `tools/c_local_check.sh`, `tools/c_icount.sh`. They live only in this session's container until they
  are committed and pushed.
* **Board validation:** still required for both A and B.
* **Pushed:** nothing.

# Session 5 (Nov 1, 13:30-): starting from the board-validated candidate B

## Baseline and board results since session 4

| Candidate | Commit | Board run | T | M | S | K |
|---|---|---|---:|---:|---:|---:|
| 647e26f | `647e26f` | run-10dbb3692f36 | 47.56 ms | 14.4 MB | 606 | 25.70 |
| A | `75bd0cd` | (in PERF.md) | 40.23 ms | 13.1 MB | 624 | 28.36 |
| **B** | **`9fb5c93`, tag `tc-good-6`** | **run-99e4e3237a66** | **36.59 ms** | **13.5 MB** | **629** | **29.52** |

The session-4 projections were A 28.8 and B 29.5. B landed on its projection; A came in 1.5% below. The source of `9fb5c93` is
byte-identical to this report's candidate B, except for one unused import that B still carries.

## Where the time goes in B (local, 4 threads, full seed 1, 30 interleaved runs)

| Variant | Median | CPU | What it says |
|---|---:|---:|---|
| B | 78.2 ms | 275 ms | |
| no mask write | 73.4 | 259 | the locked OR is about 6% |
| no name lookup | 48.7 | 173 | name lookups are about 38% |
| no lookup, no region, no aggregation | 48.6 | 165 | region lookup and totals are nearly free |

## Experiments

| # | Hypothesis | Correctness | Local median (B same run) | Memory | S | Board K | Decision |
|---|---|---|---:|---:|---:|---:|---|
| F1 | name table of 15/16 of 2^17 entries (multiply-shift home), load 0.82: -0.5 MB | pass + stress1 | 73.9 (72.5) | -0.5 MB | 633 | N/A | rejected: about 0 net K |
| F2 | 14/16 of 2^17 (load 0.87): -1.0 MB | pass + stress1 | 76.7 (72.5) +5.7% | -1.0 MB | 633 | N/A | rejected: about -1.3% K net |
| F3 | 13/16 | over capacity, so it **correctly fell back** to the single-chunk run (490 ms) | | | | N/A | shows the cliff: a table under about 107 k names makes every run slow |
| P1 | partitioning names by hash across threads: upper bound by having each thread look up only the names it would own | diag | 67.5 (76.6) with a quarter of the lookups | | | N/A | rejected: a quarter of the lookups on a private quarter of the table still cost 17 of 26 ms; sharing is not the dominant cost, and the exchange plus about 100 lines would cost more |
| H1 | huge pages for the 8 MB entry table only (`PR_THP_DISABLE_EXCEPT_ADVISED`, Linux 6.18, falls back to disabling everywhere; table aligned to 2 MiB, `MADV_HUGEPAGE`): fewer TLB misses, no memory cost because every page of it is used | pass; AnonHugePages 8,192 kB confirmed | 69.95 (69.83), 80 runs | = | 633 | N/A | rejected: neutral locally; the board's kernel version is unknown |

## Conclusion for this session

No Track C source change beat `9fb5c93` locally. The three hypotheses with a clear mechanism (smaller table, less sharing, fewer
TLB misses) were each measured and rejected. `tc-good-6` remains the submission. The next opportunities, in order: (1) an
instruction-level pass over `same()` and `warm()` of B (about 170 instructions per record for the whole lookup path); (2) a compact
region table, now that B's 1 MB region table competes with the 8 MB entry table for L2 on the board (estimated net about 0
before, so only worth a try together with something else); (3) H1 on the board, only if the board's kernel is 6.18 or newer
(otherwise the prctl falls back and nothing changes).
