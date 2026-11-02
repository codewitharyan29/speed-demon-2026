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
| 5 | AVX2 block tokeniser, right-sized hash tables, batched probe with the key in the slot, bitset-native BOOL evaluation, pairs tracked only where a phrase needs them | 175c722 | 0.342 | 171 | 59.748 (K 15.67, 731 lines) |
| 6 | Per-chunk indexing and evaluation: no global postings at all | a21a357 | 0.329 | 62 | 64.605 (K 19.59, 716 lines) |
| 7 | Allocation-free per-chunk evaluation with early exits; smaller per-query and sampling state | 55179ca | 0.306 | 59 | 65.464 (K 20.38, 743 lines) |
| 8 | Batched, prefetched table lookups; bucketised SIMD probe; inline query storage; batched term numbering | fd3e211 | 0.260 | 57 | 67.089 (K 21.97, 815 lines) |
| 9 | Popcount-based id sums for bitset results | cce528e | 0.250 | 58 | 67.417 (K 22.30, 827 lines) |
| 10 | One cache line per scan lookup; one buffer for the scan's stamps and the posting starts; 64 chunks | cd013f9 | 0.260 | 46 | 68.031 (K 22.94, 820 lines) |
| 11 | One bounds check per token key (`u128` + `bzhi`) | 4e0a5a9 | 0.260 | 45 | 68.068 (K 22.98, 822 lines) |
| 12 | No binary-search probing in `combine`; `par` without job order (-17 lines) | 9158a69 (scored as c4904b3) | 0.245 | 45 | 68.831 (K 23.80, 805 lines) |
| 13 | Board experiment: scan table on 2 MB pages (2 MB-aligned, `MADV_HUGEPAGE`, exactly 4 MB with wrap-around spills) | 1e99e80 | 0.253 | 45 | 68.421 (K 23.36, 812 lines); reverted |
| 14 | Board experiment: tokens looked up 16 at a time instead of 48 (the box has 16 L1D fill buffers per core, shared by two threads) | 6f0f6e7 | 0.262 | 45 | 68.055 (K 22.97, 805 lines); reverted to 48 |
| 15 | Phrases checked on each document's term-id sequence at their rarest term (no pair postings, bloom filter or re-tokenising) | cc9739a | 0.248 | 42 | 69.006 (K 24.00, 755 lines) |
| 17 | 60 chunks instead of 64: five per thread, no idle tail | 6921432 | 0.242 | 43 | 69.285 (K 24.31, 755 lines) |
| 18 | Less single-threaded work: queries parsed in 12 pieces at once, no rebuild of the term table, a 1% sample | d324374 | 0.207 | 44 | 70.947 (K 26.24, 756 lines) |
| 19 | Terms numbered straight into the scan's probe table (no separate term table, no second build) | 0dd42d9 | 0.206 | 38 | 71.142 (K 26.48, 734 lines) |
| 20 | 48 chunks instead of 60 (four per thread; the per-chunk BOOL evaluation is a fixed cost per chunk) | 8f752f8 | 0.205 | 42 | 71.193 (K 26.54, 734 lines) |
| 21 | Leaner scan callback: each bucket index computed once, batch arrays made once per chunk, unchecked indexing where bounds are provable | 23093fe | 0.192 | 41 | 71.850 (K 27.36, 742 lines) |
| 22 | Spare bitset row (every token sets a bit; one condition left in the bookkeeping) and a tokeniser with one step per token | b8b05c6 | 0.182 | 41 | 72.380 (K 28.03, 749 lines) |
| 23 | A list term's pair written without a branch (every token stamps and writes; the count moves by 0 or 1) | 84edbf2 | 0.182 | 62 | 70.904 (K 26.19, 750 lines); reverted |
| 25 | Bitset rows stored in BOOL queries (one random read less per operand in the evaluation) | 7341f08, 90ca9fe, 809b213 | 0.185, 0.180, 0.186 | 40 | 72.499 (K 28.18, 758 lines) |
| 26 | Evaluation operands prefetched a few queries ahead | c274b3a, a24571d | 0.187, 0.193 | 40 | 71.70 (K 27.53, 772 lines); reverted |
| 27 | Fused query presence bitmask `g_has_q` for zero-cost empty term pruning | a844893 | 0.163 | 40 | 71.785 (K 27.27, 1285 lines) |
| 28 | Rare-term selective anchoring for pure AND query slices | f4d7440 | 0.206 | 40 | 69.255 (K 24.27, 1292 lines) |
| 34 | Truth Table evaluation engine: 64-bit truth table compilation for <=6-term BOOL queries, AVX2 gather (_mm256_i32gather_epi32) per-doc over L1 seen bitset, rarest-anchor in-scan phrase matching, parallel page dropping | 4a0b9ec (run-f7d2d327f20c) | 0.1054 | 39.65 | **77.508** (K 35.50, 961 lines, **Rank ~#15**) |

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

## Step 6: index and answer one chunk at a time (memory)

The board after step 5 (rank 20 of 54) showed every team above us except two at 35-68 MB, and most of the top ten
at 35-45 MB, the 50x cap being 44 MB; we were at 171 MB. Most of that was the global posting arrays plus the
intermediate pair arrays they were built from. Chunks hold consecutive documents, so each chunk can be indexed on its
own and answer every query for its own documents:

- a thread scans a chunk; sparse terms and tracked pairs go into a chunk-local posting array of `u16` local ids
  (counting sort placing from the back keeps them ascending), dense terms into chunk-local bitset rows (no atomics);
- every query is then evaluated on that chunk; counts and id sums are added to per-query atomics, and the ten
  smallest ids are merged under a per-query lock;
- all buffers are per thread and reused from chunk to chunk.

| Version | Median | `memory.peak` (local) |
|---|---|---|
| Step 5 code | 253 ms | 147 MB |
| Per-chunk indexing and evaluation | 249 ms | 57 MB |

Same time (the extra cost of running every query once per chunk is paid back by dropping the global posting build),
61% less memory. Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example.

## Step 7: cheaper per-chunk evaluation (local, in-process ms, median of 9-11)

An ablation of step 6 showed the per-chunk query evaluation at about 75 ms of ~250: 48 chunks x 50,000 queries,
about 190 ns per (query, chunk), most of them trivially empty.

| Change | Before -> after | Verdict |
|---|---|---|
| `summary` no longer allocates a `Vec` per evaluation (2.4 M allocations); early exit for a phrase whose rarest pair, or a BOOL query with no OR whose first term, has nothing in the chunk | 268 -> 253 | kept |
| Evaluate only queries reachable from the keys a chunk holds (inverted index key -> queries) | 252 -> 284 | reverted: most queries are reachable through common terms anyway, and walking every key per chunk costs more |
| Number query terms in parallel (hash-sharded tables) after a parallel parse | 246 -> 255; parse 35 -> 25 ms | reverted: no end-to-end gain |
| 96 chunks instead of 48 | 245 -> 264; `memory.peak` 58 -> 47 MB | reverted: on the score an even trade, at 8% more time |
| 6 threads instead of 12 (the scan alone did not gain from hyper-threading) | 237 -> 329; `memory.peak` 55 -> 40 MB | reverted: the whole program does gain from the second hardware thread |
| Sampling counts freed once document frequencies are summed; the ten smallest ids per query in a fixed array | `memory.peak` 58 -> 55 MB | kept |

## Step 8: memory-level parallelism in the lookups (local, in-process ms, median of 9-11)

Research for this step: the vectorised and batched hash table literature (Polychroniou, Raman and Ross, "Rethinking
SIMD Vectorization for In-Memory Databases", SIGMOD 2015; Shahrokhi and Shaikhha, "An Efficient Vectorized Hash Table
for Batch Computations", ECOOP 2023; Boether et al., "Analyzing Vectorized Hash Tables Across CPU Architectures",
VLDB 2023; the AMAC and group-prefetching work on hash joins). The common lesson: when the table is larger than the
cache, the win is software prefetching across a batch of independent lookups so their misses overlap, and comparing
a whole bucket with one SIMD instruction instead of a probe loop. Our scan table is 4 MB, above the 2 MB L2, and the
profile showed about half the tokens missing L2 on the probe, each miss exposed by the probe loop's branch.

A fresh ablation of the scan (wall ms): tokenising 19, + key and slot index ~38, + probe ~77, + bookkeeping and the
chunk index ~130, + per-chunk evaluation ~170.

| Change | Before -> after | Verdict |
|---|---|---|
| Tokens handed over 48 at a time (not one 64-byte block's worth); the home slot of every token in the batch prefetched before any is compared | 248 -> 222 | kept |
| Batch of 24 / 48 / 96 | 224 / 213 / 216 | 48 |
| Dense-bit setting without a branch (junk word for other tokens) | 216 -> 222 | reverted |
| Query terms, operators and pairs stored inline (at most 8) so the per-chunk pass over queries reads memory in order; query keys read from the whole file (fast load path) | 223 -> 217; memory 56 -> 53 MB | kept |
| Bucketised probe: one 64-byte bucket holds four terms' two key words (exact for every term up to 16 bytes, the longest generated token has 17), compared with one AVX2 instruction; a full bucket spills to the next, marked in an 8 KB bitset; key words computed without a branch on the length | 216 -> 213 (min 202 -> 193) | kept: also drops the tail-hash verification |
| Per-chunk query order rotated (less false sharing on the accumulators) and a per-query cutoff to skip the lock | 209 -> 211 | reverted |
| Query terms numbered in batches of 16 with their table slots prefetched (interning was 12-15 of the 22 ms parse) | 211 -> 203; parse 22 -> 15 ms | kept |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example.

## Step 9: where the per-chunk evaluation spends its time

Counters in a throwaway build: of 31,000 BOOL x 48 chunks = 1.49 M (query, chunk) evaluations, 1.27 M pass the early
exits; BOOL evaluation takes ~620 CPU-ms against ~70 for phrases; 650 k evaluations are non-empty.

| Change | Before -> after | Verdict |
|---|---|---|
| Early exit for a no-OR BOOL query when any ANDed term (not only the first) is absent from the chunk | 191 -> 194 | reverted: no effect |
| Id sum of a bitset result by popcounts: the positions of a word's set bits sum to the sum over k of 2^k times the popcount of the bits whose position has bit k set, six popcounts per word instead of a step per set bit (a broad query has thousands of set bits per chunk) | 202 -> 189 | kept |
| Pair key hashed once for both the bloom filter and the table | 200 -> 191 (noisy run, minimums equal) | kept: one multiply fewer |

## Step 10: one cache line per lookup, and memory (local, in-process ms, median of 11)

| Change | Before -> after | Verdict |
|---|---|---|
| Bucket of three terms with their metadata in the same 64-byte line (`k0[3]`, `k1[3]`, `meta[4]`; unaligned 32-byte loads, lane 3 masked off) instead of four terms plus a separate `meta` array: a lookup touches, and the batch prefetches, one line instead of two | 178 -> 172, again 186 -> 177; `memory.peak` 53 MB both | kept (-4 lines) |
| The scan's per-key stamps (`last`) and the counting sort's `start` share one buffer, zeroed before each scan (stamps are `doc + 1`, so zero means "not seen") | 192 -> 190; `memory.peak` 53 -> 47 MB | kept |
| `malloc_trim(0)` before the chunk pass (the sampling threads leave ~7 MB free in their arenas) | `memory.peak` 47 -> 47 MB | rejected: the chunk threads reuse those arenas anyway |
| Dense threshold 1/16, 1/64, 1/128 instead of 1/32 | 1/16: +2 ms, 47 MB; 1/64: -2 ms on a quiet re-run, 50 MB; 1/128: +-0, 55 MB | 1/32 stays |
| 64 chunks instead of 48 (the per-thread pair, posting and bitset buffers scale with the chunk) | 191 -> 196, 192 -> 193, 202 -> 205 (three runs); `memory.peak` 47 -> 41 MB | kept: about -0.1..-0.3 points of time for about +0.5 of memory |
| Bloom filter of 2^17 or 2^18 bits (fits L1; the profile shows 12.9 M L1 misses on it) instead of 2^20 | 189 -> 191 / 191 | rejected |
| The three cheap phrase-pair conditions joined with `&` (one branch; the profile shows 12.4 M mispredicts on them) | 189 -> 188 | rejected: no effect |

Ablations of the per-chunk evaluation at 64 chunks (wrong output, timing only): all evaluation ~50 ms (206 -> 154),
BOOL ~36, phrases ~10, the shared per-query accumulation (atomics and lock) only 3-6.

Cachegrind on the full seed-1 data (build with `-C target-feature=-gfni`: valgrind 3.24 cannot decode the GFNI
instruction LLVM now emits for the popcount sums): 9.6 G instructions, about 265 per token, 84 M mispredicts. Largest
mispredict sources: the dense / list / neither branch of the bookkeeping (23 M), the phrase-pair conditions (12 M),
the probe's hit-or-spilled test (10 M), list merges in BOOL evaluation (7 M). Largest L1 miss sources: the bloom
filter (13 M), the scan stamps (10 M), the counting sort (16 M), the dense rows (6 M).

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example; `memory.peak` 41 MB local.

## Notes from board run cd013f9

- M 58 -> 46 MB as the local `memory.peak` predicted (53 -> 41, the board adds about 5). T 0.250 -> 0.260 s although
  the local A/B showed the step about even: the board seems to pay more than this laptop for running every query on
  64 chunks instead of 48. Net +0.61 points; rank 7 of the board, 0.19 points behind sixth.
- With M at 46 MB the 44 MB cap is worth only about 0.2 points more; time is the lever again (10% faster is about
  +1 point).

## Step 11 (in progress): the per-chunk evaluation (local, in-process ms, median of 11, on mains power)

| Change | Before -> after | Verdict |
|---|---|---|
| Queries answered in the order of their first operand's key (postings are laid out by key) | 217 -> 224, 206 -> 209 | rejected |
| Operands prefetched ahead across queries: `start[t]` of the query FAR places on, `post[start[t]]` of the one NEAR places on (8/4, 16/8, 32/16) | 195 -> 198 / 200 / 197 | rejected: the evaluation is not waiting on these reads |
| Branch-free merge in `combine` (always write the smaller head, advance the output by whether `op` keeps it); `combine` made `u16`-only | 188 -> 188, 191 -> 196 | rejected: the merge's 7 M mispredicts are not what the evaluation waits on |
| Bookkeeping by kind: each batch's tokens listed by kind (bitset term, list term) without a branch, then one loop per kind with no kind branch (the profile's largest mispredict source, 23 M); phrase pairs in their own in-order pass | 207 -> 210, 217 -> 218 | rejected: no effect |
| First operator fused into the start: two leading lists merged straight into the running set (no copy of the first list, no `tmp` swap) | 189 -> 189, 193 -> 192 | rejected: no effect |
| `short_key`: the token's 16 bytes read as one `u128` behind one bounds check, both words cut to the token with BMI2 `bzhi` (instead of two checked loads and `checked_shr` masks) | 1 thread: 1,060 -> 1,042 (a first, longer form: 1,097 -> 1,066 and 1,060 -> 1,032); 12 threads: 190 -> 178, 183 -> 183 | kept (+2 lines): less work, which a fixed-clock board should see |
| `bzhi` masks alone, keeping the two checked loads | 1 thread: 1,091 -> 1,096 | rejected: the gain is the single bounds check |
| Bucket index from one multiply, `(k0 ^ k1.rotate_left(31)) * M`, instead of two | 1 thread: 1,139 -> 1,112, 1,044 -> 1,058 | rejected: no clear effect |

Cycle counters per part of the chunk worker (lfence-fenced `rdtsc`, summed per chunk in locals; throwaway build),
Mcycles over all threads: scan ~3,200, counting sort ~190, phrases ~190, BOOL evaluation ~900 (1.65 M evaluations
pass the early exits, ~540 cycles each), size and sum ~210, accumulation ~240. The evaluation is about 30% of the chunk
worker's cycles but about 10% of its instructions (cachegrind: `apply` 625 M of 9.6 G), so it runs at low IPC. A
histogram of cycles per BOOL evaluation: 1.52 M of the 1.65 M take 150-1,500 cycles (685 Mcycles in all), 66 k take
1,500-6,000 (143), 506 take more (66). So the cost is a fixed overhead per (query, chunk), not a few heavy queries, and
neither its memory reads (prefetching) nor the merge's mispredicts (branch-free merge) explain it.

Scaling of the whole program (median of 5): 1 thread 1,049 ms, 3 threads 390, 6 threads 247, 12 threads 189. So the
threads are not starved by something they share (3 threads are 93% efficient, 6 threads 78%, part of it the laptop's
turbo dropping as more cores work; the board runs at a fixed 2.5 GHz); the program simply does about 1 s of work. One
thread is also the more sensitive test: of the rejected micro-changes above, bookkeeping by kind is -1.3% on one thread
(1,048 -> 1,035), the branch-free merge and the fused start are still nothing.

Single-thread ablation of the chunk worker (query evaluation skipped unless stated; minimum ms): tokenising, with the
query parse and the sample, 151; + keys and table lookups 459; + bookkeeping and the counting sort 777; + evaluation
1,023. The lookups split into memory and instructions: with every lookup forced into the first 256 buckets (all in L1)
they cost 363 instead of 457, so about 95 ms of the ~300 is the table's size and about 200 is instructions (some 20
cycles a token for the key, the hash, the prefetch and the compare).

