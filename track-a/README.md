# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The input is cut into 4 MiB newline-aligned chunks that 12 threads take from one counter. A worker reads its chunk
with `pread` into a 256 KiB buffer (no mapping of the 239 MB file, so no page faults on it and no teardown of it) and
parses each line without validation: AVX2 compares find the spaces after the timestamp, the status class is one
byte, and the latency is turned into a number with an 8-digit SWAR multiply chain. The endpoint is looked up in one
lock-free dictionary shared by all workers (open addressing, keyed by a 64-bit AES-round hash of the whole endpoint,
computed without branches over its first 64 bytes; key bytes are not compared on a hit, and a collision between two of
the at most 1,842 endpoints has odds of about 1e-13), which gives dense ids. Each line becomes a 6-byte record (latency, endpoint id, 5xx flag)
appended to a region private to its chunk and hour; the regions live in one sparse anonymous mapping, so only the
touched pages cost memory.

When all chunks are parsed the endpoint ids are sorted by their bytes once. Then workers take whole hours: they
counting-sort the hour's records by endpoint, read the nearest-rank percentiles (insertion sort for small groups,
`select_nth_unstable` for large ones), and format the lines. A finished hour is written to the output file as soon as
every earlier hour is, so the write overlaps the remaining work.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

Format the output directly into a shared mapping of the file, interleave two lines per loop to hide the dictionary
loads, and release each hour's records as soon as it is summarised to lower the memory peak.
