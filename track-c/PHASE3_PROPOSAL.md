# Track C phase 3: measured stage costs, decisions, and the proposal (Nov 1, 16:45 UTC)

Best official K is still 29.76 (`9fb5c93`). Labels: **board** (official runs), **local** (VM, 4 vCPUs, no SMT), **estimate**.

## 1. Board measurements (all on the control's source with one stage removed)

| Run | Removes | T | M | Source |
|---|---|---:|---:|---|
| control (3 runs) | | 35.73 / 36.59 / 36.73 ms (mean 36.35) | 13.5-13.78 MB | feed |
| `tc6_nomask` via `9c7e08a`, run-4f92ba5b0852 | the locked mask OR (the write prefetch stays) | **34.33 ms** | 14.21 MB | feed |
| `tc6_nomask` via `4fa0bee`, run-191a7780c05f (repeat) | same | **34.34 ms** | 13.82 MB | feed |
| `tc6_nolookup`, run-0b3959a3cbab | name lookup and mask | 24.70 ms | 9.96 MB | other session |
| `tc6_noresolve`, via run-c22361965e83 | name, region, totals | 21.91 ms | 7.61 MB | feed |
| `d5_read`, run-7cc802c10d14 | everything but reading and setup | 8.52 ms | 7.64 MB | other session |
| `d4_fixed` | everything but setup | pending | | |

| Stage | Board cost |
|---|---:|
| read + setup | 8.5 ms |
| parse | 13.4 ms |
| region + totals | 2.8 ms |
| name lookup (reads, compare, write prefetch) | about 9.6 ms |
| locked mask OR | about 2.0 ms (36.35 - 34.33) |

## 2. Decisions that follow from the rules set for this phase

* **O2 is not evaluated.** The board shows the locked OR at about 2 ms, below the 3 ms threshold. The migration O2 would also
  remove is partly inside the 9.6 ms (the write prefetch still takes ownership in `tc6_nomask`), but the rule was set on this
  diagnostic.
* **Thread pool: built, conditional on `d4_fixed`.** `pool_b` (`docs/track-c-candidates/pool_b_thread_pool.patch`, against
  `9fb5c93`) spawns the threads once for the regions, records and mask-count phases, with barriers in between. It is correct on
  9/9 fixtures, S 637 (+8). Local: neutral on full and sample, -0.7 ms (-10%) on tiny, where setup dominates (4 spawns; the board
  spawns 12). Submit it only if setup exceeds 2 ms.

## 3. The parse stage is plausibly throughput-bound on the board (estimate, from board numbers)

Instructions in the parse stage (valgrind, full seed 1): stage 1 about 290 M + stage 2 about 560 M = about 850 M. Board time for
it: 13.4 ms on 6 cores at 2.5 GHz with turbo off, i.e. 201 M core-cycles: **about 4.2 instructions per core-cycle**, near what a
Golden Cove core sustains with two SMT threads. Two consequences:

* R2's cut was mostly in stage 2 and the name compare: about 10% of the parse stage's instructions, i.e. about 1.3 ms. That is
  inside the board's ±1 ms per run, so R2's single null run does not refute instruction cuts. It only shows they are small.
* **Candidate A1 (one pass per record, no structural stage) was rejected locally (+23%) for its serial dependency chain.** That
  chain is a latency problem, which SMT exists to hide: the second thread on the core issues while the first waits. Locally there
  is no SMT. A1 runs **14% fewer instructions in total (about -230 M, all in parsing)** and 20 fewer lines. On a
  throughput-bound SMT core that predicts about -3.5 ms in parse (hypothesis, medium-low confidence). A1 is correct on all 9
  fixtures. **This is the board run with the highest information value**: a gain would make the sequential design the parse
  architecture to optimise further (about 150 instructions per record look reachable), and a loss would close that direction
  for good.

## 4. Proposal: the architecture to build if the board confirms

| Stage | Today (board) | Proposed | Basis |
|---|---:|---|---|
| read + setup | 8.5 ms | one thread pool (`pool_b`); regions parsed by the same pool | `d4_fixed` pending |
| parse | 13.4 ms | A1 sequential parse if its board run wins; the structural parser otherwise | A1 board run |
| name lookup | 9.6 ms | R1's one-vector compare (already part of A1's base) | R1 local -9.5% instructions |
| locked OR | 2.0 ms | keep (below threshold) | `tc6_nomask` |
| region + totals | 2.8 ms | keep | |

If A1 delivered -3.5 ms, R1 -1 ms and the pool -1 ms, T would be about 30.8 ms, K about 32.4 (S 615). **K > 40 would still not
be reached.** The board numbers say the gap to the leaders (whole run 17-19 ms) is spread over every stage. The only stage large
enough to move K by itself is parsing (13.4 ms), and it only moves enough if the sequential design proves out on the board and
can then be taken to about half its instructions.

## 5. Board runs requested (in order; each answers one question)

1. `d4_fixed` (already queued by the other session): splits the 8.5 ms, and decides `pool_b`.
2. **A1** (`docs/track-c-candidates/a1c_sequential_parser.patch` applied to R1 `7c64950`; valid output): does SMT hide the
   sequential parser's serial chain?
3. `pool_b`, only if (1) shows setup above 2 ms.

## 6. Diagnostic scheduling (recorded 16:55 UTC)

`tc6_nomask` is complete, with two readings, 34.334 ms and 34.338 ms. Both runs were Track D heads that carried the diagnostic's
Track C source. It is not to be repeated. The board never ran the diagnostic commits themselves (`9e7524d`, `f7037fd`), because
later pushes superseded them. A script that waits for a run of its own commit therefore waits forever. Readings are taken from
whichever run carries the source.