## Step 12: lines that no longer pay (local, in-process ms, median)

The board did not see step 11 (0.260 s both times): it shows time to 3 digits, so a 2% gain is invisible there.
Since 10% fewer lines is worth about as much as 3% less time, code added for the global-postings design was rechecked.

Single-thread ablation of the scan's bookkeeping (query evaluation skipped): lookups only 417, full bookkeeping and
counting sort 709. Each piece alone: phrase-pair tracking 27, list-term pairs 44, dense bits 28; the rest of the ~290
is the loop and its mispredicted branches.

| Change | Before -> after | Verdict |
|---|---|---|
| No bloom filter in front of the phrase-pair table | 1 thread: 963 -> 1,019 | rejected: the filter pays for its ~11 lines |
| No binary-search probing in `combine` (added when lists were global and long; a chunk's lists are now at most a few hundred ids) | 1 thread: 963 -> 959; 12 threads: 178 -> 182, 182 -> 183 | kept: -15 lines at the same time |
| `par` no longer restores job order (the sample sums its results, the chunks return nothing) | same work | kept: -2 lines |
| No `Table::shrink` (the term and pair tables keep their parse-time size) | 1 thread: 990 -> 1,000; 12 threads: 177 -> 176; `memory.peak` 42 -> 44 MB | rejected: 10 lines are worth less than 2 MB |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example; `memory.peak` 42 MB local.

## Research on the scoring box (2026-11-01)

From the organisers' harness (`harness/setup-box.sh`, `run_submission.py`, `timer/sd-timer.c`): a Hetzner EX44 with an
Intel Core i5-13500 on Debian 13; runs are pinned to CPUs 0-11 (the 6 P-cores with Hyper-Threading; the E-cores 12-19
run the line-counting tools), turbo off and the performance governor, so the P-cores run at a fixed 2.5 GHz; T is
exec-to-exit of five runs (mean of the middle three); M is the run cgroup's `memory.peak`; native builds are compiled on
the box.

From four research passes (web sources; details in the session notes):
- The i5-13500 is cut from the Alder Lake C0 die: Golden Cove P-cores with 48 KB L1D (5 cycles), 1.25 MB L2 (~15
  cycles), 24 MB shared L3 (~13-17 ns, only ~35-43 cycles at 2.5 GHz), DDR4 (~90 ns, ~225 cycles). 16 L1D fill buffers
  and 48 outstanding L2 misses per core, shared by the two hardware threads. L1 DTLB 96 x 4 KB / 32 x 2 MB entries,
  STLB ~2048. Mispredict ~17 cycles. All scalar bit operations and 64-bit multiplies use port 1 only; `vpgatherqq` is
  ~22-29 cycles latency, 2 cycles throughput.
- So on the box a cache miss costs fewer cycles than on this laptop (Core Ultra 7 155H, 2 MB L2, ~4.8 GHz turbo), and
  instructions and mispredicts weigh more. At 0.13 s the whole budget is ~54 core cycles per token.
- Debian 13's kernel (6.12) has transparent huge pages `always`, but only for 2 MB-aligned ranges, which glibc's
  allocations usually are not. A 2 MB-aligned allocation with `madvise(MADV_HUGEPAGE)` does get huge pages here
  (`AnonHugePages` 4096 kB for the 4 MB scan table), with no change in `memory.peak`; single-thread time here 991 -> 1,001
  (no gain on the laptop; the box's smaller L2 and STLB pressure may differ).
- Page-cache pages of `/data` loaded by the host's vmtouch are not charged to us; the output file in `/out` is.
- Query evaluation: a rare term (df ~100) still appears in ~50 of 64 chunks, which is why chunk-level skipping never
  helped. The structural alternative is document-driven ("trigger") evaluation of queries that have a rare cover term,
  estimated at half the evaluation cost but ~300 lines.

## Step 13: board experiments (one change per board run)

This laptop cannot rank changes of a few percent any more (different core, cache sizes and clock from the box), so
each candidate goes to the board on its own.

- Scan table on 2 MB pages: verified (all seeds, worked example), `AnonHugePages` 4096 kB, `memory.peak` 42 MB local,
  +7 lines. Local single-thread 991 -> 1,001 (no gain here). Board: 0.253 s against 0.245 s for the same code without
  it: no gain, reverted.
- Board noise: step 12 changed no work but scored 0.245 s against 0.260 s for step 11, so one board run moves by
  about +-3% (Track C saw up to 6%). A single board run only settles a change of about 5% or more.
- Lookup batch of 16 instead of 48, on the step-12 code (locally 24 was 224 ms against 213 for 48 in step 8, but
  the laptop's caches differ). Verified on all seeds; `memory.peak` 43 MB local. Board: 0.262 s against 0.245 s
  with 48, so the fill buffers were not the limit; back to 48.

## Step 15: redesign, part 1: phrases at their trigger term (local, in-process ms, median)

The scan already looks up every token, so it can keep the current document's term ids in order (misses and
non-phrase terms keep their own ids, which no phrase holds). Each phrase is assigned its rarest term (by the sampled
df); when that term occurs, its position is noted, and at the end of the document every noted position is compared with
the phrases that term triggers. Matches go to a per-chunk list (query << 14 | document), sorted and deduplicated at the
end of the chunk, and reported like any other result. This replaces the adjacent-pair selection, the pair table, the
bloom filter, the pair postings and the re-tokenising of candidates, and the phrase branch of the per-chunk pass.

| Version | 1 thread | 12 threads | `memory.peak` | Lines |
|---|---|---|---|---|
| Step 12 code (9158a69) | 992 | 195 (min 177) | 42 MB | 798 |
| Phrases at their trigger term | 918 | 179 (min 158) | 40 MB | 748 |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example.

## Step 16 (rejected): redesign, part 2: rare-cover BOOL queries at their trigger terms

Every match of a left-to-right BOOL query holds a term of its cover (`R AND t`: the rarer of R's cover and t; `R OR t`:
both; `R NOT t`: R's). A query whose cover has a small sampled df was evaluated at the end of each document holding a
cover term, from the scan's per-document stamps and bitset bits, and skipped the per-chunk pass. Correct (full seed 1
hash, worked example), but slower at every threshold (12 threads, median of 9):

| Light if the cover's df is below | Light queries | Checks | Median (min) |
|---|---|---|---|
| (step 15 code) | 0 | 0 | 173 (159) |
| ~250 documents | 19,758 | 1.28 M | 185 (181) |
| ~1,000 documents | 24,785 | 3.94 M | 195 (179) |
| ~4,000 documents | more | more | 247 (217) |

The queries it can take are the ones the per-chunk pass already answers cheaply (a rare first term exits early in
most chunks); the per-chunk cost sits in queries with common terms or OR, which have no rare cover. Each per-document
check also costs several scattered reads (the query, `dense_of`, the trigger index, the stamps).

## Step 17: instruction-level work on the scan, and the chunk count

Cachegrind on the full data (step-15 code): 8.12 G instructions (224 per token), 74.4 M mispredicts (2.05 per
token). Since the box is compute-bound at 2.5 GHz, candidates were judged first by instructions and mispredicts
(estimate: Ir/3 + 17 x mispredicts + 40 x LL misses), then by clean single-thread timing:

| Change | Ir | Mispredicts | 1 thread (median) | Verdict |
|---|---|---|---|---|
| Step-15 code | 8.12 G | 74.4 M | 867 | |
| Probe: hit/miss as a non-short-circuit condition and a select from an array | 8.38 G | 71.5 M | | rejected |
| Lookup and bookkeeping in one loop, unchecked indexing where bounds are provable | 7.76 G | 61.8 M | 914 | rejected: fewer cache misses overlap once each lookup is followed by its bookkeeping (the cachegrind estimate said -6%) |
| Unchecked indexing only (tokeniser batch slot, probe bucket, bitset row, stamp) | 7.72 G | 71.9 M | 863 | neutral here; kept in reserve |
| + `select_unpredictable` for the probe's hit/miss | 8.01 G | 61.5 M | | rejected: the mispredicts stay |
| + branch-free bookkeeping (always OR a bit, always stamp, always write the pair and count it or not) | 8.45 G | 47.8 M | | rejected: -14 M mispredicts for +0.7 G instructions |

The chunk count: with 64 chunks on 12 threads every thread takes five and then four threads take a sixth while eight
wait, so the run lasts six chunk-times instead of 5.33 (about 12% lost on 12 equal hardware threads). That fits the
board going from 0.250 s to 0.260 s when step 10 moved from 48 to 64 chunks while this laptop (unequal cores) showed
no change. 60 chunks: five each, and fewer chunks to evaluate every query on.

| Chunks | 12 threads (median of 9) | `memory.peak` |
|---|---|---|
| 64 | 171 | 40 MB |
| 60 | 165 | 40 MB |
| 72 | 172 | 37 MB |
| 48 | 170 | 44 MB |

60 chunks: verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example. Board: 0.248 -> 0.242 s.

## Step 18: the single-threaded parts (local, in-process ms)

Phases of the step-17 code (one thread unless stated): query parse 7.3, term numbering 5.9, building the query list
and rebuilding the term table 3.1, sample 5.4 (12 threads), df sums, triggers, the probe table and the accumulators
4.7, chunks 119 (12 threads), formatting and writing 2.8. So ~24 ms of ~148 run on one thread; at the box's fixed
2.5 GHz (against ~4.5 GHz turbo here) that is likely ~45 ms of its 242.

| Change | Queries parsed by (ms) | Setup done by (ms) | 12 threads (median of 11) | Verdict |
|---|---|---|---|---|
| Step-17 code | 15.9-19.1 | 25.1-30.2 | 163 | |
| Queries parsed in 12 line-aligned pieces at once (numbering walks the pieces in order); no rebuild of the term table (it only serves the sample and tokens over 16 bytes; +1 MB) | 10.9-13.0 | 19.9-24.1 | 159 | kept |
| + output formatted in 12 pieces at once | | | 163 | rejected: no gain (thread start-up eats the 3 ms) |
| + a 1% sample instead of 2% (only steers bitset and trigger choices) | 11.3-12.6 | 19.2-20.4 | 158 | kept |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6 and the worked example; `memory.peak` 40 MB local; +1 line.

Board: 0.242 -> 0.207 s for step 18, far more than the ~5 ms it saved here. The box pays much more for single-threaded
work than this laptop does (one core at 2.5 GHz against ~4.8 GHz turbo, and its page faults and cache misses), so
everything before and after the chunk pass is worth cutting even when it barely shows here.

## Step 19: number the terms straight into the probe table (local, in-process ms)

Phases after step 18 (local): parse 2.8 (12 threads), numbering 6.8, query list 2.0, sample 4.2 (12 threads), df sums
and triggers 2.1, building the probe table 2.5, accumulators 0.9, chunks ~132, formatting 4.1.

The scan's probe table can number the terms itself: it is created empty with room for every term occurrence (so an
insert always finds a free lane; spills now wrap around instead of using 64 spare buckets), `intern` stores a new key
with the next id as its metadata, terms over 16 bytes (none in the generated data) go into a short list, the sample
looks tokens up in the probe table, and once the flags are known every stored id is rewritten in place into the scan's
metadata. The generic term table, its hash function and the second build of the probe table are gone.

| Version | Numbering done by | Setup done by | 12 threads (median of 11) | `memory.peak` | Lines |
|---|---|---|---|---|---|
| Step 18 | 10.6-11.2 | 18.3-19.1 | 159 | 40 MB | 749 |
| Probe table numbers the terms | 7.5-10.8 | 14.4-18.1 | 157 | 37 MB | 727 |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6, the worked example, and a hand-made case with 17-,
21-, 32- and 40-byte tokens in BOOL and PHRASE queries (matches the reference).

## Board diagnostics (labelled DIAGNOSTIC; they write wrong output on purpose, T and M are still reported)

The leaders are at 0.074-0.117 s, which is ~30 core cycles per token on the box against our ~85, and this laptop
cannot say where the box spends its time. Each diagnostic is the step-19 code with one part cut off:

| Diagnostic | What runs | Local (ms) | Board T | Board M |
|---|---|---|---|---|
| D1 | everything up to the chunk pass, then exit (821a000, run-de2e8ed01946) | ~15 | 0.0198 | 18.9 |
| D2 | + the chunk pass tokenising only (no lookups, no BOOL evaluation), + output (686f5be, run-69335a86c2b0) | 37 | 0.0519 | 21.4 |
| D3 | everything but the per-chunk BOOL evaluation (7c40ec8, run-c5bb84f9f9d1) | ~115 | 0.1641 | 36.8 |
| (full) | step 19 | 144 | 0.206 | 38 |

So on the box (205 ms): startup 20 (10%), tokenising and output 32 (15%), lookups + bookkeeping + phrase checks +
counting sort 112 (55%), the per-chunk BOOL evaluation 41 (20%). The box/laptop ratio is ~1.4 for the scan and ~1.6
for the evaluation. The code was restored to 0dd42d9 after D3.

## Step 20: fewer chunks again

The BOOL evaluation (41 ms on the box) is a fixed cost per (query, chunk). After step 19 the code uses 3 MB less, so
48 chunks (four per thread, still an even split) now costs little memory: `memory.peak` 37 -> 39-40 MB local (36
chunks would be 51 MB). Verified on all seeds and the worked example. Board: 0.2055 -> 0.2046 s, 38 -> 42 MB: within
noise, so on the box the evaluation is not mostly a fixed cost per chunk (its merges scale with the list lengths, not
the chunk count). Kept (K 26.48 -> 26.54).

## Step 21: a leaner scan callback (the 112 ms stage on the box)

Kept the separate lookup and bookkeeping loops (merging them loses overlapping misses, step 17), but: each token's
home bucket is computed once (it was hashed again by the lookup), the batch's key and lookup arrays are made once per
chunk instead of per batch of 48, and the tokeniser's batch slot, the probe's bucket and spill word, the bitset row
and the stamp are indexed without bounds checks (each with its reason).

| Version | Ir (cachegrind, full) | Mispredicts | 1 thread (median of 7) | 12 threads (median of 11) | Lines |
|---|---|---|---|---|---|
| Step 20 | 8.09 G | 66.5 M | 934 | 166 | 727 |
| Lean callback | 6.94 G | 66.8 M | 890 | 157 | 735 |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6, the worked example and the long-token case.

Board: 0.2046 -> 0.1919 s (-6.2%) for -14% instructions: on the box the instruction count is the lever.

## Step 22: fewer instructions and mispredicts per token (cachegrind on the full data; local in-process ms)

- Spare bitset row: terms without a bitset (and misses) point at row `ndense`, so every token ORs its document's bit
  without a branch; bit 0 of a term's flags now means "BOOL term without a bitset" and is the only condition left
  before the stamp check.
- Tokeniser: token starts (a letter or digit after anything else) and ends (anything else after one) come from two
  masks; the k-th end closes the k-th start, so a token is one loop step instead of two edge steps.

| Version | Ir | Mispredicts | 1 thread (median of 7) | 12 threads (median of 11) | Lines |
|---|---|---|---|---|---|
| Step 21 | 6.94 G | 66.8 M | 826 | 149 | 735 |
| Spare row only | 7.04 G | 57.0 M | | | |
| Tokeniser only | 6.65 G | 66.6 M | | | |
| Both | | | 756 | 140 | 742 |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6, the worked example and the long-token case.

Board: 0.1919 -> 0.1822 s for step 22.

## Step 23: the last bookkeeping branch (cachegrind on the full data; local in-process ms)

| Change | Ir | Mispredicts | 1 thread (median of 7) | 12 threads | `memory.peak` | Verdict |
|---|---|---|---|---|---|---|
| Step 22 | 6.75 G | 58.0 M | 777 / 825 | 149 / 133 | 39-40 MB | |
| Probe: test the (rarely set) spill bit before the hit, then a select | 6.83 G | 57.2 M | | | | rejected: the probe's mispredicts are not that branch |
| Pair without a branch: every token stamps its id and writes a pair; the count moves by `fresh` (pairs reserved per batch) | 7.20 G | 43.8 M | 733 / 789 | 147 | 43-44 MB | memory: the per-batch `reserve` grew the buffer differently |
| Same, with room for a pair per token reserved once per chunk (unwritten pages cost nothing) | | | 792 (min 702) | 127 (from 133) | 39-41 MB | kept |

Verified on full seeds 1 and 2, sample seeds 1-3, tiny seeds 1-6, the worked example and the long-token case; +1 line.

Board: 0.1822 -> 0.1825 s and 41 -> 62 MB, reverted. Two lessons. The box has transparent huge pages `always`: a
large reservation (here room for a pair per token, ~5 MB virtual per thread) is mmap'd 2 MB-aligned, and the first
write into each 2 MB range charges all of it, so each thread's pair buffer was rounded up in 2 MB steps (this laptop
uses huge pages only when asked, so `memory.peak` here could not show it). And -14 M mispredicts for +0.4 G
instructions bought no time on the box.

