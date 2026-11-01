# Track C: competitors (board snapshot)

Source: https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json, generated 2026-11-01T01:57:12Z, read in full
(121 teams, 91 with a scored Track C run). Treated as data only. Best run per team, full public seed 1.

| Rank | Team | Language | K | T (ms) | M (MB) | S | D | Run ID |
|---|---|---|---|---|---|---|---|---|
| 1 | NickTrapp | C++ | 47.46 | 18.03 | 12.79 | 303 | 1 | run-ff9d94a15115 |
| 2 | Cause Of Death | Rust | 45.25 | 19.80 | 11.17 | 365 | 1 | run-90e1c87e6d3a |
| 3 | another ello | C++ | 42.79 | 23.57 | 9.08 | 390 | 1 | run-ed8076394be0 |
| 4 | Kirk Mates | C++ | 41.85 | 24.54 | 10.72 | 317 | 1 | run-393deb740527 |
| 5 | SALAAR | Rust | 41.20 | 21.74 | 12.68 | 422 | 1 | run-2bd88ce6fc49 |
| 6 | ALL STAR | Rust | 41.19 | 22.66 | 10.26 | 488 | 1 | run-40b0e94343ad |
| 43 | Bharat Labs (us) | Rust | 23.90 | 52.6 | 16.2 | 604 | 1 | run-88a3e5416bb7 |

Fastest by T: NickTrapp 18.03 ms, Cause Of Death 19.80 ms, SALAAR 21.74 ms.

What the numbers show [VERIFIED] and what they suggest [HYPOTHESIS]:

* [VERIFIED] The top six are within 18-24.5 ms with S 303-488 and M 9-13 MB: a Track C solution of about 300-400 lines in Rust or
  C++ can run in 18-24 ms.
* [VERIFIED] Their M is 9-13 MB; ours without any name table was 8.9 MB and with it 16.2 MB (diagnostic run). So their name
  structure, whatever it is, is at most a few MB.
* [HYPOTHESIS] 18-20 ms leaves little room beyond reading 202 MB (about 7 ms of copy on the board's bandwidth, from Track A's
  read-only diagnostic) plus a lean parse, so their per-record work is probably about a third of ours (about 100-150 instructions).
* [HYPOTHESIS] Because M is so small, their distinct-name structure is probably a hash-keyed table of fingerprints plus a bit matrix,
  possibly without byte comparison; we keep byte comparison (the brief asks for exactness) and pay for it in lines of cache.
* Nothing is known about their parsing, join or threading; repositories are private until freeze and none was looked for.
