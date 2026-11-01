# Track C: shared name table, mask writes and memory, one level deeper

No new official result; the best official K is still 29.76 (`9fb5c93`). Every number here is either a deterministic count from
the input, an existing board measurement from PERF.md, or a local VM measurement (labelled). Local timings are not used to rank
board-level designs.

## 1. Evidence

### Board measurements already in PERF.md (labelled diagnostics: wrong output on purpose, T and M reported)

| Diagnostic | Code then | Board T | Board M | What it measured |
|---|---|---:|---:|---|
| exit at the top of `main` | | 0.72 ms | 8.14 MB | process floor |
| exit after the region table | | 3.54 ms | 8.63 MB | regions.csv about 2.8 ms (before the SWAR parser) |
| no record parsing | | 7.8 ms | 17.8 MB | fixed cost then (included an 8 MB memset since removed) |
| no name lookups | a1294ea, T 71 | 44.3 | 22.0 | lookups 27 ms of 71 (38%) |
| no name lookups | fa128ea, T 55 | 31.5 | 8.9 | lookups 23.5 of 55 (43%); **the name table is 7.4 MB of M** |
| no mask update, no write prefetch | T about 55 | 48.9 | 16.3 | reading the table 17 ms, mask writes 6 ms |

So the diagnostic method works on the board. Board M follows the name table's resident size. And the name table (lookups plus
mask writes) has been about 40% of board T in every measurement.

### Deterministic: the contention pattern of the mask writes (full seed 1, 12 workers taking 1 MiB chunks)

* 3.22 M repeat writes to a name's line; **88.4% of them find the line last written by another worker.**
* The gap between two writes of one line: p10 9.3 k, median 67 k, p90 224 k records. That is about 0.1-2 ms, far longer than
  any write.
* Hence there is **no lock contention** (no two workers on one line at once). What the board pays is **migration**: almost every
  locked OR needs a line that is Modified in another core's cache, a cross-core transfer, about 3.2 M of them. A read-only
  line would come from L3 instead.

### Footprint per worker on the board (i5-13500: 6 P-cores, 1.25 MB L2 each shared by 2 SMT threads, 24 MB L3)

| Structure | Size | Per core, shared now | If partitioned by owner |
|---|---:|---|---|
| name entries (key 24-32 B + mask 32 B) | 8 MB (2^17 x 64 B) | the whole 8 MB, random | keys 4 MB read-only (shared, S state) + masks 4 MB / 12 = 340 KB per owner, 680 KB per core |
| tags | 128 KB | whole | whole (read-only) |
| region table | 1 MB | whole | whole (read-only) |
| block, separators, quote words | about 60 KB per thread | private | private |

With 2 SMT threads per core, a core holds two workers' private data plus the shared tables' working set in 1.25 MB. Only the
partitioned masks (680 KB per core) could stay L2-resident; the 8 MB of shared entries cannot under any layout.

## 2. Bottleneck model of the current board time (control, T about 36 ms)

| Part | Estimate | Basis |
|---|---:|---|
| fixed (process, regions, tables, merge, output) | 4-5 ms | 3.54 ms measured after regions in an old run, minus the SWAR regions saving, plus tables and output |
| reading 202 MB with `pread` | about 6.5 ms | Track A's board read-only run: 239 MB in 8.4 ms including startup |
| parse, region lookup, totals | 10-12 ms | remainder |
| name table: reads | 9-11 ms | 17 of 55 ms then, scaled by the share it had; B's direct table removed one random line since |
| name table: locked writes and migration | 4-6 ms | 6 ms measured then; the pattern above is unchanged since |

Uncertainty is about ±3 ms on each of the last three rows, which is why the two targeted diagnostics below are needed.

## 3. Candidates investigated

| Option | What it does | Result or analysis | Decision |
|---|---|---|---|
| read-only replication of the whole table per worker | each worker has its own copy | 12 x 8 MB: M +88 MB, K x 0.63 | rejected (M) |
| thread-local cache of hot names | a small private map in front of the shared table | names are uniform over 100 k (no hot set); a 4 k-entry cache would hit about 4% | rejected |
| per-thread private masks, merged at the end | no shared writes | 12 x 3.2 MB = +38 MB, K x 0.85 | rejected (M) |
| per-thread log of (name, country), merged by owners at the end | no shared writes | 3.32 M x 4 B = +13 MB, K x 0.94 | rejected (M) |
| non-atomic mask OR on shared lines | no locked instruction | a data race: two workers updating one word lose a bit. The gaps make it rare but not impossible | rejected (correctness) |
| test the bit before the locked OR | skip 10% of the ORs | +3% locally (branch) | rejected (measured) |
| sharded table with a lock per shard | | adds the very synchronisation that is the cost | rejected |
| NUMA placement | | the board is one socket, one memory domain | not applicable |
| **O2: identity read-only, membership owned (prototype built)** | keys in a 32 B read-only table; masks in their own table, each range written by one worker only; workers send `slot << 8 \| country` words to the owner through single-producer/single-consumer rings, 16 per cache line | see below | **built, correct, needs a board run** |