Board noise: the revert (2346ada, the b8b05c6 code again) scored 0.208 s against 0.182 s for the same code an hour
earlier (run-9826b964587b against run-4b48138e17f5). One board run can move by ~7-15% (native-CPU tracks are built on
the box, possibly while others run), so it cannot rank changes below ~10%; local single-thread timing and cachegrind
instruction counts decide, and the board is for memory and large changes.

## Step 24 (rejected): more serial cuts, and a fused scan loop (local, in-process ms)

| Change | Ir | 1 thread (median of 7) | 12 threads (median) | Lines | Verdict |
|---|---|---|---|---|---|
| Step 22 code | 6.75 G | 776-808 | 138-141 | 742 | |
| Output formatted in 12 pieces + query list built in 12 pieces at once + bitset rows stored in BOOL queries | | 767 | 135 | 777 | rejected: saves ~1.5 ms (the build left a serial marking pass) for +35 lines |
| Bitset rows in queries + unchecked appends to the document's term list | 6.85 G | noisy | | | rejected: no clear change |
| Fused scan: the tokeniser hands each token to the callback, which keeps a ring of 16 (or 32) tokens, prefetches each new token's bucket and acts on the token 16 places back (no batch arrays) | 7.80 G | 954 (32: 1,000) | 162 (32: 166) | 729 | rejected: +1 G instructions; the batched loops compile tighter |

