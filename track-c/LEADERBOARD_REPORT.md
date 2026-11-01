# Track C: leaderboard gap analysis, board evidence and experiment registry (Nov 1, 16:05 UTC)

Labels: **measured** (board feed or board run), **local** (KVM, 4 vCPUs, no SMT; it does not rank board designs),
**estimate** (model), **hypothesis**.

## 1. Leaderboard (measured; public feed board.json generated 2026-11-01T15:57:34Z, each team's best scored run)

Reproduce with `curl -sS -o board.json https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json`, then
`python3 -I tools/c_board_analysis.py board.json`. 105 teams have a scored Track C run.

| # | K | T ms | M MB | S | lang | team |
|---:|---:|---:|---:|---:|---|---|
| 1 | 48.54 | 17.36 | 9.32 | 451 | Rust | SALAAR |
| 2 | 47.50 | 18.02 | 12.75 | 303 | C++ | NickTrapp |
| 3 | 46.27 | 19.05 | 10.41 | 393 | Rust | ALL STAR |
| 4 | 46.14 | 19.58 | 10.15 | 378 | C++ | another ello |
| 5 | 45.75 | 19.48 | 11.19 | 357 | Rust | Cause Of Death |
| 6 | 44.47 | 21.96 | 11.06 | 294 | Rust | AVENGERS |
| 7 | 42.78 | 24.10 | 8.88 | 374 | C++ | Microsecond |
| 8 | 42.70 | 24.90 | 9.24 | 322 | C++ | Kirk Mates |
| 9 | 40.91 | 28.36 | 8.31 | 320 | C++ | Lex |
| 10 | 39.54 | 28.12 | 8.55 | 398 | C++ | Cosmic |
| ... | | | | | | |
| **ours** | **29.76** | **35.73** | **13.78** | **629** | Rust | Bharat Labs (`2e4f48e` = source of `9fb5c93`) |

* **Language (measured):** Rust holds ranks 1, 3, 5 and 6; C++ holds 2, 4 and 7-12. Nothing in the data says C++ is faster.
* **K 30 to 40 (measured):** the K 40 group has T about 24-28 ms, M about 8.3-9.2 MB and S about 320-400. Against us, that is
  -8 to -12 ms, -4.5 to -5.5 MB and -230 to -300 lines.
* **K 40 to 45+ (measured):** time. T 17-22 ms at M 9-12.8 MB.
* The feed holds only best and latest per team, so submission histories (gradual steps or jumps) cannot be read from it.

## 2. Scoring (organisers' `harness/scoring.py`, verified; matches the feed's K to within 0.2%)

K = (14.44 s / T)^0.5 x min(367 MB / M, 50)^0.2 x min(108 / S, 1)^0.15 x (1 / D)^0.15. M is the run cgroup's `memory.peak`
(`harness/run_submission.py`); `/data` is pre-warmed so the input pages are not charged to the run.

T needed for a target K (calculated):

| M, S | K 35 | K 40 | K 45 | K 50 | K 60 | K 70 | K 80 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 13.8 MB, 629 (ours) | 25.8 | 19.8 | 15.6 | 12.6 | 8.8 | 6.5 | 4.9 |
| 10.0 MB, 629 | 29.4 | 22.5 | 17.8 | 14.4 | 10.0 | 7.3 | 5.6 |
| 10.0 MB, 450 | 32.5 | 24.9 | 19.6 | 15.9 | 11.0 | 8.1 | 6.2 |
| 9.3 MB, 451 (leader) | 33.4 | 25.6 | 20.2 | 16.4 | 11.4 | 8.3 | 6.4 |

Sensitivity at our point: -1 ms T = +1.42% K, -1 MB M = +1.52% K, -10 lines S = +0.24% K. **1 MB of memory is worth as much as
1 ms**, and we are 4-5 MB above the field.

## 3. Board evidence about our own time (measured)

| Run | Source | T | M | What it says |
|---|---|---:|---:|---|
| run-ec41ba8a226a and two others | control `9fb5c93` | 35.73-36.73 ms | 13.5-13.78 MB | |
| latest in the feed (`ddfe81a`, unscored by design) | control with `tc6_noresolve` (no name lookup, no region, no totals) | **21.62 ms** | **7.81 MB** | parse + read + fixed alone take 21.6 ms; the name/region/totals stage takes about 14-15 ms (40%) and about 6 MB of M |
| run-a3e9f05bd3dc | R2 (14% fewer instructions locally) | 36.09 ms | 13.3 MB | no gain |

**Our parse stage alone (21.6 ms) is slower than the leaders' whole runs (17-19 ms).** Even a free name table would leave us at
K about 35 (T 21.6, M 7.8 + 0.5, S 629). Both halves must change.

**Warning (measured, 16:00 UTC):** `speed-demon`'s `track-c/src/main.rs` currently *is* the `tc6_noresolve` diagnostic (commit
`9e7524d` from the other session's `tools/c_diag_run.sh`, which restores the source and pushes only after the board result
arrives). If that script does not finish, Track C on the submission branch prints wrong output. This session did not push to
`speed-demon`.

