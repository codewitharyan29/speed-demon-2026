# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The queries are read first and every distinct term goes into one open-addressing hash table. The corpus is then
scanned once, in 48 newline-aligned chunks on 12 threads. Bytes are tokenised with a 256-entry table (lower-cased
letters and digits, everything else a separator) and each token is looked up in the table while it is hashed; only
terms the queries use are recorded, once per document. Per chunk and term the counts are known, so every chunk
scatters its (term, document) pairs into its own slots of one posting array and each list comes out sorted.

BOOL queries are evaluated strictly left to right with sorted-list merges. PHRASE queries intersect the term lists
to get candidate documents, then re-tokenise each candidate from the mapped file and look for the exact sequence.
Results are formatted by hand into per-job buffers and written once.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

Galloping intersections, bitsets for dense terms, and a smaller memory footprint.
