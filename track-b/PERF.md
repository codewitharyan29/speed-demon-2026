# Track B: performance log

Board runs are identified by the commit the board measured. Local numbers come from a 12-CPU Docker container on a
Windows laptop (input on a docker volume), so they are only used to compare variants, never as results.

| Step | Change | Run (commit) | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2200 | 0 |
| 1 | Baseline: query terms interned in a hash table, parallel corpus scan keeping postings only for query terms, sorted-list merges for BOOL, candidate intersection plus re-tokenising for PHRASE | pending | | | |

## Local checks for step 1

- Output hash on full seed-1 data matches the published `f86973c2…`; sample and tiny match the Python reference
  byte for byte; the spec's 18-query worked example passes.
- About 1.3-1.7 s wall including container start, `memory.peak` 311 MB (includes the mapped corpus pages locally).

## Next

Profile the phases (scan, posting build, BOOL, PHRASE), then galloping intersections and bitsets for dense terms,
and cut memory (the 50x cap is about 44 MB).
