# Track C status

Updated: 2026-11-01 (code freeze: 2026-11-02 15:00 UTC).

* Best verified commit: **95582d0** (tag `tc-good-3`), board run **run-c30f1d80a49f**: T 52.85 ms, M 15.5 MB, S 605, D 1, **K 24.05**.
  Baseline K 9.07 (`tc-baseline`). Leaders K 41-47 (docs/COMPETITORS.md). Board noise for identical code: T 52.6-55.8 ms.
* Correctness (local, Docker, reference = official `reference/c_join.py`): full seeds 1-5, sample 2-10, tiny 1-30, adversarial file,
  hostile-LF files (fallback fires) at five chunk sizes; thread count forced to 3, 5, 12, 24 and by cpuset 1/2/4/8: same hash.
  Debug-assertions + overflow-checks build under valgrind memcheck on tiny 1-30, adversarial, hostile 3-5 (chunks 2^14-2^18) and sample 2: no errors, output equal.
  Clean-clone `docker build --network=none --no-cache` OK; 50 runs with the harness flags: 50 identical sha256 (d8088b1c...f536).
* Not done: real sanitizers (nightly compiler not available offline), 12-CPU cpuset run (VM has 8; thread count 12/24 forced instead).
* Open risks: the spec-2.2 reliance (README); fast path gives up beyond 256 countries / 2^17 names (fallback 2^22).
* Remaining gap: T is 2.5x the leaders'; ideas left in PERF.md.
