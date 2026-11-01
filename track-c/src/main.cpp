// Track C (join): aggregates of records.csv per country of its region_id (regions.csv), exactly per spec c-join.md.
// All threads run three phases separated by barriers: regions.csv into a byte table region_id -> country index; records.csv
// in 1 MiB chunks, each parsed with a SIMD structural index, its names looked up in batches in a shared table; then the
// distinct names per country are counted from the masks. A chunk starts at an LF followed by a digit 1-9 or a BOM, which
// inside a quoted field never happens (spec 2.2). Any record that does not fit makes the whole file be parsed again as one
// chunk, which needs no such assumption.
#include <algorithm>
#include <atomic>
#include <barrier>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <immintrin.h>
#include <memory>
#include <mutex>
#include <string>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <thread>
#include <unistd.h>
#include <vector>

using u8 = uint8_t;
using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;
using std::atomic;
constexpr auto RLX = std::memory_order_relaxed;
constexpr size_t BLOCK = 32 << 10, PAD = 128, BATCH = 16, CHUNK = 1 << 20;

static int rec_fd, reg_fd;
static size_t rec_len, reg_len;
static void read_at(int fd, u8 *dst, size_t n, size_t off) {
  while (n) {
    ssize_t k = pread(fd, dst, n, off);
    if (k <= 0) abort();
    dst += k, n -= k, off += k;
  }
}

// The 8 digits in w (first digit in the lowest byte, padding bytes zero) as a number.
static inline u64 digits8(u64 w) {
  w = ((w & 0x0F0F0F0F0F0F0F0FULL) * 2561) >> 8;
  w = ((w & 0x00FF00FF00FF00FFULL) * 6553601) >> 16;
  return ((w & 0x0000FFFF0000FFFFULL) * 42949672960001ULL) >> 32;
}
// The n digits (n >= 1) at p.
static inline u64 number(const u8 *p, size_t n) {
  if (n == 0) return 0; // not a valid field; the caller rejects it
  if (n > 8) {
    u64 v = 0;
    for (size_t i = 0; i < n; i++) v = v * 10 + (p[i] - '0');
    return v;
  }
  u64 w;
  memcpy(&w, p, 8);
  return digits8(w << (8 * (8 - n)));
}

// One distinct name in a cache line: a 32-byte key (byte 0 is the length + 1, or 255 for a name longer than 31 bytes whose
// bytes are in the spill area; then up to 31 bytes, zero padded) and a mask of the countries it occurred in. Word 0 of the
// key is stored last, with release, so that a reader who sees it nonzero sees the whole key.
struct alignas(64) Entry {
  atomic<u64> key[4];
  atomic<u64> mask[4];
};
// The key block of the name s[0..len) (1 readable byte before s, 32 after).
static inline __m256i block(const u8 *s, size_t len) {
  const __m256i iota = _mm256_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                                        23, 24, 25, 26, 27, 28, 29, 30, 31);
  int l = (int)std::min<size_t>(len, 31) + 1;
  __m256i v = _mm256_and_si256(_mm256_loadu_si256((const __m256i *)(s - 1)),
                               _mm256_cmpgt_epi8(_mm256_set1_epi8((char)l), iota));
  return _mm256_insert_epi8(v, (char)l, 0);
}
static inline u64 hash_of(__m256i v, size_t len) {
  __m128i x = _mm_xor_si128(_mm256_castsi256_si128(v), _mm256_extracti128_si256(v, 1));
  unsigned __int128 r = (unsigned __int128)((u64)_mm_cvtsi128_si64(x) ^ 0x9E3779B97F4A7C15ULL) *
                        ((u64)_mm_extract_epi64(x, 1) ^ 0xA0761D6478BD642FULL ^ len);
  return (u64)r ^ (u64)(r >> 64);
}