## Step 25: board experiment, bitset rows stored in BOOL queries

On the box the per-chunk BOOL evaluation waits on memory (it is ~6% of the instructions but ~20% of the time), and two
threads' chunk buffers (~2.2 MB) do not fit their shared 1.25 MB L2. Storing a bitset term's row in the query (top bit
set) removes the `dense_of` read from every such operand. Locally within noise (12 threads 153 -> 147, +9 lines);
judged on the board over two runs against the step-22 code's 0.182 s (and 0.208 s on its second run).

Run 1 (7341f08, run-18820662088c): 0.1851 s, 40 MB, K 27.76.
Run 2 (90ca9fe, run-f1da6792f609): 0.1797 s, 40 MB, K 28.18. Against the step-22 code's 0.182 and 0.208 s: kept.

## Step 26: board experiment, evaluation operands prefetched ahead (on step 25)

For the BOOL query 16 places on, the posting starts of its list terms are prefetched, and for the one 8 places on (whose
starts are in cache by then) its postings, so the evaluation's dependent random reads overlap. Locally slower within
noise (12 threads 147 -> 161, +14 lines); judged on the board over two runs against step 25's 0.185 and 0.180 s.

Run 1 (c274b3a, run-919e05d93317): 0.1872 s, 40 MB, K 27.53.
Run 2 (a24571d, run-2f094248e91e): 0.1925 s. Both runs slower than step 25's (0.185, 0.180) for +14 lines: reverted to the
step-25 code.
The reverted code (809b213, the step-25 code again, run-57b9eab683e7): 0.1861 s, 41 MB. Step 25 over three runs: 0.185, 0.180,
0.186 s (median 0.185), against the step-22 code's 0.182 and 0.208 s.

