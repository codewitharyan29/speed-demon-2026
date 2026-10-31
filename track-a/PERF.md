# Track A: performance log

Board runs are identified by the commit the board measured (the board lists runs by commit). Local numbers are the
in-process time of the full seed-1 dataset in a Docker container pinned to 8 CPUs (the Docker VM on this laptop has
8, not 12), reading from a Docker volume. They are noisy in absolute terms and are only used to compare variants.

## Board runs

| Step | Change | Run (commit) | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Rust: mmap, 48 chunks on 12 threads, per-chunk hash of (hour, endpoint), merge and sort per hour-of-day shard | | | | |

## Local measurements

| Variant | Result (ms, 8 CPUs) | Verdict |
|---|---|---|
| Per-chunk groups merged on one thread, then sorted in parallel | scan 195, merge +310, sort +170: 1000-1300 total | replaced |
| Groups sharded by hour of day (12 shards) in the scan; each shard merged and summarised on its own thread | 650-800 total, output identical | kept |

All three data sizes (tiny, sample, full) match the reference output byte for byte (full sha256 495ca096...).
`memory.peak` locally 92 MB.

## Ideas not done yet

- Cache the previous line's (hour, endpoint) group: consecutive lines rarely repeat, so probably no gain.
- Avoid the three-way copy (per-chunk vectors, concat into one, sort) with a radix pass on latencies.