// Every distinct name, shared by all workers: entries placed by the hash; tags[i] holds 8 bits of the hash of the name in
// entries[i] (0: free), so a lookup reads one entry line in all but rare cases.
struct Names {
  size_t n;
  atomic<u8> *tags;
  Entry *e;
  u8 *spill;
  size_t spill_cap;
  alignas(128) atomic<size_t> used{0}, spilled{0};
  atomic<bool> full{false};
  explicit Names(int bits) : n(size_t(1) << bits), spill_cap(size_t(32) << bits) {
    tags = (atomic<u8> *)calloc(n + 16, 1);
    e = (Entry *)(((uintptr_t)calloc(n * 64 + 64, 1) + 63) & ~uintptr_t(63)); // calloc zeroes lazily, aligned by hand
    spill = (u8 *)calloc(spill_cap, 1);
  }
  static u8 tag(u64 h) { return u8(h >> 56) | 1; }
  // Prefetches, for writing, the entry a later lookup of hash h most likely reads; returns its index + 1 (0: none yet).
  u32 warm(u64 h) const {
    size_t i = h & (n - 1);
    __m128i v = _mm_loadu_si128((const __m128i *)(tags + i));
    u32 hit = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8((char)tag(h))));
    u32 free = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_setzero_si128())) | 1 << 16;
    hit &= (free & -free) - 1;
    if (!hit) return 0;
    size_t p = (i + __builtin_ctz(hit)) & (n - 1);
    _mm_prefetch((const char *)(e + p), _MM_HINT_ET0);
    return u32(p + 1);
  }
  bool same(size_t i, const u8 *s, size_t len, __m256i v) const {
    u64 first = e[i].key[0].load(std::memory_order_acquire);
    if (len > 31) {
      if ((first & 0xFF) != 255 || e[i].key[2].load(RLX) != len) return false;
      return memcmp(spill + e[i].key[1].load(RLX), s, len) == 0;
    }
    if (first != (u64)_mm256_extract_epi64(v, 0)) return false; // also false while the entry is unwritten
    return _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_load_si256((const __m256i *)&e[i].key[0]), v)) == -1;
  }
  // The entry of name s[0..len) (key block v, hash h), added if new; hint from warm().
  Entry *find(const u8 *s, size_t len, __m256i v, u64 h, u32 hint) {
    if (hint && same(hint - 1, s, len, v)) return e + hint - 1;
    u8 t = tag(h);
    for (size_t i = h & (n - 1);; i = (i + 1) & (n - 1)) {
      u8 x = tags[i].load(std::memory_order_acquire);
      if (x == 0) {
        if (used.fetch_add(1, RLX) >= n / 8 * 7 || spilled.load(RLX) + len > spill_cap) { // keep 1/8 free
          full.store(true, RLX);
          return e + i;
        }
        u8 zero = 0;
        if (tags[i].compare_exchange_strong(zero, t)) {
          if (i < 16) tags[n + i].store(t, RLX); // the copy a search near the end reads
          add(i, s, len, v);
          return e + i;
        }
        used.fetch_sub(1, RLX);
        x = zero;
      }
      if (x == t) {
        while (e[i].key[0].load(std::memory_order_acquire) == 0) _mm_pause(); // another worker is writing it
        if (same(i, s, len, v)) return e + i;
      }
    }
  }
  void add(size_t i, const u8 *s, size_t len, __m256i v) {
    u64 k[4];
    if (len <= 31) {
      _mm256_storeu_si256((__m256i *)k, v);
    } else {
      size_t at = spilled.fetch_add(len, RLX);
      memcpy(spill + at, s, len);
      k[0] = 255, k[1] = at, k[2] = len, k[3] = 0;
    }
    for (int j = 1; j < 4; j++) e[i].key[j].store(k[j], RLX);
    e[i].key[0].store(k[0], std::memory_order_release);
  }
};

struct Agg {
  u64 count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
  i64 sum = 0, min = INT64_MAX, max = INT64_MIN;
};
struct Rec {
  u64 id;
  i64 cents;
  const u8 *name;
  u64 hash;
  u32 region, len, name_bytes, note_bytes, hint;
};
struct Batch {
  Rec r[BATCH];
  size_t n = 0;
};

