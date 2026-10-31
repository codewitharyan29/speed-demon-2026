# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
`PERF.md` has every step with its board run, and the experiments that did not work.

## Approach

**Parse.** `graph.txt` is mapped with `mmap` and cut into 48 newline-aligned chunks that 12 threads parse at once.
Numbers are read eight bytes at a time (SWAR): the first byte below `'0'` ends a number, and three multiplications turn
up to eight digits into an integer. Each chunk's edge list is sized from the chunk's length, so it never reallocates.
While parsing, each edge bumps an atomic out-degree for its tail and marks its head as having an in-edge; self-loops
are dropped.

**First CSR.** A prefix sum turns the degrees into offsets, and the degree array is reused as the write cursor: edges
are scattered into place with `fetch_add`, each packed as `head << 32 | weight`.

**Renumbering.** Node ids are a random relabelling of a road lattice, so a search over the original ids misses the
cache on every neighbour. Each thread repeatedly takes an unclaimed node and grows a breadth-first region of up to
32k nodes around it. Regions are numbered one after another; with their offsets known, every region writes its new
ids, slots and degrees in parallel. The CSR is then rebuilt in the new numbering, so a search walks memory that is
mostly contiguous.

**Queries.** Each query is a forward Dijkstra from `s` that stops when `t` is popped, with a radix heap (valid
because Dijkstra's keys never decrease). A thread's distance array holds distance + 1 as `u32` (0 means unseen): it is
zero-filled lazily by the kernel and restored through a list of touched nodes, so a query costs only its own search.
Four bytes per node halve the memory 12 threads keep resident; the few searches whose distances pass 2^32 (they cross
edges of near-maximal weight) run again with `u64`, through the same generic search. `s == t` and targets without
in-edges are answered without searching. Queries run in order of their source's new id, each thread working through
its own contiguous share before helping the others, so a thread's searches reuse the same part of the graph in cache.
Answers are written in input order with one `write`.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

- A cheaper build: the scatter, the region growth and the rebuild still read and write the edge list three times
  (about 70 ms locally); a parallel radix sort by source could replace the atomic scatter.
- A leaner search kernel: the relaxation test is the main mispredicted branch, and the distance loads the main cache
  misses. Bidirectional search did less work but ran slower here (see `PERF.md`), so goal-directed search with cheap
  landmarks is the next idea to measure.
