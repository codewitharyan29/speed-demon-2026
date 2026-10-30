# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

## Approach

The input is mapped with `mmap` and cut into 48 newline-aligned chunks that 12 threads parse in parallel by hand
(no UTF-8 validation, no allocation per line). While parsing, each edge bumps an atomic out-degree for its tail and
marks its head as having an in-edge. A prefix sum turns the degrees into CSR offsets and the edges are scattered into
place with `fetch_add` slots, each edge packed as `head << 32 | weight`.

Node ids are a random relabelling of a road lattice, so a search over the original ids misses the cache on every
neighbour. The nodes are renumbered by growing breadth-first regions of about 32k nodes, one per thread at a time,
and the CSR is rebuilt in the new numbering. A search then walks memory that is mostly contiguous.

Each query is a forward Dijkstra from `s` that stops when `t` is popped, with a radix heap (valid because Dijkstra's
keys never decrease) and u64 distances. Per-thread state is a zero-filled `dist` array holding distance + 1, restored
through a list of touched nodes, so a query costs only its own search and untouched pages never use memory.
Queries where `s == t`, or where `t` has no in-edge (which would make a forward search settle the whole map), are
answered without searching. Answers are collected in query order and written with a single `write`.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

PGO with a profile trained on generator output, a measured bidirectional search, and sorting queries by expected cost.