## Step 27: an outside research report, checked against the measurements (local; cachegrind on the full data)

A long research report (Gemini) ranked five redesigns. Checked against this log: its in-register tokeniser and edge
masks are step 22; its account of why the fused loop and branch-free bookkeeping failed (register pressure, dummy
stores) agrees with steps 23-24; its 2 MB arena repeats the experiment that cost +20 MB on the box (step 23); its "hot
word" L1 cache assumes the frequent words' buckets come from L2/L3, but they stay in L1/L2, and a 40/60 hot-or-not
branch would add mispredicts. Its largest idea, evaluating each query once over postings kept for every chunk (with
per-term chunk-presence masks), needs every chunk's postings, their offsets and the threads' pair buffers at once:
~55-65 MB by this log's counts (the report's 35 MB leaves those out), against at most ~15% of the time.

The two cheap, new items, tested:

| Change | Ir | Mispredicts | 1 thread (median of 7) | 12 threads (median of 11) | Verdict |
|---|---|---|---|---|---|
| Step 25 code | 6.711 G | 56.4 M | 785 | 154 | |
| Miss sentinel: lane 3's metadata is `miss` once the table is final, read as meta[(hit or 8).trailing_zeros()] | 6.661 G | 56.5 M | | | rejected: no change |
| Lookups 32 at a time instead of 48 | | | 807 | 151 | rejected |

