# Track B: performance log

Board runs are identified by the commit the board measured. Local numbers are the in-process time on the full
seed-1 data in a 12-CPU Docker container on a Windows laptop (`tools/ab.py`: variants interleaved, median of 7-11 runs).
They are only used to compare variants, never reported as results.

## Board runs

| Step | Change | Run (commit) | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2200 | 0 |
| 1 | Baseline: query terms interned in a hash table, parallel corpus scan keeping postings only for query terms, sorted-list merges for BOOL, candidate intersection plus re-tokenising for PHRASE | 6ee4e92 | 0.772 | 328 | 50.613 (K 10.29, 334 lines) |
| 2 | Binary-search probing for lopsided intersections, SWAR tokeniser, bigram postings for phrases | 08b7a67 | 0.469 | 375 | 54.788 (K 12.47, 409 lines) |
| 3 | Terms in over 1/32 of the documents as shared bitsets (chosen from a 2% sample), (term, document) pairs packed into 4 bytes | f2ef5ee | 0.484 | 221 | 56.098 (K 13.24, 496 lines) |
| 4 | `-C target-cpu=native` | 5f206c6 | 0.474 | 223 | 56.310 (K 13.37, 496 lines) |
| 4b | (unchanged code, re-run with a Track D push) | ac9a033 | 0.469 | 222 | 56.436 (K 13.45, 496 lines) |
| 5 | AVX2 block tokeniser, right-sized hash tables, batched probe with the key in the slot, bitset-native BOOL evaluation, pairs tracked only where a phrase needs them | pending | | | |

## Local measurements behind step 2 (in-process ms, median)

| Variant | Median | Verdict |
|---|---|---|
| Step 1 | 771 | |
| `combine` probes the long list by binary search when one side is 16x shorter | 678 | kept: AND/NOT with a short running set no longer walks a 100k-id list |
| Streaming shift-and phrase matcher instead of building token spans | 661 vs 655 | no change; kept (no allocation) |
| SWAR tokeniser (classify 8 bytes per step, hash 8-byte words) | 684 vs 697 | no change at all, so the scan is not limited by byte handling; kept, it is what the phrase verifier uses |
| Bigram postings: the scan also records, per document, each adjacent pair of phrase terms | 562 vs 646 | kept: a 2-term phrase is exactly its pair list; longer phrases intersect pair lists, so only a handful of candidates need re-tokenising (candidates were 214k docs / 439 MB before) |
| Dense terms (sampled df over 1/32) as shared bitsets instead of pair lists | 587 vs 560; `memory.peak` 270 vs 359 MB | reverted: -89 MB, but not faster and +45 lines, about break-even on the score |

## Local measurements behind step 3

| Variant | Result | Verdict |
|---|---|---|
| Dense terms as bitsets (362 of 75.7 k terms; 10 MB) | pairs 24.9 M -> 14.9 M; `memory.peak` 359 -> 270 MB; time within noise | kept now: the board showed memory is the weak spot (375 MB) |
| Pairs packed as `term << 14 | document-in-chunk` | another -60 MB; `memory.peak` 211 MB with both; time 604 -> 635 ms median (min 541 vs 550), within noise | kept; guarded by assertions on the key count and the chunk size |
| Plain byte loop with `is_ascii_alphanumeric` instead of the SWAR tokeniser (-13 lines) | 699 vs 628 ms | reverted: the tokeniser pays for its lines now that the rest is faster (it showed no gain in the earlier test only because the probe cost hid it) |

## Where the time goes (step 2 numbers) (ms, local, 12 threads)

query parse 40 (single thread), scan 340-360, posting build 40, answering 100.
Scan ablation (wall ms): tokenising only 78, +key and hash 105, +table probe 200, +used/last check 245, +bigram
probe and pair push 340-380. About 75% of corpus tokens are query terms, so the table lookup is a hit path:
cache misses on slots, keys and per-thread `last`, plus unpredictable branches.

A touch-only pass over the whole mapped corpus takes 3 ms, so page faults are not the cost.

## Ideas not done yet

- One record per table slot (fingerprint, key, flags) so a hit costs one cache miss instead of three.
- Pairs packed into 4 bytes (term 17 bits, document-in-chunk 13 bits) to cut the intermediate arrays; 14.9-24.9 M pairs
  at 8 bytes each are most of the memory.