static u8 *region_of; // region_id -> country index + 1 (0: not in regions.csv)
static size_t max_id;
static Names *names;

// Per worker: the input block, separator positions, quote words with the number of quotes before each, three batches.
struct Worker {
  std::vector<u8> buf;
  std::unique_ptr<u32[]> at; // room for 64 separators per word, plus 8; never zeroed
  size_t at_cap = 0, seps = 0;
  std::vector<u64> qw;
  std::vector<u32> qb;
  Batch b[3];
  Agg agg[256];
  u64 counts[256] = {};
  u32 quotes(size_t pos) const { return qb[pos / 64] + __builtin_popcountll(qw[pos / 64] & ((1ULL << (pos % 64)) - 1)); }

  // Stage 1: commas and LFs outside quotes in buf[0..end) go to `at`; quote bits to qw/qb. A quote toggles being inside
  // quotes, and so does each half of a "" pair, so the bytes inside are the prefix XOR of the quote bits (a carry-less
  // multiplication by all ones).
  void structure(size_t end) {
    size_t words = (end + 63) / 64;
    if (at_cap < words * 64 + 8) at_cap = words * 64 + 8, at.reset(new u32[at_cap]);
    qw.resize(words + 1), qb.resize(words + 1);
    u64 inside = 0;
    u32 seen = 0, *out = at.get();
    size_t cnt = 0;
    const u8 *b = buf.data();
    for (size_t w = 0; w < words; w++) {
      __m256i lo = _mm256_loadu_si256((const __m256i *)(b + 64 * w)), hi = _mm256_loadu_si256((const __m256i *)(b + 64 * w + 32));
      auto bits = [&](char c) {
        __m256i x = _mm256_set1_epi8(c);
        return (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(lo, x)) |
               (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(hi, x)) << 32;
      };
      u64 q = bits('"');
      u64 in = (u64)_mm_cvtsi128_si64(_mm_clmulepi64_si128(_mm_set_epi64x(0, (i64)q), _mm_set1_epi8(-1), 0)) ^ inside;
      inside = (u64)((i64)in >> 63);
      u64 sep = (bits(',') | bits('\n')) & ~in;
      size_t k = __builtin_popcountll(sep);
      for (int j = 0; j < 8; j++) out[cnt + j] = u32(64 * w + __builtin_ctzll(sep | 1ULL << 63)), sep &= sep - 1;
      for (size_t j = 8; j < k; j++) out[cnt + j] = u32(64 * w + __builtin_ctzll(sep)), sep &= sep - 1;
      cnt += k, qw[w] = q, qb[w] = seen, seen += __builtin_popcountll(q);
    }
    qw[words] = 0, qb[words] = seen;
    seps = cnt;
  }

