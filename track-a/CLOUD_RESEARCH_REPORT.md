# Track A: cloud research report (Nov 1, 2026)

Branch `cloud/track-a-research` (from `exp/a-algo`). **`track-a/src/main.rs` is unchanged (A1).** The experiment that passed
correctness is a patch, `tools/a_exact_identity.patch`. No board runs were made.

## Verified baseline

* A1 = `89573db` (source identical on `speed-demon` and on this branch): best board run `run-40f68e04b5c6`, T 23.71 ms, M 18.35 MB,
  S 508, K 43.71. Local rebuild with rustc 1.99.1 (the board image's compiler) and the folder's `.cargo` flags (`crt-static`,
  `relocation-model=static`; `target-cpu=raptorlake` in place of `native`): S counted as 502 + 6 = 508, matching the board.
* Formula verified in the organisers' `harness/scoring.py`: K = (Tref/T)^0.5 x min(Mref/M, 50)^0.2 x min(Sref/S, 1)^0.15 x
  (Dref/D)^0.15. With A1's numbers, 1 ms of T is about 2.1% of K, 1 MB of M about 1.1%, 10 lines of S about 0.3%.
* Architecture: 1 MiB chunks taken in order by 12 workers; `pread` in 32 KiB blocks; AVX2 space search; endpoint hashed with up to
  8 AES rounds (4 independent blocks over the first 64 bytes, one more per 16 bytes beyond); a 4,096-slot shared dictionary keyed
  by the **64-bit hash alone** (no byte comparison on a hit); 4-byte records into per-(chunk, hour) regions of a `MAP_NORESERVE`
  arena; hours summarised and released (`MADV_DONTNEED`) while later chunks are still parsed, on the guess that a line trails the
  first line of its chunk by at most 2 hours; any chunk that breaks the guess makes the whole input be processed again.

## Test data used here (and why it is synthetic)

The organisers' generator and reference could not be run in this session (the permission policy blocks executing downloaded
code), and speeddemon.dev is not reachable through the proxy. So:

* `tools/a_gen_spec.py` writes a file from the spec: 72 hours, 8 shards with non-decreasing timestamps (shard 0 uploads each
  hour 5-10 minutes late, shards 1-7 every 1-120 s, 0-30 s late), Zipf(1) popularity over 1,842 templates including the 13 fixed
  ones, the latency-digit mix measured on seed 1 by `tools/a_stats.py`, the listed statuses, and edge cases E2-E7, E11 and E14
  placed near their own time. Seed 1 at 3.1 M lines: 223 MB and 131 k groups (the real seed 1 is 239 MB and 126.9 k groups).
* `tools/a_ref.py`: an independent reference written from the spec (nearest rank `(p * n + 99) div 100`, byte order of
  endpoints, integer ppm).
* Fixtures: `tiny` (3 k lines), `s2` (300 k, seed 2), `full-1` (3.1 M), and `hostile` (3.1 M with edge cases scattered anywhere,
  which the spec allows: "no bound on how far a line can be out of order is promised").

## Quantitative findings

| Measurement | Value | How |
|---|---|---|
| A1, 4 threads, full-1 | 51.8 ms median, CPU 189 ms | 50 interleaved runs, fresh `/out` each time |
| A1, 1 thread | 162 ms, CPU 161 ms | `taskset -c 0` |
| user / sys CPU at 4 threads | 147 / 74 ms; 6,388 minor faults | `wait4` rusage |
| syscalls in one run | 7,450 `pread` (the input in 32 KiB blocks plus the 213 chunk cuts), 74 `madvise`, 72 `write`, 74 `mremap`, 88 `mmap` | `strace -c` |
| **startup** (`main` to the first chunk) | **0.4 ms** with a fresh `/out` | timestamps in an instrumented build |
| tail (last chunk start to exit) | 1.3-2.3 ms | same |
| hostile order (lines far out of order) | **every run takes the second pass: about 2x the time** | `hostile`; also the first version of the generator |

* **Startup is not significant**: 0.4 ms of 52 locally. Thread creation, dictionary and arena setup are inside that. One
  measurement artefact to avoid: a run that finds an old `summary.txt` spends about 6 ms truncating it (`File::create`); the board
  writes to an empty `/out`, so only a fresh directory gives honest numbers. The local harness now deletes the file before each run.
* **The guess is A1's real risk, not its speed.** With lines a few hours out of order, every chunk is parsed twice. The output is
  still correct (the fallback works), but T roughly doubles. The organisers' generator places lines near their own time, so seed 1
  does not trigger it. A holdout that did would cost about half of K. This is worth knowing before freeze, but it is not something
  to optimise against without evidence that the holdout differs.
