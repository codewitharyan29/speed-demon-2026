# Track C status

Updated: 2026-11-01 ~02:20 UTC (code freeze: 2026-11-02 15:00 UTC).

* Best verified commit: **5806282** (HEAD of `speed-demon`), board run **run-88a3e5416bb7**: T 52.6 ms, M 16.2 MB, S 604, D 1, **K 23.90**.
  The tracked baseline was K 9.07 (`tc-baseline`, run-c3e9263072bb). Leaders: K 41-47 (docs/COMPETITORS.md).
* Correctness evidence for this commit (local, Docker; reference = the official `reference/c_join.py`): full seeds 1, 2, 3 (seed 1 also
  through `check.py c`, sha256 d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536), sample seeds 2-10, tiny seeds 1-30,
  the adversarial file (tools/c_adversarial.py), three hostile-LF files at five chunk sizes (tools/c_hostile_lf.py), the single-chunk
  fallback forced on tiny/sample/full/adversarial. Clean-clone `docker build --network=none --no-cache` of track-c: 14 s, 118 MB image.
  50 consecutive runs of the container with the harness's flags (read-only root, 1 GB /tmp, 8 GB, pids 512, cpuset 0-7): 50 identical
  hashes, exit 0. Output identical with 1, 2, 4 and 8 CPUs (the VM has 8; 6 and 12 not testable here).
* Not done: sanitiser build (the unsafe code is in `structure` and `record`, justified in comments), 12-CPU determinism run (the
  VM has 8 CPUs).
* Open risks: the single reliance on spec 2.2 (README); the fast path gives up beyond 256 countries or 2^17 names (the fallback
  handles 2^22); measured T is 2.5x the leaders'.
* Next experiments by expected value: partition the name space across workers (-10 ms est.); persistent thread pool (-1 ms);
  byte region table (-1 MB).