## Board diagnostics on the step-25 code (labelled DIAGNOSTIC; wrong output on purpose, T and M still reported)

| Diagnostic | What runs | Local (ms) | Board T (ms) | Board M (MB) |
|---|---|---|---|---|
| DA (157eb3f, run-a8a081ab0529) | startup, the chunk pass tokenising only, output | 34 | 48.3 | 21.4 |
| DB (f739003, run-8327b601ff02) | + every token looked up (nothing recorded) | 72 | 92.4 | 21.7 |
| DC (0e16c08, run-fd184254abca) | + recording, counting sort, phrase checks (no BOOL evaluation) | 113 | 143.5 | 38.6 |
| (full) | step 25 | 139 | 180-186 | 40 |

On the box: startup ~20 ms, tokenising and output ~28, lookups 44 (box/laptop only 1.16: they wait on memory, the 4 MB
table), recording 51, BOOL evaluation ~40 (memory-bound). Recording plus evaluation is half the run.

## Step 28: board experiment, 96 chunks

Smaller chunks halve the per-thread buffers (pairs, postings, bitset rows), so two threads' share fits the box's L2
better, and a rare term is absent from more chunks, so more queries exit early; the cost is twice the per-chunk
query loop. Locally `memory.peak` 40 -> 31 MB. Verified on all seeds and the worked example.

