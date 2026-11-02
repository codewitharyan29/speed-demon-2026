# Track C: architectures beyond cx5, and the board diagnostics that decide between them

cx5 (`66693b1`, main.cpp sha256 `e016cdf08682676b`) stays the production baseline: board K 31.88 (T 34.514 ms, M 14.139 MB, S 432).
Labels used below: **[local]** a measurement on the KVM VM (4 vCPUs, no SMT, 260 MB L3; not comparable with the board),
**[board]** an official measurement, **[sim]** a Python simulation of the data structure, **[derived]** arithmetic on the above.
No local timing is used as a board prediction. Nothing here was submitted.

## 1. What the local measurements say (all [local], full seed 1, valid builds, interleaved medians)

Marginal cost of one stage = time of a build that repeats that stage's work (output unchanged and identical to the published hash)
minus cx5. Sources: `docs/track-c-cpp-candidates/diag_{read2,struct2,parse2,table2}.cpp`.

| Repeated work | 1 thread | 4 threads (cx5 wall 68.5, CPU 255) |
|---|---:|---:|
| second `pread` of every block from a cold offset (`READ2`) | +47 ms | +12.9 ms (19% of wall) |
| second structural pass (`STRUCT2`) | +16 | +7.7 (11%) |
| second record parse incl. hash and prefetches (`PARSE2`) | +47 | +15.9 (23%) |
| second lookup + mask update on a line that is now hot (`TABLE2`) | +53 | +11.7 (17%) |

Replacing the lookup by one direct access to a random entry line (output not exact; timing only): 1 thread 217 -> 181 ms,
4 threads wall 69.8 -> 56.6 ms (-19%). With the line always in L2 (4096 lines): 165 ms. With a plain instead of a locked update:
-8 ms. Reading that: the cost of the exact lookup is mostly its instruction path (tag probe, hint, key rebuild and compare,
about 36 ms at one thread), the cache miss on the entry line is about 16 ms, the lock about 8-12 ms.
Per record, 90% are the first occurrence of their (name, country) pair (2,980,424 of 3,320,000 from the reference output), so
testing the bit before setting it, or any pair-level cache, removes at most 10% of the work.

## 2. Table-structure alternatives for exact lookup [sim], 100,000 names, 2^17 slots of 64 B (the same 8 MB)

| Scheme | Result |
|---|---|
| linear probing, no tags | name in its home slot 62%, within 1 slot 77%, mean 2.59 lines touched per lookup |
| linear probing + 16 tags (cx5) | exactly 1 entry line per lookup; 1.8% of names beyond the 16-tag window |
| two choices h1, h2 (1 name per line) | at h1 62%, at h2 19%, neither 19% |
| 128 B buckets of 2 | 19% of names overflow |
| 256 B buckets of 4 | 11% of names overflow |

So no tagless exact scheme gets one line per lookup at this load factor; the tag array is what delivers it. A table that is
bigger lowers the collisions but costs 1.5% of K per MB. Rejected by simulation, no implementation needed.

## 3. Architectures

Common to all: identity stays exact (a 32-byte key compared in full; names over 31 bytes through the spill area). cx5 reference
counts per record: random accesses 3 (region byte, tag line, entry line), 1 dependent pair (tags -> entry), 1 locked RMW on a line
that was last written by another core 88% of the time (earlier measurement in `docs/track-c-locality`).

### A. Exchange-sharded private tables (owner computes)
* **Flow.** Each worker parses a block and writes (32-byte key, country, hash) = 40 B per record into a staging line for
  `owner = hash mod 12`; a full 4 KiB buffer is published to the owner through a single-producer ring (release store of the
  tail, acquire load by the owner). The owner probes only its shard: 100,000/12 = 8,300 names = 0.53 MB of entries + 11 KB of tags,
  and sets the country bit with a plain OR. Aggregates stay per worker.
* **Exactness.** The key block is compared in full at the owner; names over 31 bytes would need their bytes carried in the buffer
  (variable-length record) or a separate slow shard.
