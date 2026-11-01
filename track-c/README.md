# Track C: Join

The task, the input files and the exact output format are in the
[spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md). Language: **C++** (C++23, standard
library and Linux system calls only; D = 1). Built by `Dockerfile` with gcc 15: `-O3 -march=x86-64-v3 -mpclmul -flto`, no
exceptions or RTTI, static libstdc++. Measurements and every board run are in [PERF.md](PERF.md); the earlier Rust version is
commit `9fb5c93`.

## Approach

One pool of threads (as many as the CPUs the process may use) runs three phases separated by `std::barrier`.

1. **Region table.** `regions.csv` is read in 48 pieces. Each line's comma is found with one 8-byte SWAR test, the id is converted
   with SWAR arithmetic, and the country code gets a dense index (in the order codes are first met; the output is sorted by code
   at the end, `_UNMATCHED` last). The table is one byte per region id: id -> country index, 0 for an id that is not a region.
   The same phase finds where each chunk of `records.csv` starts (below).
2. **Records.** `records.csv` is cut into 1 MiB chunks, 128 KiB over the last 15% so that the workers finish together. A worker
   reads a chunk in 32 KiB blocks with `pread`.
   * Stage 1 finds, with AVX2, every `"`, `,` and LF in 64-byte words. A carry-less multiplication of the quote bits gives which
     bytes are inside quotes (a quote toggles, and so do both halves of a `""`). The commas and LFs outside quotes are exactly
     each record's four commas and terminator, and their positions go to an array.
   * Stage 2 takes a record as five known boundaries. Numbers come from SWAR arithmetic on the digit counts the boundaries give.
     The unescaped length of a name or a note is its length minus its `""` pairs, counted with a popcount of the quote bits.
     **A name is keyed by its bytes as written, `""` included**: writing a value with every `"` doubled is one to one, so two
     names are equal exactly when those bytes are, and no name is ever unescaped.
3. **Distinct names.** One shared table of 2^17 entries of 64 bytes, placed by a hash of the name. An entry is a 32-byte key
   (byte 0 is the length + 1, then up to 31 name bytes, zero padded) and a 256-bit mask of the countries the name occurred in.
   A name longer than 31 bytes stores 255 in byte 0 and keeps its bytes in a spill area, compared byte by byte with `memcmp`. A
   one-byte tag array (8 bits of the hash per entry) is read 16 tags at a time with SSE, so a lookup reads one entry line in all
   but rare cases. Every hit is compared exactly, as one 32-byte AVX2 compare. A new entry's key word 0 is written last with
   release, and readers acquire it. Records are looked up in batches of 16 with three in flight: tags are prefetched while the
   next batch is parsed, then the entry is prefetched for writing, then resolved. The country bit is set with `lock bts`; the
   worker that finds it unset counts the name for that country, so there is no counting pass at the end.
4. **Output.** The per-worker totals are merged, formatted without floating point, and written with one `write`.

## What the fast path relies on (spec 2.2) and what happens if it is wrong

* **An LF inside a quoted field is never followed by a digit 1-9 or a BOM**, so a chunk starts at the first LF followed by one of
  those. Check: every record's five separators must be `,,,,` and LF, and a chunk's last record must end exactly where the next
  chunk starts. If any chunk fails, the whole file is parsed again as one chunk (no cut, so no assumption) with a table of 2^22
  entries. Tested with `tools/c_hostile_lf.py` inputs, where the fallback fires and the output is still exact.
* **At most 255 countries** (the spec has 240, plus `_UNMATCHED`), **ids 1 and above, region ids and fields as the grammar
  says** (spec 2.1 allows assuming well-formed input). An invalid `regions.csv` line or more countries makes the run give up
  (exit 1); empty number fields fail the record checks, and so trigger the fallback.
* **At most 7/8 of 2^17 distinct names** (the spec's pool is 100,000); more makes the fast path give up, and the fallback's
  table holds 2^22.
* A record, a field or a name of any length is handled: a record is carried to the next block until it is complete.

## Tests

Every source is checked against an independent Python reference (`tools/c_ref.py`; it reproduces the published sha256 of full and
sample seed 1) on tiny, sample and full seed 1, `tools/c_adversarial.py`, `tools/c_hostile_lf.py`, three
`tools/c_stress_gen.py` files and `tools/c_twins_gen.py` (names that differ in exactly one byte at every position 0-40). It is
also checked with 200-byte blocks and 3,000-byte chunks (every carry and chunk-boundary path), and with AddressSanitizer,
UndefinedBehaviorSanitizer and ThreadSanitizer (no reports).

## With another 24 hours

The board puts the time in three parts of similar size: read plus setup about 8.5 ms, parse about 13 ms, name table about
11 ms. Instruction cuts did not move it (R2) and a one-pass parser was slower (row 25); what moved it was removing random lines per
record and whole phases (rows 21, 23, 28). Next would be the same kind of removal in the parse stage, for which no measured
candidate exists yet.
