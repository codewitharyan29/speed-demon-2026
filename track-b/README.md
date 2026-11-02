# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).
`PERF.md` has every step with its board run ID, and the experiments that did not work.

One Rust file (`src/main.rs`, zero external crates), built with `--edition 2024`, LTO fat, `codegen-units = 1`,
`-C target-cpu=alderlake -C llvm-args=-align-loops=64 -C relocation-model=static`.

## Approach

**Query parsing.** All queries are read and parsed up-front by 12 threads in parallel. Every distinct term is interned
into a compact integer id. A 1/64 sample of the corpus (streamed in the same thread pool) estimates each term's document
frequency; terms appearing in more than 1/32 of documents are classified as *dense* (bitset-mode) and the rest as *rare*.
Each phrase query is assigned its rarest contained term as its anchor.

**Truth table compilation.** The boolean specification guarantees all BOOL queries have at most 6 terms evaluated
strictly left to right with the four operators `AND`, `OR`, `AND NOT`, `OR NOT`. For each such query we pre-compute a
64-bit integer truth table over all 64 possible combinations of 6 term-present bits; evaluating the query on a document
then reduces to a single 64-bit shift-and-mask: `(table >> mask) & 1`. Queries with more than 6 terms fall back to a
short sequential evaluation over the same `seen` bitset.

**Parallel corpus scan.** The corpus is split into newline-aligned chunks (one per thread); all 12 threads scan their
chunks concurrently with no global postings at all. Inside each chunk, an AVX2 tokeniser finds token boundaries 64 bytes
at a time. Every token is looked up in the intern table (48 at a time, one `_mm256_i32gather_epi32` per batch) and its
id written into a per-document term sequence.

**Per-document evaluation.** At the end of each document the L1-resident `seen` bitset (one bit per term id, 512 bytes)
holds exactly the distinct terms present. For BOOL queries the 6 presence bits are gathered by AVX2 into a 6-bit mask
and the truth table is probed in a single instruction; counts and document-id sums are updated directly without any
posting-list allocation or counting sort.

**In-scan phrase matching.** At each occurrence of a phrase's rarest anchor term, the neighbouring token-id sequence
for that document is compared inline against the phrase's term list. No pair postings, no bloom filter, no second pass.

**Memory and line-count management.** After each chunk is fully answered, the owning thread releases the scanned pages
back to the OS with `madvise(MADV_DONTNEED)` in parallel with output formatting. Peak RSS stays at 39–40 MB, safely
inside the 44 MB ceiling that preserves the 50× memory multiplier. The compiled binary is 961 lines, keeping the size
multiplier healthy.

## Dependencies

None: Rust standard library only. No external crates. All SIMD is written as `core::arch::x86_64` intrinsics.

## Performance

Best board result (commit `4a0b9ec`, run `f7d2d327f20c`):

| T (s) | M (MB) | S (lines) | Points | Rank |
|---|---|---|---|---|
| 0.1054 | 39.65 | 961 | **77.508** | ~#15 |

The live time fell from 0.163 s (step 27, C++ chunk-BOOL engine) to **0.105 s** with the truth table engine — a 36%
latency reduction with no change in memory footprint. The K factor (performance multiplier) rose from 27.3 to 35.5.

## With another 24 hours

- Tighter AVX2 usage in the token lookup: pack the gather indices and results for 8 queries at a time to hide
  more gather latency behind independent work.
- Tuple-set evaluation for queries sharing term subsets: group overlapping queries and evaluate their shared prefix once.
- Faster output formatting: batch-write the sorted top-10 id lists with a SWAR decimal encoder.
- Profile-guided inlining decisions for the phrase-match hot path.
