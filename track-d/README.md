# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
`PERF.md` has every step with its board run, and the experiments that did not work.

One C++23 file (`src/main.cpp`, no dependencies), built with `-O3 -march=x86-64-v3 -flto`. It is stored as `clang-format`
(LLVM style) output, because the board counts the lines of the formatted file.

## Approach

**One parallel pass over the mapped file.** The body is cut into 48 newline-aligned chunks that 12 threads take dynamically.
A thread parses 16 lines, prefetches the counters they touch, then does the atomic increments, so the cache misses overlap
instead of following one another (this alone took 15 ms off the build).

**Four slots per node.** Out-degree is 2.9 on average and 97% of nodes have at most four edges, so the first four edges of a node
go straight to `e4[4u + c]` as `target << 11 | weight` (a `u16` counter says how many arrived); the remaining ~1% go to a spill
list. That removes the degree-counting pass and its prefix sum, and later passes read one 16-byte row per node. Weights of 2047 and
above store the sentinel 2047 and keep the real value in a table.

**Locality renumbering.** Nodes get new ids from breadth-first regions of up to 8192 nodes (claimed with atomic bit tests, spill
edges ignored since only the order of regions depends on them), so a search touches nearby memory. A scatter then writes the
forward graph as a CSR with 4-byte edges (19 MB, inside the 24 MB L3).

**Reverse graph.** The same graph with every edge reversed, in the final ids, again as four-slot rows built in one pass.

**Bidirectional search.** The target is the r-th node settled from the source, so two balls of half the radius hold about half
the nodes: the local query phase went from 96 to 40 ms. Each direction has a Dial queue with buckets 16 distance units wide
(nodes of a bucket in any order; a node is skipped when its own distance is in an earlier bucket). The side whose lowest
possible distance is smaller grows; the search ends when the two bounds add up to the best meeting. A result below 60000, or
an exhausted search that skipped no edge, is exact; anything else (distances that do not fit 16 bits) is answered by an exact
64-bit Dijkstra, 6-7 queries on the real inputs.

**Distances.** Forward and backward distances share one 32-bit word (low and high half, stored inverted so zero means unseen),
kept in 512-entry pages taken from a per-worker bump pool on the first write; page 0 stays zero and backs every unseen page, and
the pages a query used are cleared at its end. A meeting check therefore costs no extra cache miss, and the query phase needs
only a few MB per worker.

**Pruning and scheduling.** Queries with `s == t`, a target without incoming edges, or a source without outgoing edges are
answered without a search (an unreachable target would otherwise make a one-directional search cover the whole graph). The
queries are sorted by source; each of the 12 workers starts on its own slice and then helps with the others.

**Memory.** Transparent huge pages are switched off for every allocation: with THP set to `always` (the board's behaviour, as far
as its memory numbers show) the sparsely used per-worker regions are charged in whole 2 MB pieces, about 35 MB for 12 workers.
The peak is the build's (the five arrays alive during the scatter), about 65-70 MB.
