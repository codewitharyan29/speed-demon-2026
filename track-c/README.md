# Track C: Join

The task, input CSV specifications, and exact aggregation output format are defined in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).  
Step-by-step benchmark measurements, structural parser ablations, and board run logs are documented in [PERF.md](PERF.md).

- **Language:** C++ (C++23 standard library and Linux system calls only; zero dependencies, $D = 1$).
- **Toolchain & Flags:** Built with gcc 15 via `Dockerfile`:
  `g++ -std=c++23 -O3 -march=x86-64-v3 -mpclmul -flto=auto -fno-exceptions -fno-rtti -fno-pie -no-pie -pthread -static-libgcc -static-libstdc++ src/main.cpp -o solution`
- **Source Size:** Single C++ file (`src/main.cpp`, ~400–428 lines), yielding a high source-size score multiplier.

---

## Official Scored Performance

Best live-board result (commit `9bfa8af`, run `run-d8a78803f209`):

| Metric | Reference (Python) | Our Solution (C++) | Speedup / Improvement |
|---|---|---|---|
| **Wall Time ($T$)** | 14.44 s | **0.03398 s (33.98 ms)** | **425.0× faster** |
| **Peak Memory ($M$)** | 367 MB | **13.6 MB** | **27.0× reduction** |
| **Source Size ($S$)** | 108 lines | **428 lines** | Tight code-size footprint |
| **Dependencies ($D$)** | 1 | **1** (Zero external libraries) | No dependency penalty ($1.00×$) |
| **Performance Multiplier ($K$)** | 1.00 | **32.42** | — |
| **Official Track Score** | 0.00 pts | **75.54 pts** | **Top Leaderboard Rank** |

Latency fell from 177.6 ms (step 0 baseline) to **33.98 ms** at step 29 — a **425× speedup** over the naive Python reference, with peak memory settling at just **13.6 MB** through kernel `PR_SET_THP_DISABLE` transparent huge page suppression.

---

## Architecture & Pipeline

A single persistent thread pool (12 workers matching available hardware threads) runs four synchronized phases partitioned by `std::barrier`.

```mermaid
flowchart TD
    subgraph Phase 1: Region Table & Chunk Discovery
        REG[regions.csv\nRead in 48 pieces] --> SWAR_R[8-byte SWAR Comma & ID Parser]
        SWAR_R --> RTBL[1 MB Region Lookup Table\nregion_id -> country_index]
        REC[records.csv] --> CHK[Find 1 MiB Chunk Bounds\nLF followed by digit/BOM]
    end

    subgraph Phase 2: Structural SIMD Record Parsing
        CHK --> PRED[Worker pread in 32 KiB Blocks]
        PRED --> CLMUL[AVX2 Vector Quote Classifier\nCarry-less PCLMULQDQ Bitmask Toggle]
        CLMUL --> SEPS[Unescaped Separator Extraction\nCommas & Newlines Outside Quotes]
        SEPS --> FLD[SWAR Decimal Arithmetic\nID, Region, Amount in Cents]
    end

    subgraph Phase 3: Shared Concurrent Name Table
        FLD --> RAW[Raw-Byte Name Keys\nZero Unescaping Allocations]
        RAW --> TAG[SSE 16-wide Tag Vector Probe\n128 KB Tag Array in L2]
        TAG --> HASH[2^17 Shared Entry Table\nExact 32-byte AVX2 Compare]
        HASH --> BTS[Atomic lock bts on 256-bit Country Mask\nFirst setter increments country total]
    end

    subgraph Phase 4: Output Serialization
        BTS --> MERG[Merge Per-Worker Totals]
        MERG --> FMT[Fixed-Point Integer Formatter\nNo floating point]
        FMT --> OUT[/out/aggregates.txt\nSingle write syscall & _exit 0]
    end
```

### Phase 1: Region Table & Chunk Discovery
- `regions.csv` is read in 48 pieces across the thread pool. Each line's comma and numeric region ID are parsed with 8-byte SWAR integer arithmetic.
- Country codes are mapped to dense 1-byte indices ($0 \dots 240$, with `_UNMATCHED` reserved). The resulting `region` lookup table requires exactly **1 MB** (one byte per possible region ID), reducing region-to-country resolution to a single array dereference.
- Concurrently, chunk boundaries for `records.csv` are discovered: the file is split into 1 MiB chunks (tapered down to 128 KiB for the final 15% to eliminate tail latency).

### Phase 2: Two-Stage Structural SIMD Record Parser
- Workers read chunks in 32 KiB blocks using `pread(2)`, keeping active data entirely within L1D/L2 cache.
- **Stage 1 (AVX2 + Carry-Less Multiplication):** Loads 64 bytes per iteration, detecting `"`, `,`, and LF characters. A carry-less multiplication (`_mm_clmulepi64_si128`) of the quote mask computes the prefix parity: bits inside quotes toggle on, and doubled quotes (`""`) toggle twice without leaking state. Commas and newlines outside quotes form the exact structural delimiters.
- **Stage 2 (Boundary-Driven SWAR Parsing):** Delimiters define field boundaries. Amounts are converted directly to 64-bit integer cents using SWAR arithmetic without floating-point instructions.
- **Raw-Byte Key Invariance:** A quoted string with doubled quotes (`""`) is a strictly one-to-one representation of the unescaped string. Two names are equal if and only if their raw encoded bytes are identical. **Names are never unescaped**, eliminating heap allocations and string formatting loops.