* **Memory.** Tables unchanged (8 MB in total). Staging 12 x 12 x 4 KiB = 0.56 MB and rings about the same: M +1.1 MB, K x0.985 [derived].
* **Random accesses / dependent loads.** Producer: 1 (region byte), 0 dependent. Owner: 2 (tags, entry), both L2-resident on the
  board's 1.25 MB L2 (an assumption; the VM has 2 MB L2 and 260 MB L3 and cannot show it). Locked operations 0; line transfers of
  mask lines 0; ring lines about 32,000 handoffs of 4 KiB.
* **Cost.** +25-30 instructions per record (copy out, copy in, ring bookkeeping), +1.1 MB. The Rust prototype of this idea (O2)
  measured +10.8% at 4 threads [local]; the lock plus ownership transfers it removes are 8-14% [local, above]. Local net: about 0.
* **Projection [derived].** At net -5% T: K 32.7; at -8%: 33.2 (+0.1 for nothing else changing); to reach 35 it would need -17%.
  The only argument for more is the L2 residency on the board, which is exactly what the VM cannot show.
* **Cheapest experiment that can reject it.** Board diagnostic `TABLE2` (section 4): if the whole second hot lookup+update costs
  under 3.5 ms on the board, the lookup path cannot exceed 10% of T and A is rejected without writing it.
* **Data-race freedom.** Ring words are written by one thread and published by a release store; the consumer reads them after an
  acquire load of the tail. Shard tables are owned by one thread each. No atomic RMW is needed except the termination count.

### B. SMT-paired ingest: one thread reads, its sibling parses
* **Flow.** Threads are paired by hyperthread siblings (`/sys/devices/system/cpu/cpu*/topology/thread_siblings_list`). Six reader
  threads `pread` 64 KiB blocks into a two-slot ring held in the pair's shared L2; six parser threads consume the slots (structure,
  records, lookup, aggregates). Chunk and record logic is unchanged, only who does the `pread`.
* **Why.** Local READ2 says reading is 19% of wall at 4 threads; a thread in a kernel copy stalls on memory and leaves issue slots
  to its sibling. Today all 12 threads alternate between the two phases.
* **Memory.** 6 pairs x 2 x 64 KiB = 0.75 MB, replacing 12 x 32 KiB: M +0.4 MB, K x0.994 [derived].
* **Random accesses.** Unchanged (3). Added: one L2-to-L2 handoff per 64 KiB block; 3,100 handoffs in total.
* **Projection [derived].** If the read is half hidden: -9% T -> K 33.4; fully hidden (the ceiling): -19% -> K 35.4.
* **Why it may fail.** Parser threads would carry 81% of the work on 6 threads. Whether a core running a stalled reader plus a parser
  beats two mixed threads is a property of the i5-13500 that no local run shows (no SMT here), so local runs can only prove it correct.
  If SMT on the board yields little in total, the pair gains nothing.
* **Cheapest experiment that can reject it.** Board diagnostic `READ2`: it gives the true cost of reading 202 MB on the board with
  12 threads. If that is under 4 ms (12% of T) the ceiling is below K 34 and B is dropped.
* **Data-race freedom.** Two atomics per slot (full/empty), release on publish, acquire on consume; the data in a slot is written by
  the reader and read by the parser only after the acquire.

### C. Columnar blocks with batched probing
* **Flow.** Structure a block, then run each field as its own tight loop over all of the block's records (ids, region ids, amounts
  as numbers; name keys and hashes), then probe 16 hashes at a time.
* **Evidence [local].** cx13 (three passes over the block) and cx14 (cheaper boundary pass) were correct and 3-4% slower: splitting
  a record into passes costs more in intermediate stores and loads than the longer independent loops save, and halving stage 1
  (cx14) did not change time. The per-record instruction budget (538, cachegrind) is spread over about 40 source lines.
* **Projection [derived].** Best case a 100-instruction cut (parse numbers across records) is about -8% at one thread; cx13's
  measured +4% is the evidence against it. Not pursued.

### D. Transposed bitmaps (one bit per (country, slot))
* 240 x 16 KiB = 3.9 MB bitmaps plus a read-only key table of 4 MB: the same 8 MB, but **two** random lines per record (key line
  and bitmap line) and the same lock/coherence traffic on the bitmap lines. Strictly worse than cx5 by access count; rejected by analysis.

