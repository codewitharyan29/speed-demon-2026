# Track C: performance log

Board runs are identified by the commit the board measured (the board lists runs by commit). Local numbers are the
in-process time of the full seed-1 dataset in a Docker container pinned to 8 CPUs (the Docker VM on this laptop has
8, not 12), reading from a Docker volume. They are noisy in absolute terms and are only used to compare variants.

## Board runs

| Step | Change | Run (commit) | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Rust: mmap, quote-parity chunking on 12 threads, direct-indexed region table, per-name country bitmasks | | | | |

## Local measurements

| Variant | Result | Verdict |
|---|---|---|
| Rust version, 96 chunks | 670-990 ms (median about 770), `memory.peak` 135 MB | kept as step 1 |

Tiny, sample and full outputs match the reference byte for byte (full sha256 d8088b1c...).

A bug found on the way: scanning for the first record start of a chunk from the byte before the cut counted that
byte's quote twice, so chunks could start inside a quoted field with a newline (277 k of the 3.3 M records have one).
Tiny and sample outputs caught it as wrong id sums; counts and amounts stayed right.

## Ideas not done yet

- 16 % of records have an unquoted name or note; a SWAR search for the closing quote instead of a byte loop.
- Smaller `Names` tables per thread (they start at 2^18 slots each).