  // Stage 2: the record at `start` with separators s[0..5) into batch bt; false if its separators are not ",,,,\n".
  bool record(size_t start, const u32 *s, Batch &bt) {
    const u8 *b = buf.data();
    size_t e1 = s[0], e2 = s[1], e3 = s[2], e4 = s[3], e5 = s[4];
    if ((b[e1] ^ ',') | (b[e2] ^ ',') | (b[e3] ^ ',') | (b[e4] ^ ',') | (b[e5] ^ '\n')) return false;
    bool bom = b[start] == 0xEF;
    if (bom && (b[start + 1] != 0xBB || b[start + 2] != 0xBF)) return false;
    size_t p = start + 3 * bom;
    bool neg = b[e4 + 1] == '-';
    size_t dot = e5 - (b[e5 - 1] == '\r') - 3, whole = e4 + 1 + neg;
    if (e1 <= p || e4 == e3 + 1 || dot <= whole || e5 < e4 + 5) return false; // a number with no digits
    i64 cents = i64(number(b + whole, dot - whole) * 100 + (b[dot + 1] - '0') * 10 + (b[dot + 2] - '0'));
    // An unquoted text field has no quotes; a quoted one has its two delimiters and two more per "" pair. A name is looked
    // up by its bytes as written: writing a value with every " doubled is one to one, so two names are equal exactly
    // when these bytes are. Its length after unescaping is one less per pair.
    size_t qn = quotes(e2) - quotes(e1), qt = quotes(e3) - quotes(e2), nq = qn != 0, tq = qt != 0;
    size_t ns = e1 + 1 + nq, len = e2 - e1 - 1 - 2 * nq;
    Rec &r = bt.r[bt.n++];
    r.id = number(b + p, e1 - p), r.cents = neg ? -cents : cents, r.name = b + ns, r.len = u32(len);
    r.name_bytes = u32(len - (qn / 2 - nq)), r.note_bytes = u32(e3 - e2 - 1 - 2 * tq - (qt / 2 - tq));
    u64 region = number(b + e3 + 1, e4 - e3 - 1);
    r.region = region > max_id ? 0 : u32(region);
    r.hash = hash_of(block(r.name, len), len), r.hint = 0;
    _mm_prefetch((const char *)(names->tags + (r.hash & (names->n - 1))), _MM_HINT_T0);
    _mm_prefetch((const char *)(region_of + r.region), _MM_HINT_T0);
    return true;
  }
  void resolve(Batch &bt) {
    for (size_t i = 0; i < bt.n; i++) {
      Rec &r = bt.r[i];
      u8 c = region_of[r.region];
      Entry *e = names->find(r.name, r.len, block(r.name, r.len), r.hash, r.hint);
      e->mask[c / 64].fetch_or(1ULL << (c % 64), RLX);
      Agg &a = agg[c];
      a.count++, a.id_sum += r.id, a.sum += r.cents, a.min = std::min(a.min, r.cents), a.max = std::max(a.max, r.cents);
      a.name_bytes += r.name_bytes, a.note_bytes += r.note_bytes;
    }
    bt.n = 0;
  }
  void warm(Batch &bt) {
    for (size_t i = 0; i < bt.n; i++) bt.r[i].hint = names->warm(bt.r[i].hash);
  }

  // The records between lo and hi (record starts); false if any record does not fit.
  bool chunk(size_t lo, size_t hi) {
    size_t next = lo, keep = 0, now = 0;
    for (;;) {
      size_t n = std::min(BLOCK, hi - next), end = keep + n;
      buf.resize(end + PAD + 1);
      read_at(rec_fd, buf.data() + keep, n, next);
      next += n;
      bool last = next == hi;
      if (last && buf[end - 1] != '\n') buf[end++] = '\n'; // the last record may lack its terminator
      memset(buf.data() + end, 0, PAD);
      structure(end);
      size_t start = 0;
      for (size_t k = 0; k + 5 <= seps; k += 5) {
        if (!record(start, &at[k], b[now])) return false;
        start = at[k + 4] + 1;
        if (b[now].n == BATCH) {
          // The batch before has had its tags prefetched: read them and prefetch the entries. The one before that has had
          // its entries prefetched: resolve it.
          warm(b[(now + 2) % 3]), resolve(b[(now + 1) % 3]), now = (now + 1) % 3;
        }
      }
      for (int k : {1, 2, 0}) warm(b[(now + k) % 3]), resolve(b[(now + k) % 3]); // before the buffer changes
      if (last) return start == end;
      keep = end - start;
      memmove(buf.data(), buf.data() + start, keep);
    }
  }
};

// Start of the first record at or after cut: an LF followed by a digit 1-9 or a BOM.
static size_t record_start(size_t cut) {
  u8 buf[512];
  for (size_t at = cut - 1; at + 1 < rec_len; at += 511) {
    size_t n = std::min<size_t>(rec_len - at, 512);
    read_at(rec_fd, buf, n, at);
    for (size_t k = 0; k + 1 < n; k++)
      if (buf[k] == '\n' && (u8(buf[k + 1] - '1') < 9 || buf[k + 1] == 0xEF)) return at + k + 1;
  }
  return rec_len;
}