### E. Tagless slot tables
* Rejected by the simulation in section 2.

## 4. Board diagnostics prepared (not submitted)

All four have **correct output** (the published full hash `d8088b1c...`, 36 of 36 local fixtures); each repeats one stage's work, so
the extra board time is that stage's cost with the real memory system and 12 threads. They are slower than cx5 by construction, so
they cannot be promoted; they need your approval to be run (one run each, 4 runs). Sources, each cx5 plus one `#define`:

| File | Repeats | Board question it answers | Decision it feeds |
|---|---|---|---|
| `diag_read2.cpp` | a second `pread` of every block from a cold offset | cost of reading and copying 202 MB at 12 threads | B: continue only if >= 4 ms |
| `diag_struct2.cpp` | the structural pass | board cost of stage 1 | any parser-side redesign |
| `diag_parse2.cpp` | the record parse, hash and prefetches | board cost of stage 2 | any parser-side redesign |
| `diag_table2.cpp` | the lookup and the bit set, on a now-hot line | instruction/lock cost of the table path on the board (misses are not repeated) | A: continue only if >= 3.5 ms |

Local deltas for the same builds are in section 1 so that a board delta can be compared with them stage by stage. The sum of the
four local deltas at 4 threads is 48 of 68.5 ms; the rest (cache misses on the entry line, ownership transfers, fixed costs) is
not covered by any of them.

## 5. Board diagnostics, run Nov 2 (approved by the user; diagnostics only, never promoted)

Each ran the exact source in `docs/track-c-cpp-candidates/diag_*.cpp` (cx5 plus one `#define`) as `track-c/src/main.cpp` on
`speed-demon` (team Bharat Labs). All gates passed for all four (check, build, correctness, timeout, sandbox): output equals the
reference. After the four, cx5 was restored (commit `830b2e6`, tree identical to `41735ce`, main.cpp sha256 `e016cdf08682676b`).

| Run | Commit | Source sha256 (first 16) | Board T | M | S | Official K (not meaningful) |
|---|---|---|---:|---:|---:|---:|
| cx5 reference (4 runs of the same source) | `66693b1`, `da56952`, `41735ce`, `830b2e6` | `e016cdf08682676b` | 34.514 / 34.706 / 34.699 / 34.773 ms (mean **34.673**, spread 0.26) | 13.86-14.14 MB | 432 | 31.86-31.92 |
| diag_read2 `run-06073db82245` | `417d6bd` | `32b1ec2b84eb9c33` | 37.888 | 14.01 | 457 | 30.23 |
| diag_struct2 `run-66f42f9d1b90` | `7333749` | `b3e1e25c7430c4d4` | 38.895 | 13.77 | 457 | 29.94 |
| diag_parse2 `run-8dd98bf7319c` | `2daa676` | `abc9d55eccf4dd50` | 46.261 | 14.15 | 457 | 27.30 |
| diag_table2 `run-b8818122f2c7` | `8ba2758` | `f85705d59b4ea3bc` | 42.586 | 13.86 | 457 | 28.57 |
| cx5 restored `run-0d2d7eecb6f1` | `830b2e6` | `e016cdf08682676b` | 34.773 | 13.91 | 432 | 31.86 |

Output hash: the board scores correctness against the reference and all four passed; the feed does not publish the output hash.
The same builds produce the published full-seed-1 hash `d8088b1c...` locally.

**Marginal cost of repeating each stage [board], against the cx5 mean of 34.673 ms (run-to-run spread 0.26 ms):**

| Stage repeated | Board marginal | Share of T | Local 4-thread marginal (VM) | Share of local wall |
|---|---:|---:|---:|---:|
| `pread` of 202 MB (cold offsets) | **3.21 ms** | 9.3% | 12.9 ms | 19% |
| structural pass | **4.22 ms** | 12.2% | 7.7 ms | 11% |
| record parse + hash + prefetches | **11.59 ms** | 33.4% | 15.9 ms | 23% |
| lookup + bit set on a now-hot line | **7.91 ms** | 22.8% | 11.7 ms | 17% |
| sum | 26.94 ms | 78% | | |
| not attributed (entry-line misses, line ownership transfers, startup, regions, tail) | 7.74 ms | 22% | | |