* **Endpoint identity is not exact.** A hit is accepted when the 64-bit hash matches. With 1,842 endpoints the chance of a
  collision is about 1.8e3^2 / 2^65 ≈ 1e-13 for a non-adversarial input, but it is a hash standing in for equality.

## Experiments

| # | Hypothesis | Correctness | 4 threads (A1 same run) | 1 thread (A1) | M | S | Decision |
|---|---|---|---:|---:|---:|---:|---|
| ax | exact identity: confirm each hit by comparing the two masked 32-byte vectors the hash already holds; the hash then only spreads keys, so 2 AES rounds replace up to 8 | pass (tiny, s2, full-1, hostile) | 134.3 (127.9)* | 187.8 (177.0)* | = | 512 | superseded: keys only 16-byte aligned |
| ax2 | as ax, keys 64-byte aligned via `alloc_zeroed` | pass | 127.6 (125.9)* | | +0.6 MB (glibc zeroes over-aligned memory) | 516 | rejected |
| **ax3** | as ax, keys aligned by hand in a calloc'd block | pass | **52.95 (51.76) +2.3%** | 175.8 (168.4) +4% | = | **512** | **best exact variant; not adopted for speed** |
| ax4 | keys stored by slot, so the key load does not wait for the id | pass | 53.44 (51.76) +3.2% | 166.9 (162.3) +2.8% | = | 519 | rejected |
| diag | identity removed | invalid (the summary indexes ids never registered and panics) | | | | | discarded |

\* Runs marked with an asterisk were measured before two fixes, so their absolute values are inflated. The generator's first
version scattered E6/E7 lines in time, so A1's guess failed and every run made two passes. Separately, those runs also paid the
6 ms `summary.txt` truncation. Only the A1-relative comparison within each run is meaningful.

Conclusion on identity: making it exact costs about 2-4% of CPU (about -1.2% K at the board's T). The comparison is cheap (two
vector compares against a line that is usually in L1), but it sits on every line's critical path. A1's weaker identity is faster,
and no collision occurs in any known input. ax3 is the exact option if the organisers' judges weigh exactness; it is not a K gain.

## Rejected or not attempted, with reasons

* Percentile selection: already benchmarked against eight alternatives on the real group sizes (`ALGORITHMIC_RESEARCH.md`);
  no new hypothesis.
* Block size: 64 to 32 KiB was a board-measured step (PERF.md row 6). The kernel copy, not syscall count, is the floor (the
  board's read-only run was 8.4 ms).
* mmap variants, PGO, O_DIRECT output, reader threads, other thread counts: rejected on the board before; no new mechanism.
* A cheaper hash with hash-only identity: weakens identity further; not done.

## Best candidate

A1 stays the best for K. ax3 (`git apply tools/a_exact_identity.patch`) is the best exact-identity variant: +2.3% T locally,
+4 lines.

## Reproduction

```bash
python3 -I tools/a_gen_spec.py /home/user/adata/full-1 1 3100000
```

```bash
python3 -I tools/a_ref.py /home/user/adata/full-1 /home/user/adata/full-1.ref
```

```bash
sh tools/a_local_build.sh a1 track-a/src/main.rs
```

```bash
XTRA=hostile sh tools/a_local_check.sh a1
```

```bash
BARGS="--data /home/user/adata/full-1 --out /tmp/aout" BRM=/tmp/aout/summary.txt python3 -I tools/local_bench.py 50 a1 ax3
```

Machine: KVM guest, 4 vCPUs of a Xeon (Sapphire Rapids), rustc 1.99.1. The board is an i5-13500 with 12 threads; local times rank
ideas, they are not board results.

## Three best remaining opportunities

1. **Bound the cost of the out-of-order fallback.** Today a single late line makes everything be parsed again. Instead, keep the
   records of hours already summarised in a small overflow list, and re-summarise only the affected hours. Upside: insurance
   against a holdout with different shard timing (a 2x cliff). Complexity: medium. Risk: medium. Validation: hostile fixture plus
   one board run.
2. **Less kernel work per line.** sys time is a third of CPU locally (`pread` copy and 6.4 k page faults on the record regions). A
   reusable per-worker record area instead of fresh `MAP_NORESERVE` regions released by `madvise` would remove most faults. The
   record bytes would have to be copied to their hour later, so the gain is uncertain; measure faults and sys time first.
3. **Exact identity at zero cost.** Fold the comparison into the space search: the scan already loads the 64 bytes that hold the
   endpoint, so comparing them with the key could replace the hash for the common case, keeping the hash only to choose the slot.
   Low complexity; worth one local try before deciding whether exactness matters for judging.
