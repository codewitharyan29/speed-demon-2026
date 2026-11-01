# Track C: Join

The task, the input files and the exact output format are in the
[spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md). Language: Rust, standard library only
(D = 1). Measurements and every board run are in [PERF.md](PERF.md).

## Approach

1. **Region table.** `regions.csv` (200,000 lines) is read in 16 pieces by all threads and written into a table indexed by
   `region_id` whose entries are a packed three-letter country code. The codes that occur get dense indices in the byte order of
   the codes, so the output order needs no sort; `_UNMATCHED` is last.
2. **Chunks.** `records.csv` is cut every 1 MiB. A chunk starts at the first record found from the bytes after the cut alone: an
   LF followed by a digit 1-9 or a BOM. No pass over the data is needed to learn which quotes are open.
3. **Two stages per block.** A worker reads 64 KiB with `pread`. Stage 1 finds, with AVX2, every `"`, `,` and LF in 64-byte
   words and computes which bytes are inside quotes with a carry-less multiplication (the prefix XOR of the quote bits: a quote
   toggles, and so do both halves of a `""`). The commas and LFs outside quotes are exactly the four commas and the terminator
   of each record. Stage 2 takes a record as five known boundaries: numbers are converted with SWAR arithmetic from the digit
   counts the boundaries give, and the unescaped length of a name or a note is its length minus the number of `""` pairs, got
   from a popcount of the quote bits. No byte of a text field is looked at, except for hashing a name.
4. **Distinct names.** Every distinct name has one 64-byte entry (its length, its first 24 bytes, and a 256-bit mask of the
   countries it occurred in), found through a table of compact slots. Names are compared byte for byte; a name over 24 bytes is
   compared against a spill area; a name with `""` in it is unescaped first. Records are looked up in batches, with three batches
   in flight, so the cache misses of one batch are served while the next is parsed. At the end the set bits of the masks are
   counted per country.
5. **Output.** Totals per country are merged, formatted without floating point, and written with one `write`.

## What the fast path relies on (spec 2.2) and what happens if it is wrong

* **An LF inside a quoted field is followed by a letter or a quote, never by a digit 1-9 or a BOM** (notes are made of lowercase
  words). This is how a chunk finds its first record. Check: every record must have its four commas and LF at the positions stage 1
  found, and the last record of a chunk must end exactly at the next chunk's start. If any chunk fails, the whole input is parsed
  again as one chunk by one thread (no cut, so no assumption), with a name table that holds a name per record. Tested with
  `tools/c_hostile_lf.py` (valid per spec 2.1 but with LF + digit and LF + BOM inside notes), at chunk sizes from 16 KB to
  1 MiB: the fallback fires and the output equals the reference.
* **At most 256 countries** (the spec has 240 plus `_UNMATCHED`): the masks have 256 bits. More makes the run give up.
* **Fields are what the grammar says** (spec 2.1: "a solution may assume well-formed input"): digits are not validated. The
  separators are.
* **A name table of 2^17 names** (the spec's pool is 100,000); more makes the run give up and the fallback uses 2^22.
* A record or a field of any length is handled (a record is carried over to the next block until it is complete).

## Tests

`tools/` (outside this folder) has the scripts: `c_check.sh` (a binary against a reference output), `c_multiseed.sh` (official
generator and reference, any seed and scale), `c_adversarial.py` and `c_hostile_lf.py` (valid-by-grammar inputs outside the
generator's distribution), `cg_c.sh` (instruction and miss counts per source line with valgrind).

## With another 24 hours

Partition the name space by hash across the workers so each owns a table small enough for its L2 and nothing is shared;
one persistent thread pool; and a region table of bytes. Section "Not done" of PERF.md has the expected gains.