## 4. Gap model and what it implies (estimate)

| Part | Ours on the board | Leader-level target | Lever |
|---|---:|---:|---|
| read + fixed | about 11 ms (6.5 read + 4-5 fixed) | about 8 ms | fewer phases and spawns; compact region pass |
| parse | about 10-11 ms | about 5 ms | needs about half the instructions per record **and** no serial dependency chain (see A1 below) |
| name lookup + mask writes + totals | about 14-15 ms | about 5 ms | read-only identity, no locked writes (O2), smaller tables |
| M | 13.8 MB | 9-10 MB | the name state is about 6 MB of M; the field holds theirs in about 2-3 MB |

Memory, analysis (estimate): 100.5 k distinct names and 2.98 M distinct (name, country) pairs out of 24 M possible. Holding the
pair set needs at least about 1.7 MB in theory; a 32-byte mask per dense id is 3.2 MB. Our 2^17 x 64 B direct table touches all
8 MB. A design with dense ids (masks 3.2 MB, keys about 1.5-2 MB in a byte arena, a 512 KB tag/id index) would save about
2.5-3 MB, i.e. +4-5% K, at the cost of one more dependent load per record (index, then key and mask, whose addresses are both
known from the id, so they can be prefetched together).

## 5. Experiment registry (this session and the ones before; all correctness = identical to the independent reference on 9 fixtures)

| ID | Hypothesis | Commit / patch | Local | Board | S | Decision |
|---|---|---|---|---|---:|---|
| B (control) | direct entry table | `9fb5c93` | 69-72 ms | K 29.40 / 29.52 / 29.76 | 629 | **best verified** |
| R1 | one-vector key compare and hash | `7c64950` | -5% | not run | 636 | superseded |
| R2 | R1 + numbers in one AVX2 pass | `736e57a` | -8% | K 29.63 (no gain) | 656 | rejected by the board |
| P1, P2 | 7 separator writes; no region prefetch | `docs/...` | +2.7%, +6.9% | | | rejected |
| F1-F3 | smaller tables | `docs/track-c-session5` | 0 to -1.3% K | | | rejected |
| H1 | huge pages for the entry table | same | neutral | | | rejected |
| O2 | read-only keys, owner-written masks, rings (no locked op) | `docs/track-c-locality/o2_owner_masks.patch` | +11% (cannot show its coherence gain) | needs a run | 760 | candidate, conditional on board evidence |
| **A1** | **one-pass sequential parser: no structural stage, SWAR delimiters, 32-byte quote search** | `docs/track-c-candidates/a1c_sequential_parser.patch` | -14% instructions, **2.25x mispredicts, +23% time** | not run | **609** | rejected locally: each field waits on the previous scan (a serial chain) |
| A1d | A1 with 64-byte quote search | `a1d_sequential_parser.patch` | mispredicts 2.76 M (from 3.84 M), time unchanged | | 612 | rejected: the chain, not the mispredicts, is the cost |

A1 is still informative: it shows the structural two-stage parser exists for a reason, and that S can drop by about 20 lines
when the structural stage goes. A board run of A1 would test whether SMT hides its serial chain (hypothesis, low confidence).

## 6. Ordered plan against the measured targets (T <= 20 ms, M <= 10 MB, K >= 46)

1. **Memory first (deterministic, no board noise):** dense-id name state (about -2.5 to -3 MB) plus a compact region table
   (-0.55 MB). Estimate: M 13.8 to about 10.5 MB, +5-6% K, T to be measured. One board run.
2. **The name/region/totals stage (14-15 ms):** O2-style ownership on top of 1, so that masks have one writer and keys are never
   written after insertion. Board run of `tc6_nomask` first (or the other session's result for it) to size the locked-write
   share.
3. **Parse (about 10-11 ms):** a new hypothesis is needed. Instruction cuts did not transfer (R2), and removing the structural
   stage creates a serial chain (A1). Next: keep the branch-free structural stage, but give stage 2 independent work per record,
   decoding several records at once from the separator array (interleaved SWAR for 4 records, which breaks the per-record
   chain).
4. Fixed cost: one thread pool, fewer phases (about 1-2 ms).

No step above has a board result yet; K >= 46 is a target, not a forecast.

## 7. Datasets and check.py

`check.py` needs Docker, which this environment does not have. The full and sample seed-1 datasets were generated earlier in this
session with the organisers' generator; their sha256 values match the spec, and every candidate's output on them matches the
published output hashes (`d8088b1c...`, `9035fed3...`). The organisers' generator is blocked from further runs in this session,
so other seeds are covered by the spec-based stress, adversarial, hostile and twins fixtures. Nothing is tuned to seed 1:
the only data-dependent constants are the spec's own bounds (at most 256 countries, 2^17 names, LF-in-quote rule), each with an
exact fallback.
