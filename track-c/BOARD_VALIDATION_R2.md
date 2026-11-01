# Track C: R2 board validation and next experiments

## Status: no board run yet

The board builds only what is pushed to `speed-demon`. I was asked not to push R2 there, and speeddemon.dev is not reachable from
this environment's proxy, so no official run was possible from here. **Every K for R1 and R2 below is a projection from local
timings, not an official result.**

## Exact candidates (all on `origin/cloud/track-c-extreme-rewrite`, all recoverable)

| Name | Commit | `track-c/src/main.rs` sha256 | S (rustfmt lines + 6) |
|---|---|---|---:|
| control | `9fb5c93` (tag `tc-good-6`, also on `speed-demon`) | identical to `speed-demon` today | 629 |
| R1 | `7c64950` | | 636 |
| **R2** | **`736e57aefdbf29163b2af1c395fdf910e1a75472`** | `2a6a14763630cd85fed615b11759e8b329f87d99e3a9f713dcc9cf16f37f8cc0` | 656 |

Only `track-c/src/main.rs` differs from the control, plus documentation. `Cargo.toml`, `Dockerfile`, `.cargo/config.toml`
(`target-cpu=native`) and `.speed-demon.toml` are byte-identical to `9fb5c93`; D = 1, no dependencies.

## Verification done for R2

* Clean build from `git archive 736e57a track-c`, exactly as the Dockerfile runs it (`cargo build --release --offline`, the
  folder's own `.cargo` config, rustc 1.99.1 = the pinned image's compiler): builds without warnings. A second build with
  `target-cpu=raptorlake` (the board CPU's target) was also run.
* Both binaries are byte-identical in output to the independent reference on tiny, sample and full seed 1, adversarial,
  hostile-LF, stress1-3 and twins (18 of 18). Full seed 1 output sha256 `d8088b1c...` and sample `9035fed3...`: the published
  hashes.
* Earlier, on the same source: 200-byte blocks with 3,000-byte chunks, overflow checks with debug assertions, and valgrind
  memcheck (0 errors on tiny, adversarial, hostile and stress): all pass.

## Comparison (local = KVM, 4 vCPUs of a Xeon, 80 interleaved runs, full seed 1; board = official)

| | Board runs | Board T | Board M | Board K | Local median (4 threads) | Local instructions | Projected K |
|---|---|---:|---:|---:|---:|---:|---:|
| control `9fb5c93` | run-99e4e3237a66, run-ec41ba8a226a, run-a829e7350f95 | 36.59 / 35.73 / 36.73 ms | 13.5-13.78 MB | 29.52 / 29.76 / 29.40 | 72.00 ms | 1,785 M | (official) |
| R1 `7c64950` | none | N/A | N/A | N/A | 68.29 ms (-5.1%) | 1,616 M | 30.4 |
| R2 `736e57a` | none | N/A | N/A | N/A | 66.22 ms (-8.0%) | 1,530 M | 30.6 |

The control's three board runs span 35.73-36.73 ms (2.8%). R2's projected gain (about 8% of T) is above that spread; R2 against
R1 (about 3%) is at its edge, so separating those two needs two runs each.

## How to run it (your decision; nothing has been pushed to `speed-demon`)

On a clean checkout of `speed-demon`:

```bash
git checkout 736e57a -- track-c/src/main.rs
```

```bash
git commit -m "Track C: R2 for board validation (736e57a)" -- track-c/src/main.rs
```

```bash
git push origin speed-demon
```

This changes only `track-c/src/main.rs`; the other tracks are untouched. Run it twice (the same source pushed again, as was done
for the control). For R1, use `7c64950` instead. To restore the control afterwards:

```bash
git checkout 9fb5c93 -- track-c/src/main.rs
```

## Next experiments on the remaining stages (after validation)

Profile of R2's predecessor R1 on the full file (cachegrind; `#[inline(never)]` build): stage 2 `record` about 256 instructions
per record (R2 brings it to about 230), stage 1 `structure` about 90 per 64-byte word (about one word per record), `resolve` 83,
`warm` 31, the chunk and batch loop 30.

Done this session, both rejected:

| # | Hypothesis | Result (60 interleaved runs vs R2) | Decision |
|---|---|---:|---|
| P1 | stage 1: 7 unconditional separator writes instead of 8 (words have more than 7 separators 7.5% of the time) | +2.7% | rejected |
| P2 | stage 2: drop the region-byte prefetch and its bounds check (4 instructions) | +6.9% | rejected: memory latency still matters for the 1 MB region table |

Separator counts per 64-byte word on full seed 1: 4: 16%, 5: 38%, 6: 20%, 7: 11.5%, 8: 5.8%, more: 1.7%.

Next, in order of expected value (none built yet):

1. **The quote counts in stage 2** (3 `before()` lookups of 2 loads + `bzhi` + `popcnt` each, about 24 instructions). Hypothesis:
   most records have exactly 2 quotes in the name and 2 in the note. A single 64-bit window test over the record's bytes could
   give both counts with one `popcnt` when the record fits inside one or two quote words. Risk: branch on the record's alignment.
2. **Fewer stores per record.** `Rec` is 52 bytes written in stage 2 and read in `resolve`. `id` and `note_bytes` only feed the
   totals of the record's country, and the country is known (prefetched) one batch later. Packing id, note and name lengths into
   one word saves about 6 instructions per record; adding them in stage 2 to a per-block "pending" slot instead would need the
   country early, which P2 says costs latency.
3. **Stage 1 classification with one `vpshufb` lookup** instead of three compare pairs: about 4 instructions per word. Small but
   free of branches.

Expected values are small (each 1-3% of T). P1 and P2 both show that the remaining time is no longer pure instruction count,
so each needs an interleaved measurement before a board run.