### O2 in detail (`docs/track-c-locality/o2_owner_masks.patch`, against R1 `7c64950`)

* Every record still does an exact key compare (R1's 32-byte block) on a line that is never written after the name is inserted,
  so the line stays shared. No locked instruction remains: no `lock or`, and no `prefetchw`. Each record becomes one 4-byte word
  in a staging line for its owner, and 16 words move as one cache line.
* That is 3.2 M cross-core line migrations replaced by about 0.2 M ring-line transfers. The owner ORs into masks that only it
  writes (340 KB per owner on the board).
* Correctness: the rings are single-producer/single-consumer with Release on `tail` and Acquire when reading it, and the same in
  reverse for `head`. A full ring makes the sender apply its own inbound words while it waits, so no cycle of waiting workers can
  form. Termination: each worker flushes its staging lines, then increments `done` (Release). Workers apply their inbound rings
  until `done` equals the number of workers (Acquire), then make one final pass, which sees everything sent. Padding words point
  at one spare mask past the table, so the apply loop has no branch.
* Tests: identical to the independent reference on tiny, sample and full seed 1, adversarial, hostile-LF (the single-chunk
  fallback, where idle workers must keep draining), stress1-3 and twins.
* Memory: keys 4 MB + masks 4 MB = the same 8 MB as now, plus 12 x 12 rings x 4 KB = 576 KB.
* S: 754 + 6 = 760 (+131 over the control, -2.9% K).
* Local VM (4 threads, 50 interleaved runs): 73.0 ms against 65.9 for R1 (+10.8%); 2 threads +13%. Its instruction overhead
  is +25 per record (single worker). The VM cannot show the gain O2 is built for: with 4 owners each mask range is 1 MB rather
  than 340 KB, and there are 4 cores, not 6 cores with 12 threads. This is therefore evidence of O2's cost only, not of its
  benefit.

## 4. Proposed architecture for K > 40, and what it can and cannot reach

| Stage | Change | Board T effect (model) | M | S |
|---|---|---:|---:|---:|
| 1 | O2: read-only keys, owned masks, rings | -4 to -6 ms (writes) + 1 to 3 ms (overhead) | +0.6 MB | +131 |
| 2 | dense ids: masks 100 k x 32 B and keys 100 k x 32 B, reached through the tag table | -0 to -2 ms (smaller lines) | -1.6 MB | +30 |
| 3 | one thread pool for all phases, compact region table | -1 to -2 ms | -0.6 MB | +30 |

Best case of the model: T ≈ 27-28 ms, M ≈ 11.9 MB, S ≈ 820, so K ≈ 29.76 x (36/27.5)^0.5 x (13.5/11.9)^0.2 x (629/820)^0.15 ≈
**33**. K > 40 needs T ≈ 19.9 ms at today's M and S, or about 23 ms with M 10 MB. Even removing the whole name table (about
15 ms by the model) leaves T ≈ 21 ms at the same M and S, which is K ≈ 39. **No design measured or modelled here reaches K > 40.**
Reaching it would need both the name-table cost cut by more than half and parse plus read under about 13 ms together. The board
does not show that the second is possible: R2's instruction cuts did not move board T.

## 5. Exact evidence needed before committing to the rewrite (two board runs, not five)

Both are diagnostics on the current control source, applied from `docs/track-c-board-diagnostics/`:

1. `tc6_nomask.patch` (no locked OR; the lookup and its write prefetch stay, so the migration of the line is still paid): board T
   gives the lock cost alone.
2. `tc6_nolookup.patch` (no name lookup at all): board T gives the whole name-table cost, which is the ceiling of any redesign.

Decision rule:

* If (2) shows the name table at 12 ms or more of 36 **and** (1) shows the lock at 3 ms or more: build stages 1-2 and validate O2
  itself on the board (a valid run, two runs).
* If (2) is below 8 ms: the name table is not where K > 40 is, and no partitioning is worth +130 lines. The remaining candidates
  are then in reading and parsing, which must be measured with `tc6_noresolve` and `d5_read` first.

These runs replace the control on `speed-demon` for their duration; that decision, and the coordination with the session that
manages `speed-demon`, is yours.
