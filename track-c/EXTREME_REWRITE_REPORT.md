# Track C: extreme rewrite report (Nov 1, 14:00 UTC onward)

## Executive summary

* **Control:** `9fb5c93` (tag `tc-good-6`). Board: run-99e4e3237a66 K 29.52 (T 36.59 ms, M 13.5 MB, S 629); same source
  run-ec41ba8a226a K 29.76 (T 35.73) and run-a829e7350f95 K 29.40 (T 36.73). `speed-demon`'s `track-c/src/main.rs` is
  byte-identical to `9fb5c93`.
* **Best candidate: R2** (this branch, `track-c/src/main.rs`) = R1 + R2:
  * R1: the control with a new name key. A 32-byte key block (length + 1 in byte 0, then up to 31 name bytes) is compared
    with one AVX2 compare and hashed from the same vector.
  * R2: the id, region id and amount converted together in one AVX2 pass.
* **Best local result (full seed 1, rustc 1.99.1, `target-cpu=raptorlake`, 80 interleaved runs, 4 threads):** R2 66.22 ms,
  R1 68.29, control 72.00 (R2 -8.0%); 2 threads R2 -5.0% vs R1; instructions 1,530 M vs 1,785 M (-14%); max RSS equal; S 656
  (+27); D 1.
* **Official result: none yet.** Projected if the local ratio holds: R2 T ≈ 33.7 ms, K ≈ 30.6 (R1 ≈ 30.4).

## Formula (verified in the organisers' `harness/scoring.py`)

K = (Tref/T)^0.5 x min(Mref/M, 50)^0.2 x min(Sref/S, 1)^0.15 x (Dref/D)^0.15, Tref 14.44 s, Mref 367 MB, Sref about 107. At the
control, 1 ms = 1.4% of K, 1 MB = 1.5%, 10 lines = 0.24%.

## What the task needs (spec c-join.md) and what bounds the design

Per record, only these are needed: `id` (sum), the region id (join to a country), the amount (cents: sum, min, max), the name
(exact byte string after unescaping, for the distinct count and its byte length), and the note's unescaped byte length. The note
content itself is never needed, but its quotes decide where the record ends, so every byte of it has to be classified. Exact
name identity and exact record boundaries are required (spec 2.1). Guaranteed (spec 2.2): at most 9,999,999 for region ids, at
most 240 countries plus `_UNMATCHED`, LFs inside notes followed by a letter or a quote, a pool of 100,000 names. Not guaranteed
by the grammar, so the code keeps exact fallbacks: field lengths, a name over 31 bytes (spill), more than 2^17 names (fallback run),
LF + digit inside a quoted field (fallback run).

## Language

C or C++ through clang would use the same LLVM backend and the same AVX2 intrinsics that Rust uses here, and the board image is
Rust. No codegen advantage was expected for this workload, so no other language was built. The time went into the instruction
stream instead.

## Architecture comparison

| Candidate | Language | Architecture | T local (4 thr) | M (RSS) | S | D | Local K estimate | Official K |
|---|---|---|---:|---:|---:|---:|---:|---:|
| control `9fb5c93` | Rust | direct hash-indexed entries, 24-byte inline name compared as 3 masked words, u128 hash | 73.12 ms | 11.6 MB | 629 | 1 | 29.52 (board) | 29.40-29.76 |
| R1 | Rust | same, 32-byte key block: one-vector compare and hash | 68.29 ms | 11.6 MB | 636 | 1 | 30.4 | N/A |
| **R2** | Rust | R1 + id, region and amount converted together with AVX2 (`maddubs`, `madd`, `mul_epu32`) | **66.22 ms** | 11.6 MB | 656 | 1 | **30.6** | N/A |

## Experiment history (this session and the one before, against the control)

