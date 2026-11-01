# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
`PERF.md` has every step with its board run, and the experiments that did not work.

The solution is one C++23 file (`src/main.cpp`, no dependencies) built with `-O3 -march=x86-64-v3 -flto`.

## Approach

**Packed CSR in the L3 cache.** 99.93% of edge weights are below 2047 and node ids fit in 21 bits, so an edge is one `u32`:
`(target << 11) | weight`. The edge array is 19 MB instead of 40 MB and stays inside the scoring CPU's 24 MB L3.
The 0.07% of edges with weight >= 2047 store the sentinel `2047`; their true weight is in a small sorted table.

**Two parallel passes over the mapped file.** The body is cut into 48 newline-aligned chunks that 12 threads take dynamically.
Pass 1 skips each line with an 8-byte SWAR newline search and only counts the source's out-degree; pass 2 parses `u v w` and
writes the packed edge straight into its slot. Then `MADV_DONTNEED` drops the file's page cache before the rest of the build.

**Locality renumbering.** Nodes are renumbered by breadth-first regions of up to 8192 nodes (claimed with atomic bit tests), so a
search touches nearby memory. Edges are then scattered into the final CSR in the new ids.

**Search.** Per query, Dijkstra with a Dial queue: a ring of 4096 buckets (edge weights in the fast path are at most 4095),
pooled linked lists, and a two-level bitmap (a 64-bit summary over 64 words) that finds the next non-empty bucket with
`rotr` + `tzcnt`. Distances are 16-bit and stored inverted, so zero-filled pages mean "unseen" and the search resets only the
nodes it touched. A search whose distance would reach 60000, or that meets a weight the ring cannot hold, is answered again by an
exact 64-bit Dijkstra (6-7 queries on the real inputs).

**Pruning.** Queries with `s == t`, a target with no incoming edge, or a source with no outgoing edge are answered without a
search. The target test matters: a search for an unreachable target would otherwise cover the whole graph.

**Scheduling.** 12 worker threads each start on their own slice of the queries (sorted by source, for cache reuse) and then take
work from the other slices. Build buffers are unmapped before the searches start, and the process ends with `_exit`.