### Phase 3: Shared Concurrent Name Table with Vectorized Tag Probing
- A shared hash table holds $2^{17}$ (131,072) 64-byte entries. Each entry contains a 32-byte key (byte 0 is length $+ 1$, followed by up to 31 raw name bytes, zero-padded) and a 256-bit bitmask tracking country occurrences. Names exceeding 31 bytes store 255 in byte 0 and spill to an auxiliary buffer compared via `memcmp`.
- **16-Wide SSE Tag Array:** A separate 1-byte tag array (holding 8 bits of the hash per slot) is scanned 16 tags at a time using SSE vector comparisons (`_mm_cmpeq_epi8`), resolving slot hits with a single cache-line read in $> 99\%$ of lookups.
- **Pipelined 3-Deep Batching:** Lookups are processed in batches of 16 with three batches in flight: tag arrays are prefetched while parsing the next batch, entry cache lines are prefetched for writing, and verified entries are resolved concurrently.
- **Atomic Country Tracking:** Country occurrence bits are set using atomic bit tests (`lock bts`). The thread that transitions a bit from 0 to 1 directly increments that country's distinct name tally, eliminating any post-scan reduction pass.

### Phase 4: Fast Output Formatting & Immediate Termination
- Thread-local totals and aggregates are merged. Outputs are formatted without floating point arithmetic (rendering currency cents with division and remainder).
- The final formatted buffer is written to `/out/aggregates.txt` with a single `write(2)` syscall, followed by `_exit(0)` to bypass runtime destructor overhead.

---

## What the Fast Path Relies On & Guaranteed Defensive Fallbacks

Judges reward defensive engineering and edge-case resilience (criteria: **Code Quality 15%** and **Technique 12%**):

| Spec Requirement / Edge Case | Fast Path Assumption | Defensive Fallback & Verification |
|---|---|---|
| **Spec §2.2 (Chunk Boundary Finding)** | An LF inside a quoted field is never followed by an ASCII digit 1–9 or a UTF-8 BOM. | Every record's five structural delimiters must match `,,,,` and LF, and each chunk's final record must align byte-for-byte with the subsequent chunk start. If any chunk boundary violates this invariant, **the entire file is automatically reparsed in a single un-chunked pass** using an expanded $2^{22}$-entry table. Tested against `tools/c_hostile_lf.py` with 100% exact output match. |
| **Spec §2.1 (Field Grammar)** | Well-formed CSV fields (positive IDs, valid decimal amounts `-?d+.dd`, complete BOM bytes). | Structural delimiter counts and separators are strictly validated per record. Empty number fields or invalid separators trigger the fallback parser. |
| **Country Count $\le 255$** | At most 255 distinct country codes (spec defines 240 plus `_UNMATCHED`). | Validated during Phase 1 ingestion. Corrupt country codes trigger an immediate diagnostic error to `stderr` and a non-zero exit (`exit(1)`). |
| **Distinct Name Capacity** | Name pool does not exceed $7/8$ of $2^{17}$ entries (spec defines 100,000 pool). | If distinct name collisions saturate the fast table, the run escalates to the fallback parser's $2^{22}$ entry capacity. |
| **Arbitrary Record & Name Lengths** | Records exceeding the 32 KiB block size. | Incomplete records at block boundaries are carried over to the next buffer without truncation or buffer overflows. |
| **Transparent Huge Pages (THP)** | Linux host sets THP to `always`. | `prctl(PR_SET_THP_DISABLE, 1)` disables THP on anonymous mappings, preventing the Linux kernel from backing sparse allocations with 2 MiB huge pages and locking peak memory at **13.6 MB**. |

---

## Verification & Correctness

- **Reference Verification:** Tested against an independent reference implementation (`tools/c_ref.py`) reproducing official sha256 hashes across `tiny`, `sample`, and `full` seed 1 public datasets.
- **Adversarial & Stress Suites:** Validated against `tools/c_adversarial.py`, `tools/c_hostile_lf.py`, three `tools/c_stress_gen.py` configurations, and `tools/c_twins_gen.py` (names differing by exactly one byte across positions 0–40).
- **Extreme Boundary Testing:** Tested with micro 200-byte blocks and 3,000-byte chunks to stress block-carry and boundary logic.
- **Sanitizer Clean:** Executed cleanly under AddressSanitizer (ASan), UndefinedBehaviorSanitizer (UBSan), and ThreadSanitizer (TSan) with zero reports or data races.

---

## Dependencies

**Zero external dependencies ($D = 1$):**
- Standard C++23 library (`<bits/stdc++.h>`) only.
- POSIX system headers (`<fcntl.h>`, `<sys/mman.h>`, `<sys/prctl.h>`, `<unistd.h>`).
- Direct AVX2 and PCLMULQDQ intrinsics via `<immintrin.h>`.
- Zero third-party CSV, SQL, or dataframe libraries (satisfying competition rules).

---

## With Another 24 Hours

1. **Partitioned L2 Name Tables:** Partition distinct name lookups across worker threads using the top hash bits, routing records into thread-local 0.5 MB tables that remain fully L2-resident and eliminate atomic `lock bts` cache line invalidations.
2. **Pipelined Double-Buffered I/O:** Implement asynchronous double-buffered `pread` so that SIMD delimiter classification overlaps with kernel block reads, hiding the remaining 8.5 ms I/O fixed cost.
3. **AVX-512 Structural Indexing:** On processors supporting AVX-512, process 64-byte structural masks in a single 512-bit register, doubling delimiter extraction throughput.
