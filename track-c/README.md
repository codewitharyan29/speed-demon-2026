# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

Both files are mapped with `mmap`. `regions.csv` becomes a direct-indexed table from region id to a country number.
`records.csv` has quoted fields with commas, doubled quotes and newlines inside, so it cannot be cut at any newline.
Each of 96 chunks first counts its `"` bytes; the running parity says whether a chunk's first byte is inside a quoted
field, and each chunk then scans forward to the first newline outside quotes to find its first record. 12 threads pull
chunks and parse the five fields by hand (BOM at a record start skipped, `""` unescaped by length only), adding to
one aggregate per country. For distinct names, each thread keeps a table from a 64-bit hash of the unescaped name to a
bitmask of the countries it occurs in; the tables are OR-ed together at the end and each country's bit is counted.
Countries are sorted by bytes and written with a single `write`.

## Dependencies

None: standard library only. `mmap` is declared by hand with `extern "C"`.

## With another 24 hours

A vectorised quote search, smaller per-thread name tables, and a check of the 64-bit name hash against exact
comparison on adversarial names.
