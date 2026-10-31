# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The queries are read first and every distinct term is numbered. A 2% sample of the corpus estimates each term's
document frequency: terms in over 1/32 of the documents become bitsets, the rest sorted lists. Each phrase gets the
adjacent term pairs it needs postings for (one pair next to its rarest term, or every pair if all its terms are common).

The corpus is then scanned in 64 newline-aligned chunks on 12 threads, each chunk indexed and answered on its own, so
nothing that grows with the corpus is kept. An AVX2 tokeniser finds token edges 64 bytes at a time; tokens are looked
up 48 at a time with their table buckets prefetched, one 64-byte bucket holding the key words and metadata of three
terms, compared in one SIMD instruction. Dense terms set bits in the chunk's bitsets; other terms and tracked pairs
become `u16` chunk-local postings through a counting sort.

Every query is then evaluated on the chunk: BOOL strictly left to right on lists and bitsets, PHRASE by intersecting
its pair lists and re-tokenising the few remaining candidates from the mapped file. Counts and id sums go into
per-query atomics and the ten smallest ids into a per-query slot under a lock. Results are formatted by hand and
written once.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

SIMD intersection of `u16` lists, and fewer mispredicted branches in the per-token bookkeeping.
