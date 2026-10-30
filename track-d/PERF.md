# Track D: performance log

Board runs are identified by the commit the board measured (the board lists runs by commit). Local numbers are the
in-process time of the full seed-1 dataset on a 12-CPU Docker container (`tools/ab.py`, interleaved runs, median of 9).
They are noisy in absolute terms (a Windows laptop VM, not the scoring box) and are only used to compare variants.

## Board runs

| Step | Change | Run (commit) | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | Baseline: sequential parse, CSR, early-exit Dijkstra with a binary heap, 12 threads over queries | fdc4472 | 1.310 | 289 | 44.275 (K 7.68, 158 lines) |
| 2 | Parallel parse and CSR, parallel region-growing renumbering, radix heap, lazy `dist`, `MAP_POPULATE` | bdfba23 | 0.399 | 272 | 55.446 (K 12.85, 292 lines) |
| 3 | Drop `MAP_POPULATE` (to see whether the board charges the pre-faulted input pages to M) | pending | | | |

## Local measurements behind step 2 (full data, in-process ms, 12 CPUs)

| Variant | Median | Verdict |
|---|---|---|
| Baseline (step 1) | ~2260 | Docker wall time, bind-mounted input, so not comparable with the rows below |
| Sequential BFS renumbering | ~1570 wall vs ~2260 baseline wall | kept: ids are a random relabelling of a lattice, so ordering by BFS makes a search touch contiguous memory |
| Parallel parse, counting sort with atomics | 732 | kept |
| Radix heap instead of `BinaryHeap` | search phase ~300 vs ~380 | kept |
| Lazy zero-filled `dist` (stores distance + 1) | 668 vs 732 with the two changes above; `memory.peak` 299 -> 244 (includes ~92 MB of input page cache locally) | kept: untouched pages cost no memory |
| `MAP_POPULATE` on the input mmaps | 710 vs 736 | kept: one token |
| `-C target-cpu=native` | 705 vs 736 | within noise, not adopted yet; re-test with PGO |
| Parallel region-growing renumbering, 2^15 nodes per region | 546 vs 715 | kept: the sequential BFS was 150 ms of the run |
| Region size 2^12 / 2^15 / 2^18 | 533 / 531 / 538 | insensitive |

## Phase split now (ms, local)

parse + counting sort ~98, region growing ~38, new ids ~13, CSR rebuild ~46, searches ~330.

## Not tried yet

- Bidirectional Dijkstra: would roughly halve settled nodes but needs a reverse CSR (+40 MB, +~45 ms to build) and a
  second `dist`; the estimated net gain is small against the memory and line-count cost. Needs a measurement.
- PGO (last, because any later edit invalidates the profile), `get_unchecked` in the search loop, query ordering.

## Notes from board run bdfba23

- T fell 1.310 -> 0.399 s (3.3x) while lines went 158 -> 292: the time term gained far more than the size term lost.
- M barely moved (289 -> 272 MB) although the per-thread `dist` arrays went from eager to lazy (-55 MB locally).
  The board's memory.peak therefore seems to include the input mmap pages (92 MB graph.txt), probably because of
  `MAP_POPULATE` faulting them in. Next experiment: drop `MAP_POPULATE`, or read the input in pieces, and compare M.

## Experiments after step 3 (local, in-process ms median, full data; not yet on the board)

| Variant | Result | Verdict |
|---|---|---|
| PGO: instrumented build and run on the full seed-1 data in the pinned rustc 1.99.1 image, `-Cprofile-use`, profile regenerated for the exact source measured | 639 -> 728 (+14%) | rejected: slower, with no profile-mismatch warning. Not investigated further; `tools/pgo.sh` reproduces it |
| `-C target-cpu=native` through `.cargo/config.toml` (the scoring box builds the track itself) | 659 -> 642 | adopted: small but consistent in both minimum and median, two lines |
