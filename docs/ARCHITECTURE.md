# Track C: architecture research and decisions

Sources read in full: `specs/c-join.md`, `reference/c_join.py`, `generators/c_join.py`, `tests/test_c_join.py`, the context file
(rules, board, FAQ). Where this document says "measured" the number is a board run (PERF.md has the run IDs); local numbers
(Docker VM on a Ryzen 5 3500U, no `perf`) are only used to count instructions with cachegrind and to rule ideas out.

## Facts that shaped the design

* The scoring box is an i5-13500, turbo off, containers on the 12 threads of the six P-cores; AVX2 and PCLMULQDQ, no AVX-512.
* 202 MB, 3.32 M records, about 61 bytes each; 240 countries; names come from a pool of about 100,000 and must be compared exactly.
* The quote state cannot be learned from a newline alone (277 k records have an LF inside a note), but for valid input every `"`
  toggles the state (a `""` toggles twice), so quote parity is exact under spec 2.1.
* M is the cgroup `memory.peak`, including pages first touched by the process: the name table is most of M.

## Pipelines compared

| | Idea | Verdict | Evidence |
|---|---|---|---|
| A | single pass, direct-indexed region table, per-thread totals, chunk boundaries by a safe method | **chosen**, with the boundary rule below | baseline 177.6 ms -> 52.6 ms |
| B | two passes: parallel quote count, prefix parity, then parse | rejected: reads 202 MB twice (the baseline did this). Replaced by finding a chunk's first record from the bytes after the cut, guarded by a per-chunk check and a fallback | tools/c_hostile_lf.py |
| C | columnar extraction + sort-and-reduce | not built: the aggregation is 241 groups and fits in registers/L1; sorting 3.3 M names costs more than the table | estimate |
| D | partitioned aggregation vs thread-local aggregation | totals are thread-local (241 x 56 B); the name table is shared (one structure, 7.4 MB) | PERF.md, diagnostic runs |
| E | sort-merge vs hash join | direct-indexed table: region ids are below 10^7 and the lookup's below 10^6, so a 2 MB array of codes beats any hash; the dense country index is derived from the packed 3-letter code, so the output order needs no sort | by construction |
| F | distinct names | exact: one 64-byte cache line per distinct name (length, 24 inline bytes, 256-bit country mask), byte-for-byte comparison, compact slot table in front. Rejected: per-thread tables merged at the end (baseline: 141 MB, serial merge), 64-bit hash alone (not exact), per-country sets of hashes (not exact, more memory) | board: M 141 -> 16 MB |
| G | input access | `pread` in 64 KiB blocks (per-worker buffer). `mmap` was 14% slower on Track A's board runs for the same reason (page-table work costs what the copy does) and its populate variants were slower still; not re-run here | Track A PERF.md |
| H | threading | 12 threads (the whole cpuset), dynamic 1 MiB chunks; Track A showed 8 and 18 threads both slower | Track A PERF.md |
| I | startup | no allocator tricks needed; fixed cost is 7.8 ms of 52 on the board (diagnostic run): regions, three `thread::scope`s, tables | PERF.md |
| J | SIMD CSV scanning | **chosen**: AVX2 compares for `"` `,` LF over 64-byte words, carry-less multiplication for the in-quote mask, separators outside quotes written as positions; records are five known boundaries | 576 -> 340 instructions/record |

## Decision

The design in PERF.md row 12 (T 52.6 ms, M 16.2 MB, S 604, K 23.90). The leaders at the time of writing are at 18-20 ms with S 303-365
(docs/COMPETITORS.md); the measured gap is in the name lookups (about 20 ms) and the parse (about 24 ms), see PERF.md.

## Spec 2.2 reliances

Listed in track-c/README.md with their checks and tests. In short: (1) an LF inside a quoted field is followed by a letter or quote
(chunk starts; checked by the separator test and the chunk-end test, fallback = one chunk); (2) at most 256 countries; (3) well-formed
fields (digits unvalidated, separators validated); (4) at most 2^17 names on the fast path (2^22 on the fallback).

## Contradictions between this brief's text and the official sources

None found that mattered. The brief's S reference for Track C was not given; solving the board's numbers gives S_ref of about 107 lines
and M_ref 367 MB (the reference's own M), which reproduces every K on the board to 3 digits.