The board and the VM disagree about the read (3.2 ms against a local 12.9 ms) and about the parse (a third of T on the board). A
marginal cost is an upper bound on what a stage can give back, and assumes its repetition does not overlap with other work.

## 6. Decision

* **B (SMT-paired ingest): rejected.** Reading costs 3.21 ms. Hiding all of it gives T 31.46 and K 33.49 (exact formula, M 13.91,
  S 432); at 50% overlap K 32.66. K35 is out of reach even at the ceiling, and the threshold in the plan (4 ms) was not met.
* **A (owner-sharded tables): not built.** The whole hot lookup-plus-lock costs 7.91 ms; removing locks and line transfers can
  return part of that, and the exchange adds 25-30 instructions per record at the board's measured parse rate (about 3.9 ms per 100
  instructions per record), i.e. about +3.5 ms. Net about zero.
* **Where the board says the time is:** record parse 11.6 ms, table path 7.9 ms, structural pass 4.2 ms, read 3.2 ms, unattributed 7.7.
  K35 needs T 28.8 ms: -5.9 ms. Exact-formula projections: parse -40% (-4.6 ms) K 34.3; parse -50% K 34.95; parse -40% with
  structure -40% and table path -30% (-8.7 ms) K 36.9.
* **Next experiment:** cut the record-parse stage (about 300 of 540 instructions per record) by 40% or more with real instruction
  reductions, then the table path; measure with cachegrind first, then wall time, then a board run if K is projected above 31.9.

## 7. Two more board diagnostics, Nov 2 (approved; correct output; never promoted)

| Run | Commit | Source sha256 (first 16) | Board T | M | S |
|---|---|---|---:|---:|---:|
| diag_regions2 `run-f357e2cac587` | `58dcca3` | `f78c34bc77747b96` | 35.498 ms | 13.91 MB | 453 |
| diag_random_entry `run-9ecc068437c0` | `9ac7e0f` | `94e0fafe325cd55c` | 39.461 ms | 14.19 MB | 454 |
| cx5 restored `run-50a0990a2092` | `851376d` (tree = `830b2e6`) | `e016cdf08682676b` | 35.179 ms | 13.73 MB | 432 |

All gates passed (correctness against the reference included). Locally the same builds give the published full hash `d8088b1c...` and
pass 36/36 fixtures. `diag_regions2` runs the regions phase twice (its writes are idempotent). `diag_random_entry` prefetches one more
random entry line per record at the same pipeline stage as the real one and reads one word of it afterwards (no writes).

**cx5 reference, now 5 runs of the identical source: 34.514, 34.706, 34.699, 34.773, 35.179 ms; mean 34.774, sd 0.246, range 0.67.**
(The earlier table used four runs and a mean of 34.673; with the fifth run the marginals of section 5 change by 0.1 ms: read2 +3.11,
struct2 +4.12, parse2 +11.49, table2 +7.81.)

| Diagnostic | Marginal vs the cx5 mean | Share of T | vs the cx5 spread |
|---|---:|---:|---|
| regions phase repeated | +0.72 ms | 2.1% | 0.32 ms above the slowest cx5 run, 2.9 sd: at the edge of noise |
| one extra random entry line per record | +4.69 ms | 13.5% | 4.28 ms above the slowest cx5 run: well outside noise |

Sum of the six marginals: 31.94 ms = 92% of T; 2.8 ms unattributed (startup, tail, effects that do not add).

