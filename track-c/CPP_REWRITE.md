# Track C in C++ (branch `cloud/track-c-cpp`): a from-scratch implementation

Status: local only, **no board run**. The control is `9fb5c93` (Rust, board K 29.40-29.76); production `speed-demon` is
unchanged.

## Design

Constraints taken from the board's history: at most 2 random lines per record (every board win removed one), no long serial
dependency chains (Candidate A lost on the board), no dense-id indirection (M1-M3 lost), and as few phases and thread spawns as
possible.

| Part | Choice | Why |
|---|---|---|
| threads | one pool; three phases (regions.csv, records.csv, mask count) separated by `std::barrier` | spawn once, not three times (`pool_b`'s idea, here for free) |
| input | `pread` of 32 KiB blocks into a per-worker buffer | the board measured `pread` reading at about 6 ms for 202 MB; mmap was rejected on this board |
| stage 1 | AVX2 compares for `"`, `,`, LF over 64-byte words, carry-less multiply for the quote state, separator positions into an uninitialised array | branch-free, independent per word |
| stage 2 | five known separators per record, SWAR numbers, name keyed by its raw bytes (escaping is one to one), quote counts by popcount | no serial chain between records |
| names | 2^17 hash-indexed 64-byte entries (32-byte key block + 256-bit country mask), 1-byte tags, 16 compared at once, three batches of 16 in flight | one random entry line per record, as in the best board result (B) |
| regions | byte table region_id -> country index | one random line, as in board run 21 |
| fallback | any record that does not fit: the whole file again as one chunk with a 2^22 table | exact for any input valid per spec 2.1 |

Alternatives rejected on the evidence: hash-partitioned routing of records to owner threads (exchange memory and
synchronisation; the board showed sharing is not the main cost) and zero-copy mmap input (rejected on this board for Track A).

## Results (local: KVM, 4 vCPUs of a Xeon, no SMT; the board is an i5-13500 with 6 cores and 12 threads)

| | Control (Rust, `9fb5c93`) | C++ (`cx2`, board flags `x86-64-v3 -mpclmul`) |
|---|---:|---:|
| full seed 1, 4 threads (30-50 interleaved runs, three sets) | 67.77 / 71.81 / 69.48 ms | 67.78 / 72.47 / 72.47 ms (first cx1 build) |
| full, 2 threads (20 runs) | 115.74 ms | 116.89 ms |
| full, 1 thread (20 runs) | 215.18 ms | 218.86 ms |
| sample seed 1, 4 threads | 22.00 ms | 20.67 ms (-6%) |
| tiny seed 1, 4 threads (setup-dominated) | 6.05 ms | 5.18 ms (-14%) |
| instructions, full (valgrind, haswell build) | 1,785 M | 1,729 M (-3%) |
| mispredicts | 2.13 M | 1.89 M (-11%) |
| max RSS, 4 threads | 11.6 MB | 11.4 MB |
| **S** (clang-format LLVM non-blank non-comment + Dockerfile) | 629 | **494** |

In this VM, time on the full file is at parity. The S cut alone is worth (629/494)^0.15 = +3.7% K at equal T and M. Setup-heavy
inputs are 6-14% faster.

## Correctness

* Byte-identical to the independent reference on tiny, sample and full seed 1, adversarial, hostile-LF (single-chunk
  fallback), stress1-3 and twins (9/9), also with 200-byte blocks and 3,000-byte chunks (9/9), and with the board's exact flags
  (9/9).
* AddressSanitizer and UndefinedBehaviorSanitizer: no report on tiny, adversarial, hostile, twins, sample and stress1.
* An out-of-spec file with 2,757 countries (over the 256 limit) is rejected, by the control too.
* Concurrency: the region table is written at distinct ids before a barrier, and the name entries are published with
  release/acquire on key word 0. A reader whose acquire sees 0 compares nothing, so no entry is read while it is written.

## Expected board effect (estimate)

T about equal, possibly lower by the setup gain; M about equal; S 494. K ≈ 29.6 x 1.037 ≈ 30.7. This is not the substantial gain
the mission asks for; it is a smaller, equally fast base with the same architecture's limits.

## Reproduce

```bash
g++ -std=c++23 -O3 -march=x86-64-v3 -mpclmul -flto=auto -fno-exceptions -fno-rtti -fno-pie -no-pie -pthread -static-libgcc -static-libstdc++ track-c/src/main.cpp -o solution
```

```bash
clang-format --style=LLVM track-c/src/main.cpp | grep -vcE '^\s*(//|$)'
```
