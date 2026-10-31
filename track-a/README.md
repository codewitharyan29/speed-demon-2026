# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is mapped with `mmap` and cut into 48 newline-aligned chunks that 12 threads parse by hand (no UTF-8
validation, no allocation per line). The hour is the first 13 bytes of the timestamp, so no date arithmetic is needed,
and it sorts chronologically as bytes. Each chunk groups its latencies and 5xx counts by (hour, endpoint) in a hash
table, split into 12 shards by hour of day. A second parallel pass takes one shard at a time, merges the chunks'
groups, sorts each group's latencies and reads the nearest-rank percentiles. The formatted lines are put in global
(hour, endpoint) byte order and written with a single `write`.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

A radix pass instead of comparison sorts, and a measurement of the 12-CPU case on the board.