static std::string money(i64 v) {
  char s[32];
  u64 a = v < 0 ? u64(-v) : u64(v);
  snprintf(s, sizeof s, "%s%llu.%02llu", v < 0 ? "-" : "", (unsigned long long)(a / 100), (unsigned long long)(a % 100));
  return s;
}

// The whole computation; false if the fast path gives up (bits: size of the name table, chunk: piece size).
static bool run(int bits, size_t chunk, std::string &out) {
  // regions.csv: a header, then "id,CCC\n" lines in ascending id order, so the last line has the largest id.
  u8 head[64], tail[64];
  if (reg_len < 64) return false;
  read_at(reg_fd, head, 64, 0), read_at(reg_fd, tail, 64, reg_len - 64);
  size_t first = (u8 *)memchr(head, '\n', 64) - head + 1, last = 62;
  while (last && tail[last] != '\n') last--;
  max_id = 0;
  for (size_t i = last + 1; tail[i] >= '0' && tail[i] <= '9'; i++) max_id = max_id * 10 + tail[i] - '0';
  region_of = (u8 *)calloc(max_id + 1, 1);
  static atomic<u8> code_index[26 * 26 * 26];
  for (auto &c : code_index) c.store(0, RLX);
  std::vector<int> codes;
  std::mutex mu;
  names = new Names(bits);
  // Chunk j starts at the first record at or after its cut; the header has no quotes.
  u8 h0[256];
  read_at(rec_fd, h0, std::min<size_t>(rec_len, 256), 0);
  u8 *nl = (u8 *)memchr(h0, '\n', std::min<size_t>(rec_len, 256));
  if (!nl) return false;
  size_t body = nl - h0 + 1, chunks = (rec_len + chunk - 1) / chunk, pieces = 48;
  std::vector<size_t> starts(chunks + 1);
  auto cut = [&](size_t j) {
    if (j == 0) return first;
    if (j == pieces) return reg_len;
    size_t at = first + (reg_len - first) * j / pieces;
    u8 b[64];
    size_t n = std::min<size_t>(reg_len - at + 1, 64);
    read_at(reg_fd, b, n, at - 1);
    u8 *p = (u8 *)memchr(b, '\n', n);
    return p ? at + (p - b) : reg_len;
  };
  unsigned threads = std::max(1u, std::thread::hardware_concurrency());
  cpu_set_t set;
  if (sched_getaffinity(0, sizeof set, &set) == 0) threads = std::max(1, CPU_COUNT(&set));
  atomic<size_t> next[4] = {0, 0, 0, 0};
  atomic<bool> bad{false};
  std::barrier sync((ptrdiff_t)threads);
  std::vector<Worker> workers(threads);
  auto job = [&](int phase, size_t n) {
    size_t j = next[phase].fetch_add(1, RLX);
    return j < n ? j : SIZE_MAX;
  };
  auto work = [&](unsigned me) {
    Worker &w = workers[me];
    for (size_t j; (j = job(0, pieces)) != SIZE_MAX;) { // a line is "region_id,CCC\n"
      size_t a = cut(j), len = cut(j + 1) - a;
      std::vector<u8> t(len + 16);
      read_at(reg_fd, t.data(), len, a);
      for (size_t p = 0; p < len;) {
        u64 x;
        memcpy(&x, &t[p], 8);
        x ^= 0x2C2C2C2C2C2C2C2CULL;
        u64 z = (x - 0x0101010101010101ULL) & ~x & 0x8080808080808080ULL;
        const u8 *c = z ? &t[p] + __builtin_ctzll(z) / 8 : (const u8 *)memchr(&t[p], ',', len - p);
        size_t i = c ? size_t(c - t.data()) : p;
        if (i == p || i + 4 >= len + 1 || t[i] != ',' || t[i + 4] != '\n') { bad = true; break; }
        u64 id = number(&t[p], i - p);
        int c0 = t[i + 1] - 'A', c1 = t[i + 2] - 'A', c2 = t[i + 3] - 'A';
        if (id == 0 || id > max_id || unsigned(c0) > 25 || unsigned(c1) > 25 || unsigned(c2) > 25) { bad = true; break; }
        int code = c0 * 676 + c1 * 26 + c2;
        if (!code_index[code].load(RLX)) {
          std::lock_guard g(mu);
          if (!code_index[code].load(RLX)) {
            if (codes.size() == 255) { bad = true; break; }
            codes.push_back(code), code_index[code].store(u8(codes.size()), RLX);
          }
        }
        region_of[id] = code_index[code].load(RLX);
        p = i + 5;
      }
    }
    for (size_t j; (j = job(1, chunks + 1)) != SIZE_MAX;)
      starts[j] = j == 0 ? body : j == chunks ? rec_len : record_start(j * chunk);
    sync.arrive_and_wait();
    for (size_t j; !bad.load(RLX) && (j = job(2, chunks)) != SIZE_MAX;)
      if (starts[j] < starts[j + 1] && !w.chunk(starts[j], starts[j + 1])) bad = true;
    sync.arrive_and_wait();
    size_t used = names->n;
    for (size_t j; (j = job(3, (used + 4095) / 4096)) != SIZE_MAX;)
      for (size_t i = j * 4096; i < std::min(used, j * 4096 + 4096); i++)
        for (int k = 0; k < 4; k++)
          for (u64 m = names->e[i].mask[k].load(RLX); m; m &= m - 1) w.counts[k * 64 + __builtin_ctzll(m)]++;
  };
  std::vector<std::thread> pool;
  for (unsigned i = 1; i < threads; i++) pool.emplace_back(work, i);
  work(0);
  for (auto &t : pool) t.join();
  if (bad || names->full) return false;
  // The output: countries in the byte order of their codes, then _UNMATCHED (index 0).
  std::vector<std::pair<std::string, int>> order;
  for (int code : codes)
    order.push_back({{char('A' + code / 676), char('A' + code / 26 % 26), char('A' + code % 26)}, code_index[code].load(RLX)});
  std::sort(order.begin(), order.end());
  order.push_back({"_UNMATCHED", 0});
  for (auto &[key, c] : order) {
    Agg t;
    u64 distinct = 0;
    for (auto &w : workers) {
      const Agg &a = w.agg[c];
      t.count += a.count, t.id_sum += a.id_sum, t.sum += a.sum, t.name_bytes += a.name_bytes, t.note_bytes += a.note_bytes;
      t.min = std::min(t.min, a.min), t.max = std::max(t.max, a.max), distinct += w.counts[c];
    }
    if (!t.count) continue;
    out += key + " " + std::to_string(t.count) + " " + std::to_string(t.id_sum) + " " + money(t.sum) + " " + money(t.min) +
           " " + money(t.max) + " " + std::to_string(distinct) + " " + std::to_string(t.name_bytes) + " " +
           std::to_string(t.note_bytes) + "\n";
  }
  return true;
}

int main() {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0); // with huge pages every sparsely touched 2 MiB of the tables would be backed
  reg_fd = open("/data/regions.csv", O_RDONLY), rec_fd = open("/data/records.csv", O_RDONLY);
  struct stat st;
  if (reg_fd < 0 || rec_fd < 0) return 1;
  fstat(reg_fd, &st), reg_len = st.st_size, fstat(rec_fd, &st), rec_len = st.st_size;
  std::string out;
  // If the fast path gives up, parse everything as one chunk, with a table that holds a name per record.
  if (!run(17, CHUNK, out) && (out.clear(), !run(22, std::max<size_t>(rec_len, 1), out))) return 1;
  int fd = open("/out/aggregates.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (fd < 0 || write(fd, out.data(), out.size()) != (ssize_t)out.size()) return 1;
  _exit(0);
}