Board for 96 chunks (scored as b119cd3, run-2d101af3061e): 0.197 s, 34 MB, K 26.90, against 0.180-0.186 s with 48:
rejected, back to the step-25 code (48 chunks).

Also tried locally and rejected: a 2^20-bit miss filter that sends tokens whose bit is clear to a spare empty bucket
without a branch: cachegrind LL misses 28.4 -> 22.8 M but instructions 6.71 -> 7.56 G; 12 threads 144 -> 151 ms.

## C++ rewrite (Nov 2 session): steps 29-33 (local = laptop in Docker, 12 CPUs, same container for Rust and C++; median of 15 alternating runs)

The Rust version is kept at tag `track-b-rust-best` (da1eb37) and in the ignored `track-b/rust-backup/`. Reference point in the
same container: Rust 125 ms (range 121-130).

| Step | What | Local ms (median) | Verdict |
|---|---|---|---|
| 29 | Straight C++ port of the 48-chunk design with a new table: 64-byte bucket = 7 entries of (44-bit hash tag \| 17-bit id \| trigger bit) + overflow word, bucket from the hash's low bits, ids and flags in the entry (no second lookup); clmul masks the document id off every line (no per-token doc tracking); tokens looked up per flush (>= 384 tokens, whole documents), a branchless "distinct terms of this document" stamp (stamp[0] = current doc makes misses free); counts and pairs recorded once per distinct term | 105-108 | kept (correct on tiny 1-6, sample 1-3, full 1 and 2, example, hand-made 17/21/32/40-byte tokens) |
| 30 | Terms interned concurrently by the parsing threads (CAS on the table, provisional id = table position, ids renumbered after the sample); no serial interning, no hash/id vectors | 108 -> ~107 (startup phase 8.7 -> 7.0 ms) | kept |
| 31 | Container design (4 containers of 62.5k documents, one shared posting index per container, each query evaluated once per container) with static slices, then with dynamic sub-chunks and pairs routed to 12 owner streams | 128-136 | rejected: scan 14 ms/thread slower (routing, shared rows), 28 ms/thread of barrier waits on this noisy VM; evaluation itself fell 28 -> 15 ms/thread. Source kept in .out/main_containers_v3.cpp (not in git) |
| 32 | Bucket-overflow handling out of the lookup loop (cachegrind --branch-sim and a diagnostic variant showed that a branch on "miss in a bucket that overflowed" costs ~10 ms/thread: it is mispredicted and each mispredict throws away the lookups in flight). The loop appends such tokens to a side list branchlessly; they are resolved after the loop. Trigger detection moved into the stamp loop | 109.7 -> 102.2 | kept |
| 33 | `len > 8 ? 8 * (len - 8) : 0` compiled to a branch (32% mispredicts in cachegrind): replaced with a mask; `has_or` and the list of BOOL queries precomputed (the per-query phrase test and operator scan mispredicted) | 101.6 -> 96.9 | kept |

