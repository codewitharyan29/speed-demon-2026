# Track C status

Updated: 2026-11-01 (code freeze: 2026-11-02 15:00 UTC).

* Best verified commit: **66693b1** (tag `tc-good-8`; C++ cx5 by the cloud session), board run **run-ce3a1da5b3de**: T 34.51 ms, M 14.14 MB, S 432, D 1, **K 31.88**. Earlier: cx3 4452f30 (run-c6a876ca3df0, K 31.51), first C++ 6ec50ad (K 30.54), Rust candidate B 9fb5c93 (K 29.29-29.76). Recovery points: 4452f30, 4e11c91 (Rust).
  Baseline K 9.07 (`tc-baseline`). Leaders K 41-47 (docs/COMPETITORS.md). Board noise for identical code: T 52.6-55.8 ms.
* Correctness (local, Docker, reference = official `reference/c_join.py`): full seeds 1-5, sample 2-10, tiny 1-30, adversarial file,
  hostile-LF files (fallback fires) at five chunk sizes; thread count forced to 3, 5, 12, 24 and by cpuset 1/2/4/8: same hash.
  Debug-assertions + overflow-checks build under valgrind memcheck on tiny 1-30, adversarial, hostile 3-5 (chunks 2^14-2^18) and sample 2: no errors, output equal.
  Clean-clone `docker build --network=none --no-cache` OK; 50 runs with the harness flags: 50 identical sha256 (d8088b1c...f536).
* Not done: real sanitizers (nightly compiler not available offline), 12-CPU cpuset run (VM has 8; thread count 12/24 forced instead).
* Open risks: the spec-2.2 reliance (README); fast path gives up beyond 256 countries / 2^17 names (fallback 2^22).
* Remaining gap: T is 2.5x the leaders'; ideas left in PERF.md.