**What these numbers do and do not say.** A repeated phase or an extra access estimates the marginal cost of adding that work to
a run that is already doing everything else; it does not isolate hardware cache misses from line-ownership transfers (the extra
read may pull a line out of another core's cache), from the 6 extra instructions per record, or from memory-level parallelism that
the extra stream steals from the real one. Stages overlap, so marginal costs need not sum to T (here they do, to within 8%).
The extra line read is a *read* of a line that other threads write: it measures miss plus transfer, not misses alone.

**Decision.**
* Regions: 0.7 ms at most (K +0.3 if it were free). Not significant; no work on region setup.
* Entry line: one random 64-byte line per record is worth up to 4.7 ms (K up to 34.25 if it cost nothing, which no exact table can
  achieve). A cache-conscious layout can only recover part of it. Because 90% of (name, country) pairs are first occurrences, there
  is no reuse to exploit, and a table small enough to stay in L2 (1.25 MB per core pair) cannot hold 100,000 exact names; only a
  sharded table (architecture A) could, at a measured-instruction cost of about +3.5 ms. Realistic gain: 0-1.5 ms. Not pursued now.
* The measured bottleneck is the record parse (11.5 ms, 33%) and, behind it, the lookup path (7.8 ms) and the structural pass
  (4.1 ms). Work continues on the parse.

## 8. Parse diet cx16 (local, Nov 2): rejected

`docs/track-c-cpp-candidates/cx16_parse_diet.cpp`: branch-free `num8` for 1-8 digit numbers (generic loop only beyond 8), fields stored
directly instead of building a 48-byte aggregate, and `quotes(e1)` carried from the previous record's `quotes(e3)` (two prefix-count
lookups per record instead of three). Correct on 9/9 fixtures (normal, adversarial, hostile-LF, stress, twins); S 442.
**Cachegrind, sample seed 1, [local]: 198.5 M instructions against 199.8 M for cx5 (-0.7%); mispredicts 323,779 against 323,522.**
GCC had already produced most of these savings from the cx5 source; the roughly 300 instructions of `record()` are spread over about
40 lines (3 number parses about 80, quote counts about 34, struct build about 42, checks about 27, key and hash about 15). No wall-time
measurement was made since the work is unchanged. Not submitted.

What this implies for the parse bottleneck [board: 11.5 ms for the record stage]: a cut of 40% (K 34.3) needs about 120 fewer
instructions per record, which no local rewriting of this formulation removed. The formulation itself (separator array from
stage 1, then per-field scalar decoding) would have to change, and the measured attempts at that (cx12-cx14) were not faster.

## 9. Broadened search, Nov 2 (local VM; the VM was restarted and ran 25-40% slower than before, so only interleaved comparisons are used)

Sources in `docs/track-c-cpp-candidates/`. All are correct on 9/9 fixtures unless noted.

| ID | What | Instructions (cachegrind, sample) | 4-thread wall vs cx5 (interleaved) | Decision |
|---|---|---|---|---|
| cx17 (`cx17_vector_numbers_gather.cpp`) | cx14 boundaries + vector number parse of 4 records at a time (`vpgatherqq`, `vpsllvq`, `vpmaddubsw`, `vpmaddwd`) for id, region id, amount | 202.4 M vs cx14 208.3 M | +17% (88.9 vs 75.7) | rejected: the gather is slow (-30 ms against scalar loads), and filling the offset/count arrays costs what the vector arithmetic saves |
| cx17s (`cx17s_..._scalar_loads.cpp`) | same with four scalar loads instead of the gather | similar | +7% (80.5 vs 75.3); equal to cx14 (79.3) | rejected |
| cx18 (`cx18_lookup_diet.cpp`) | 8-byte SWAR tag probe in `warm()`; key block carried in the record | | +3% (81.0 vs 78.8); split: SWAR probe alone +3%, key in record alone 0% | rejected |
| cx19a (`cx19a_no_checks.cpp`) | cx5 without the per-record validation (separator-type check, BOM bytes, field bounds) | 184.2 M vs 199.8 M (-7.8%) | **-4.9% in both of two interleaved rounds of 80 runs** (82.8 vs 87.1; 78.5 vs 82.6) | not submitted: also removes the fallback for a chunk cut inside a quoted field |
| cx19b (`cx19b_fewer_checks.cpp`) | cx5 without the BOM-byte check and the field-bounds check; the separator-type check stays (the fallback for a chunk cut inside a quoted field still fires) | 192.5 M (-3.7%) | **-2.7% and -2.8%** (84.9 vs 87.1; 80.3 vs 82.6) | candidate |

Reading: removing instructions that the per-record work does not need is the only change here that moved wall time, about 0.6% of
time per 1% of instructions on the VM; the board is expected to be at least as sensitive (SMT pairs share issue width, and the
parse stage is 33% of board T), but that is an inference, not a measurement.