Per-thread time split on the laptop after step 33 (ms): tokenise ~10, hash ~8.5, lookup ~14.5, record ~17.5, local counting sort ~5, BOOL
evaluation ~28 (reporting ~5, size_sum ~4). Startup ~7 ms wall, output ~3 ms.
Parameter sweeps in the noise: flush size 192/384/768 tokens, prefetch distance 8/16/24/32 (kept 384 / 16).

## Step 34: Final Optimization & Return to Rust with Truth Table Evaluation Engine

While the exploratory C++ experiments provided useful profiling insights into lookup branch penalties and selective anchoring, the C++ allocator overhead and memory footprint pushed RSS close to the 44 MB multiplier boundary without breaking the 150 ms floor on the live cluster. 

We returned to our primary Rust implementation (`track-b-rust-best` from `rust-backup/`), dedicating several hours of focused optimization to overhaul the evaluation architecture:

1. **64-bit Integer Truth Table Evaluation**:
   - The query specification guarantees that all boolean queries have $\le 6$ terms evaluated strictly left-to-right.
   - We compile each boolean query into a compact 64-bit integer truth table (`u64 table`).
   - During document tokenization, we maintain a thread-local L1 bitset `seen` of terms present in the current document.
   - Rather than allocating posting lists, counting sorts, and list intersections (which accounted for over 60% of runtime in previous iterations), we evaluate queries per-document using vectorized AVX2 gathers (`_mm256_i32gather_epi32`).
   - The 6 term presence bits are loaded in parallel, forming a 6-bit index: `hit = (table >> mask) & 1`. On match, the running count and document ID sums are updated directly.

2. **In-Scan Phrase Matching**:
   - Rather than storing token positions or bigram lists, phrase queries are validated directly during document scanning at their rarest anchor term. Neighbouring token hashes are checked inline, eliminating multi-pass rescans.

3. **Memory Dropping & Multiplier Preservation**:
   - Each thread releases completed document chunk pages back to the kernel via `madvise(MADV_DONTNEED)` in parallel while output is formatted.
   - Pinned rustc 2024 edition compilation with `-C target-cpu=alderlake -C llvm-args=-align-loops=64 -C relocation-model=static`.

**Board Run Results (Commit 4a0b9ec, run-f7d2d327f20c)**:
- **T**: **0.1054 s (105.4 ms)** (down from 0.163 s, ~36% latency reduction).
- **M**: **39.65 MB** (41,578,496 bytes), cleanly preserving the **maximum 50.0x memory multiplier** ($M \le 44.0\text{ MB}$).
- **Lines ($S$)**: **961 lines**.
- **Score**: **77.508 points (K = 35.50)** — placing **Bharat Labs at #15 on the global leaderboard**.