- Parallel query parsing; PGO and `target-cpu=native` last.

## Notes from board run 08b7a67

- T 0.772 -> 0.469 s (-39%), in line with the local 771 -> 562 ms. M rose 328 -> 375 MB: the bigram pairs added
  3.8 M (term, document) pairs and the scan keeps every pair at 8 bytes until the posting arrays are built.

## Notes from board run f2ef5ee

- M 375 -> 221 MB as the local `memory.peak` predicted (358 -> 213). T 0.469 -> 0.484 s is inside the local noise
  (+/- 5%) so the bitset step cost no measurable time. Lines went 409 -> 496.

## Experiments after step 3 (local, in-process ms median, full data; not yet on the board)

| Variant | Result | Verdict |
|---|---|---|
| PGO: instrumented build and run on the full seed-1 data in the pinned rustc 1.99.1 image, `-Cprofile-use`, profile regenerated for the exact source measured | 726 -> 888 (+22%) | rejected: slower, with no profile-mismatch warning. Not investigated further; `tools/pgo.sh` reproduces it |
| `-C target-cpu=native` through `.cargo/config.toml` (the scoring box builds the track itself) | 732 -> 706 | adopted: small but consistent in both minimum and median, two lines |

## Step 5: profiling-driven rework of the scan and the BOOL evaluator (local, in-process ms, median of 9)

Tools: `tools/cachegrind.sh` (valgrind cachegrind with branch simulation, per source line via `tools/cgtop.py`),
`tools/ablate.py` (the scan with progressively more of its work enabled), `tools/phases.py` (time and RSS per phase).

What the first profile said (sample data, 3.6 M tokens): about 166 instructions, 2 branch mispredicts and 1.8 L1 misses
per token. The SWAR word loop alone gave 2.3 M mispredicts (one loop exit per 8 bytes), the table probe 4.0 M L1 misses.
Ablation of the scan on the full data: tokenising 25-50 ms, + key and hash ~55, + probe ~95, everything ~215; the bigram
probe and the term bookkeeping were about 65 ms each.

| Change | Before -> after | Verdict |
|---|---|---|
| AVX2 tokeniser: 64-byte blocks, letter/digit and newline masks with `movemask`, token edges by `tzcnt`, the document number from a popcount of newlines (no id parsing, no per-token "first token of the line" test), tokens handed over in batches | 361 -> 338 | kept |
| Hash tables rebuilt at 3x their key count after parsing (they had been sized from an upper bound, 8-15% full, 2 MB each) | 337 -> 323 | kept |
| One `info` word per term instead of two arrays; plain stores instead of `lock or` for bitset words a chunk owns alone | 338 -> 340 | reverted: no effect |
| Batch lookup: the scan table holds each term's first key word, a tail hash and its metadata in one 16-byte slot, so a lookup is one cache line; all lookups of a batch run before any bookkeeping | 337 -> 317 | kept |
| First try of that table with a fixed 2-slot probe | 675, memory 388 MB | bug found: about 2% of keys do not fit two slots at a sane load, and the build doubled the table until they did; replaced by linear probing at load 1/6-1/3 |
| BOOL sets as a sorted list or a bitset: dense operands are combined word by word or bit-tested, never decoded into 250k-id lists | 330 -> 294; memory 214 -> 182 MB | kept |
| Fully branch-free bookkeeping (all four key words always computed, predicated writes into a per-chunk junk word, non-short-circuit conditions) | 294 -> 331 | reverted: the extra work costs more than the mispredicts it saves |
| Same without the key change | 278 -> 286 | reverted |
| Queries parsed in parallel (12 line-aligned pieces), only numbering sequential | 276 -> 288 | reverted: numbering (~19 ms) is the sequential part; splitting was only ~6 ms |
| Pairs tracked only where needed: a phrase with a rare term (from the 2% sample) gets one pair, that term and its rarer neighbour; only phrases of common words keep every pair. Only terms of tracked pairs take part in the scan's pair logic | 274 -> 250; memory 182 -> 147 MB | kept |

Correctness after each kept step: the spec's worked example, tiny and sample seed 1 against the Python reference, and
the full seed-1 hash. Before committing: tiny seeds 2-6 and sample seeds 2-3 also match the reference.