| Experiment | Result | Decision |
|---|---|---|
| table at 15/16 or 14/16 of 2^17 entries (-0.5 / -1.0 MB) | +2% / +5.7% T: net K about 0 / -1.3% | rejected |
| partitioning names by thread (upper-bound diagnostic) | a quarter of the lookups on a private quarter of the table still cost 17 of 26 ms | rejected: sharing is not the dominant cost |
| huge pages for the entry table only (`PR_THP_DISABLE_EXCEPT_ADVISED`) | 69.95 vs 69.83 ms over 80 runs | rejected |
| separators popped directly from the bitmasks instead of the `at` array | analysis: the branch-free form costs the same ~40 instructions per record, the branchy form mispredicts at irregular word boundaries | not built |
| **R1: 32-byte key block, vector compare, vector hash** | -9.5% instructions, -20% mispredicts, -6.1% T | **kept** |
| R2: the three numbers in one AVX2 pass (one branch for a number over 8 digits) | -5.3% instructions; 80 runs: -3.0% vs R1 at 4 threads, -5.0% at 2 | kept (+20 lines, net about +1% K over R1) |
| R1 with `_mm256_blend_epi16` to set byte 0 (first version) | **wrong**: that blend works per 128-bit lane, so name bytes 15-16 were not compared; caught by the new twins fixture | fixed with `_mm256_insert_epi8` |

## Technical explanation

The control compared a name as three 64-bit words, each masked to the name's length with a 128-bit shift (about 45 instructions
and a branch for names over 24 bytes), and hashed the first and last 16 bytes with more masked 128-bit arithmetic (about 27). R1
builds one vector: the 32 bytes before and after the name start, masked to `len` bytes, with byte 0 replaced by `len + 1`. This
vector is the key: the table stores the same 32 bytes, and equality is one `vpcmpeqb` plus `vpmovmskb == -1`, after an Acquire
load of word 0. Word 0 is published last, so a reader only compares a complete key. The hash folds the two halves of the vector
and multiplies once. Names over 31 bytes (none in seed 1; the grammar allows them) keep the spill area, with `255` in byte 0 and
the length in word 2. Every hit is still compared byte for byte; the hash and the tag only place and reject.

Risk: the vector compare reads the key words with a plain 256-bit load after the Acquire load of word 0. The words never change
after publication, so the load cannot see a partly written key. In Rust's memory model this is still a mixed atomic/non-atomic
access, though it is well defined at the instruction level on x86.

## Correctness

R1 and R2 are each identical to an independent reference (`tools/c_ref.py`, which reproduces the published sha256 of full and sample seed 1) on
tiny, sample and full seed 1, adversarial, hostile-LF, three stress files, and the new **twins** file (`tools/c_twins_gen.py`:
names that differ from one another in exactly one byte at every position 0-40, with `""` pairs and trailing spaces). It also matches
with 200-byte blocks and 3,000-byte chunks, and with overflow checks and debug assertions: 27 of 27 runs. Memcheck reports 0
errors on tiny, adversarial, hostile and a 1,000-record stress file. The buggy blend version fails twins, which shows the fixture
detects a skipped byte.

## Reproduction

```bash
python3 -I tools/c_twins_gen.py /home/user/cdata/twins
```

```bash
python3 -I tools/c_ref.py /home/user/cdata/twins /home/user/cdata/twins.ref
```

```bash
sh tools/c_local_build.sh r1 track-c/src/main.rs
```

```bash
XTRA="stress1 stress2 stress3 twins" sh tools/c_local_check.sh r1
```

```bash
python3 -I tools/c_local_bench.py 50 tc6 r1
```

## Final recommendation

Submit R2 (this branch's head) to the board twice against the control's three runs (29.40-29.76). If its board T is not at least
about 1% better than R1's would be, R1 (`7c64950`) is the fallback: 7 lines more than the control instead of 27. The gap to K 47.5 (T about 18 ms) is not closed by
this; the profiles say the remaining time is the stage-2 record parse (about 220 instructions per record) and the lookup and mask
write (about 130), with no single dominant term left.
