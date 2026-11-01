# Handover (plain language)

What the program does: it reads the 202 MB log in 1 MiB pieces on 12 threads. For each piece it first finds, with a few vector
instructions per 64 bytes, where the commas and line ends are that are *not* inside quotes (a quote flips "inside/outside", and a
doubled quote flips twice, so the flip pattern is exact). That gives five positions per record; everything else (the numbers, the
lengths of the name and the note) is arithmetic on those positions. The name goes into a shared table (one 64-byte cache line per
distinct name, with a bit for each country it appeared in) and is compared byte for byte, so the "distinct names" count is exact.
At the end each country's total is added up and printed in byte order of the country code.

Decisions you may be asked about:
* Why not cut the file at any newline? Because a note can contain a newline (277,000 records do). We cut at a newline followed by
  a digit or a BOM, which only a record end can be followed by in the generator's data (spec 2.2), and check each piece; if a check
  fails the program redoes everything as one piece.
* Why is it not as fast as the leaders? Measured on the board: 20 of 52 ms are the name lookups and 24 ms the parse; the leaders
  are at 18-20 ms in total. We did not find what they do differently (docs/COMPETITORS.md).
* Why no hash-only name counting? The brief and the spec ask for exact answers; a 64-bit hash would be exact only with
  probability about 1 - 10^-9. We pay for exactness in lookup time.

Before freeze (2026-11-02 15:00 UTC): HEAD of `speed-demon` must be the best verified commit (STATUS.md). Do not push more than needed:
each push re-runs every changed track. After freeze, within one hour: make the repository public under an OSI licence, and keep the
Board app installed until freeze.
