#define TABLE2 1 // diagnostic: repeats this stage's work, output unchanged
// Track C (join): per-country aggregates of records.csv joined with regions.csv,
// exactly per spec c-join.md. One thread pool runs three phases separated by
// barriers: regions.csv into a byte table (region id -> country index);
// records.csv in 1 MiB chunks, each parsed with a SIMD structural index and its
// names looked up in batches in a shared table; then the distinct names per
// country are counted from the masks. A chunk starts at an LF followed by a
// digit 1-9 or a BOM, which never happens inside a quoted field (spec 2.2). If
// any record does not fit, the whole file is parsed again as one chunk.
#include <algorithm>
#include <atomic>
#include <barrier>
#include <cstdio>
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
using namespace std;
using u8 = uint8_t;
using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;
using V = __m256i;
constexpr auto RX = memory_order_relaxed, ACQ = memory_order_acquire;
constexpr size_t BLOCK = 32 << 10, PAD = 128, BATCH = 16;
static int fd_rec, fd_reg;
static size_t n_rec, n_reg, max_id;
static u8 *region; // region id -> country index (0: not a region)

static void rd(int fd, void *dst, size_t n, size_t off) {
  for (u8 *d = (u8 *)dst; n;) {
    ssize_t k = pread(fd, d, n, off);
    if (k <= 0) abort();
    d += k, n -= k, off += k;
  }
}
static u64 load8(const u8 *p) {
  u64 w;
  memcpy(&w, p, 8);
  return w;
}
// The n >= 1 digits at p (up to 8 at once, without a branch).
static u64 num(const u8 *p, size_t n) {
  if (n > 8) {
    u64 v = 0;
    for (size_t i = 0; i < n; i++) v = v * 10 + p[i] - '0';
    return v;
  }
  u64 w = n ? load8(p) << (64 - 8 * n) : 0;
  w = ((w & 0x0F0F0F0F0F0F0F0F) * 2561) >> 8;
  w = ((w & 0x00FF00FF00FF00FF) * 6553601) >> 16;
  return ((w & 0x0000FFFF0000FFFF) * 42949672960001) >> 32;
}

// A name's 32-byte key: byte 0 is its length + 1 (255 if longer than 31: then
// word 1 is its offset in the spill area and word 2 its length), then its
// bytes, zero padded. 1 readable byte before s, 32 after.
static V key(const u8 *s, size_t len) {
  V iota = _mm256_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14,
                            15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
                            28, 29, 30, 31);
  char l = char(min<size_t>(len, 31) + 1);
  V v = _mm256_loadu_si256((const V *)(s - 1));
  v = _mm256_and_si256(v, _mm256_cmpgt_epi8(_mm256_set1_epi8(l), iota));
  return _mm256_insert_epi8(v, l, 0);
}
static u64 hsh(V v, u64 len) {
  __m128i x = _mm256_castsi256_si128(v) ^ _mm256_extracti128_si256(v, 1);
  u64 a = _mm_cvtsi128_si64(x), b = _mm_extract_epi64(x, 1);
  auto r = (unsigned __int128)(a ^ 0x9E3779B97F4A7C15) *
           (b ^ 0xA0761D6478BD642F ^ len);
  return u64(r) ^ u64(r >> 64);
}

// A distinct name and a mask of the countries it occurred in, in one line. The
// key's word 0 is stored last with release: a reader who sees it nonzero sees
// the whole key.
struct alignas(64) Entry {
  atomic<u64> k[4], mask[4];
};
// Every distinct name, shared: entries placed by the hash; tags[i] holds 8
// bits of the hash of the name in e[i] (0: free).
struct Names {
  size_t n, cap;
  atomic<u8> *tags;
  Entry *e;
  u8 *spill;
  alignas(128) atomic<size_t> used{0}, spilled{0};
  atomic<bool> full{false};
  explicit Names(int bits) : n(size_t(1) << bits), cap(n * 256) {
    tags = (atomic<u8> *)calloc(n + 16, 1); // calloc zeroes lazily
    e = (Entry *)(((uintptr_t)calloc(n + 2, 64) + 63) & ~uintptr_t(63));
    spill = (u8 *)calloc(cap, 1);
  }
  // Prefetches for writing the entry a lookup of hash h will most likely
  // read; returns its index + 1 (0: none yet).
  u32 warm(u64 h) const {
    size_t i = h & (n - 1);
    // 16 tags in one load; a tag written meanwhile is seen old or new, and any
    // hit is checked again by find(), so the load is done as one instruction
    // the compiler does not reorder or split.
    __m128i v;
    asm volatile("vmovdqu %1, %0" : "=x"(v) : "m"(*(const char(*)[16])(tags + i)));
    u32 hit = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(h >> 56 | 1)));
    u32 free = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_setzero_si128()));
    hit &= ((free | 1 << 16) & -(free | 1 << 16)) - 1;
    if (!hit) return 0;
    size_t p = (i + __builtin_ctz(hit)) & (n - 1);
    _mm_prefetch((const char *)(e + p), _MM_HINT_ET0);
    return u32(p + 1);
  }
  bool same(size_t i, const u8 *s, size_t len, V v) const {
    u64 k0 = e[i].k[0].load(ACQ);
    if (len > 31)
      return (k0 & 255) == 255 && e[i].k[2].load(RX) == len &&
             !memcmp(spill + e[i].k[1].load(RX), s, len);
    if (k0 != u64(_mm256_extract_epi64(v, 0))) return false; // or unwritten
    V x = _mm256_cmpeq_epi8(_mm256_load_si256((const V *)e[i].k), v);
    return _mm256_movemask_epi8(x) == -1;
  }
  // The entry of name s[0..len) (key v, hash h), added if new.
  Entry *find(const u8 *s, size_t len, V v, u64 h, u32 hint) {
    if (hint && same(hint - 1, s, len, v)) return e + hint - 1;
    u8 t = h >> 56 | 1;
    for (size_t i = h & (n - 1);; i = (i + 1) & (n - 1)) {
      u8 x = tags[i].load(ACQ);
      if (!x) {
        // Keep 1/8 of the table free, so that every search ends.
        if (used.fetch_add(1, RX) >= n / 8 * 7 || spilled + len > cap)
          return full = true, e + n;
        if (tags[i].compare_exchange_strong(x, t)) {
          if (i < 16) tags[n + i] = t; // the copy a search near the end reads
          u64 k[4];
          if (len < 32) {
            _mm256_storeu_si256((V *)k, v);
          } else {
            k[1] = spilled.fetch_add(len, RX), k[0] = 255, k[2] = len, k[3] = 0;
            memcpy(spill + k[1], s, len);
          }
          for (int j = 3; j >= 0; j--) e[i].k[j].store(k[j], j ? RX : memory_order_release);
          return e + i;
        }
        used.fetch_sub(1, RX);
      }
      if (x == t) {
        while (!e[i].k[0].load(ACQ)) _mm_pause(); // another worker writes it
        if (same(i, s, len, v)) return e + i;
      }
    }
  }
};
static Names *names;

struct Agg {
  u64 count = 0, ids = 0, nb = 0, tb = 0;
  i64 sum = 0, lo = INT64_MAX, hi = INT64_MIN;
};
struct Rec {
  u64 id, hash;
  i64 cents;
  const u8 *name;
  u32 reg, len, nb, tb, hint;
};

struct Worker {
  vector<u8> buf;
  unique_ptr<u32[]> at; // separator positions, never zeroed
  size_t at_cap = 0, seps = 0;
  vector<u64> qw; // quote bits of each 64-byte word
  vector<u32> qb; // quotes before each word
  Rec b[3][BATCH];
  size_t bn[3] = {};
  Agg agg[256];
  u64 counts[256] = {};
  u32 quotes(size_t p) const {
    return qb[p / 64] + __builtin_popcountll(qw[p / 64] & ((1ULL << p % 64) - 1));
  }
  // Stage 1: commas and LFs outside quotes in buf[0..end) go to `at`. A quote
  // toggles being inside quotes, and so does each half of a "" pair, so the
  // bytes inside are the prefix XOR of the quote bits (a carry-less product).
  void structure(size_t end) {
    size_t words = (end + 63) / 64, cnt = 0;
    if (at_cap < words * 64 + 8) at.reset(new u32[at_cap = words * 64 + 8]);
    qw.resize(words + 1), qb.resize(words + 1);
    u64 inside = 0;
    u32 seen = 0, *out = at.get();
    for (size_t w = 0; w < words; w++) {
      V lo = _mm256_loadu_si256((const V *)&buf[64 * w]);
      V hi = _mm256_loadu_si256((const V *)&buf[64 * w + 32]);
      auto bits = [&](char c) {
        V x = _mm256_set1_epi8(c);
        u64 l = u32(_mm256_movemask_epi8(_mm256_cmpeq_epi8(lo, x)));
        return l | u64(u32(_mm256_movemask_epi8(_mm256_cmpeq_epi8(hi, x)))) << 32;
      };
      u64 q = bits('"');
      __m128i m = _mm_clmulepi64_si128(_mm_set_epi64x(0, q), _mm_set1_epi8(-1), 0);
      u64 in = _mm_cvtsi128_si64(m) ^ inside, sep = (bits(',') | bits('\n')) & ~in;
      inside = u64(i64(in) >> 63);
      size_t k = __builtin_popcountll(sep);
      for (int j = 0; j < 8; j++, sep &= sep - 1) // more than 8 is rare
        out[cnt + j] = u32(64 * w + __builtin_ctzll(sep | 1ULL << 63));
      for (size_t j = 8; j < k; j++, sep &= sep - 1)
        out[cnt + j] = u32(64 * w + __builtin_ctzll(sep));
      cnt += k, qw[w] = q, qb[w] = seen, seen += __builtin_popcountll(q);
    }
    qw[words] = 0, qb[words] = seen, seps = cnt;
  }
  // Stage 2: the record at s0 with separators s[0..5) into r; false if they
  // are not ",,,,\n". An unquoted field has no quotes; a quoted one has two
  // delimiters and two more per "" pair. A name is looked up by its bytes as
  // written: writing a value with every " doubled is one to one, so two names
  // are equal exactly when these bytes are.
  bool record(size_t s0, const u32 *s, Rec &r) {
    const u8 *b = buf.data();
    size_t e1 = s[0], e2 = s[1], e3 = s[2], e4 = s[3], e5 = s[4];
    if ((b[e1] ^ ',') | (b[e2] ^ ',') | (b[e3] ^ ',') | (b[e4] ^ ',') |
        (b[e5] ^ '\n'))
      return false;
    bool bom = b[s0] == 0xEF, neg = b[e4 + 1] == '-';
    if (bom && (b[s0 + 1] != 0xBB || b[s0 + 2] != 0xBF)) return false;
    size_t p = s0 + 3 * bom, dot = e5 - (b[e5 - 1] == '\r') - 3;
    size_t whole = e4 + 1 + neg, qn = quotes(e2) - quotes(e1);
    size_t qt = quotes(e3) - quotes(e2), nq = qn != 0, tq = qt != 0;
    if (e1 <= p || e4 == e3 + 1 || dot <= whole || e5 < e4 + 5) return false;
    i64 c = num(b + whole, dot - whole) * 100 + (b[dot + 1] - '0') * 10 +
            (b[dot + 2] - '0');
    size_t len = e2 - e1 - 1 - 2 * nq;
    u64 reg = num(b + e3 + 1, e4 - e3 - 1);
    r = {num(b + p, e1 - p), 0, neg ? -c : c, b + e1 + 1 + nq, 0, u32(len),
         u32(len - (qn / 2 - nq)), u32(e3 - e2 - 1 - tq - qt / 2), 0};
    r.reg = reg > max_id ? 0 : u32(reg), r.hash = hsh(key(r.name, len), len);
    _mm_prefetch((const char *)(names->tags + (r.hash & (names->n - 1))), _MM_HINT_T0);
    _mm_prefetch((const char *)(region + r.reg), _MM_HINT_T0);
    return true;
  }
  void warm(int k) {
    for (size_t i = 0; i < bn[k]; i++) b[k][i].hint = names->warm(b[k][i].hash);
  }
  void resolve(int k) {
    for (size_t i = 0; i < bn[k]; i++) {
      Rec &r = b[k][i];
      u8 c = region[r.reg];
      Entry *e = names->find(r.name, r.len, key(r.name, r.len), r.hash, r.hint);
      // The one worker that sets a bit counts the name for the country.
      u64 bit = 1ULL << (c & 63);
      counts[c] += !(e->mask[c >> 6].fetch_or(bit, RX) & bit);
#ifdef TABLE2
      { Entry *e2 = names->find(r.name, r.len, key(r.name, r.len), r.hash, 0); counts[c] += !(e2->mask[c >> 6].fetch_or(bit, RX) & bit); }
#endif
      Agg &a = agg[c];
      a.count++, a.ids += r.id, a.sum += r.cents, a.nb += r.nb, a.tb += r.tb;
      a.lo = min(a.lo, r.cents), a.hi = max(a.hi, r.cents);
    }
    bn[k] = 0;
  }
  // The records between the record starts lo and hi; false if one does not
  // fit. Three batches are in flight: one is parsed while the one before has
  // its tags read and its entry prefetched, and the one before that resolved.
  bool chunk(size_t lo, size_t hi) {
    for (size_t next = lo, keep = 0, now = 0;;) {
      size_t n = min(BLOCK, hi - next), end = keep + n, start = 0;
      buf.resize(end + PAD + 1), rd(fd_rec, &buf[keep], n, next);
#ifdef READ2
      { static thread_local vector<u8> scratch(BLOCK); rd(fd_rec, scratch.data(), n, (next + n_rec / 2) % (n_rec - n)); }
#endif
      next += n;
      if (next == hi && buf[end - 1] != '\n') buf[end++] = '\n'; // unterminated
      memset(&buf[end], 0, PAD), structure(end);
#ifdef STRUCT2
      structure(end);
#endif
      for (size_t k = 0; k + 5 <= seps; k += 5) {
#ifdef PARSE2
        { Rec scratch; if (!record(start, &at[k], scratch)) return false; asm volatile("" ::"r"(&scratch) : "memory"); }
#endif
        if (!record(start, &at[k], b[now][bn[now]++])) return false;
        start = at[k + 4] + 1;
        if (bn[now] == BATCH) warm((now + 2) % 3), resolve((now + 1) % 3), now = (now + 1) % 3;
      }
      for (int k : {1, 2, 0}) warm((now + k) % 3), resolve((now + k) % 3);
      if (next == hi) return start == end;
      keep = end - start, memmove(&buf[0], &buf[start], keep);
    }
  }
};

// Start of the first record at or after cut: an LF then a digit 1-9 or a BOM.
static size_t record_start(size_t cut) {
  u8 b[512];
  for (size_t at = cut - 1; at + 1 < n_rec; at += 511) {
    size_t n = min<size_t>(n_rec - at, 512);
    rd(fd_rec, b, n, at);
    for (size_t k = 0; k + 1 < n; k++)
      if (b[k] == '\n' && (u8(b[k + 1] - '1') < 9 || b[k + 1] == 0xEF))
        return at + k + 1;
  }
  return n_rec;
}
// The start of the first line of regions.csv at or after at.
static size_t line_start(size_t at) {
  u8 b[64];
  size_t n = min<size_t>(n_reg - at + 1, 64);
  rd(fd_reg, b, n, at - 1);
  u8 *p = (u8 *)memchr(b, '\n', n);
  return p ? at + (p - b) : n_reg;
}

// The output; empty if the fast path gives up (bits: name table size).
static string run(int bits, size_t chunk) {
  // regions.csv: a header, then "id,CCC\n" lines by ascending id.
  u8 h[64], t[64];
  if (n_reg < 64) return {};
  rd(fd_reg, h, 64, 0), rd(fd_reg, t, 64, n_reg - 64);
  size_t first = (u8 *)memchr(h, '\n', 64) - h + 1, last = 62, pieces = 48;
  while (last && t[last] != '\n') last--;
  for (max_id = 0; t[++last] - '0' < 10u;) max_id = max_id * 10 + t[last] - '0';
  region = (u8 *)calloc(max_id + 1, 1), names = new Names(bits);
  static atomic<u8> idx[26 * 26 * 26];
  for (auto &c : idx) c = 0;
  vector<int> codes;
  mutex mu;
  u8 h0[256];
  size_t hn = min<size_t>(n_rec, 256);
  rd(fd_rec, h0, hn, 0);
  u8 *nl = (u8 *)memchr(h0, '\n', hn); // the header has no quotes
  if (!nl) return {};
  // Cuts every `chunk` bytes, then (past 85% of the file) every chunk / 8, so that the workers finish together.
  vector<size_t> cuts{0}, starts;
  while (cuts.back() < n_rec) cuts.push_back(cuts.back() + (cuts.back() < n_rec / 20 * 17 ? chunk : chunk / 8));
  size_t chunks = cuts.size() - 1, threads = thread::hardware_concurrency();
  starts.resize(chunks + 1);
  cpu_set_t set;
  if (!sched_getaffinity(0, sizeof set, &set)) threads = CPU_COUNT(&set);
  threads = max<size_t>(threads, 1);
  atomic<size_t> next[4] = {0, 0, 0, 0};
  atomic<bool> bad{false};
  barrier sync((ptrdiff_t)threads);
  vector<Worker> ws(threads);
  auto job = [&](int phase, size_t n) {
    size_t j = next[phase].fetch_add(1, RX);
    return j < n ? j : SIZE_MAX;
  };
  auto work = [&](size_t me) {
    for (size_t j; (j = job(0, pieces)) != SIZE_MAX;) { // "id,CCC\n" lines
      size_t a = j ? line_start(first + (n_reg - first) * j / pieces) : first;
      size_t len = (j + 1 == pieces ? n_reg : line_start(first + (n_reg - first) * (j + 1) / pieces)) - a;
      vector<u8> r(len + 16);
      rd(fd_reg, r.data(), len, a);
      for (size_t p = 0, i; p < len && !bad; p = i + 5) {
        u64 x = load8(&r[p]) ^ 0x2C2C2C2C2C2C2C2C;
        u64 z = (x - 0x0101010101010101) & ~x & 0x8080808080808080;
        u8 *cm = z ? &r[p] + __builtin_ctzll(z) / 8 : (u8 *)memchr(&r[p], ',', len - p);
        i = cm ? cm - r.data() : p;
        u64 id = num(&r[p], i - p);
        int c0 = r[i + 1] - 'A', c1 = r[i + 2] - 'A', c2 = r[i + 3] - 'A';
        if (i == p || i + 4 > len || r[i + 4] != '\n' || !id || id > max_id ||
            unsigned(c0) > 25 || unsigned(c1) > 25 || unsigned(c2) > 25) {
          bad = true;
          break;
        }
        int code = c0 * 676 + c1 * 26 + c2;
        if (!idx[code].load(RX)) {
          lock_guard g(mu);
          if (codes.size() == 255) bad = true;
          else if (!idx[code]) codes.push_back(code), idx[code] = u8(codes.size());
        }
        region[id] = idx[code].load(RX);
      }
    }
    for (size_t j; (j = job(1, chunks + 1)) != SIZE_MAX;)
      starts[j] = !j ? nl - h0 + 1 : j == chunks ? n_rec : record_start(cuts[j]);
    sync.arrive_and_wait();
    for (size_t j; !bad && (j = job(2, chunks)) != SIZE_MAX;)
      if (starts[j] < starts[j + 1] && !ws[me].chunk(starts[j], starts[j + 1]))
        bad = true;
  };
  vector<thread> pool;
  for (size_t i = 1; i < threads; i++) pool.emplace_back(work, i);
  work(0);
  for (auto &th : pool) th.join();
  if (bad || names->full) return {};
  // Countries in the byte order of their codes, then _UNMATCHED (index 0).
  vector<pair<string, int>> order;
  for (int c : codes)
    order.push_back({{char('A' + c / 676), char('A' + c / 26 % 26), char('A' + c % 26)}, idx[c]});
  sort(order.begin(), order.end()), order.push_back({"_UNMATCHED", 0});
  string out;
  auto money = [](i64 v) {
    u64 a = v < 0 ? -v : v;
    return (v < 0 ? "-" : "") + to_string(a / 100) + (a % 100 < 10 ? ".0" : ".") + to_string(a % 100);
  };
  for (auto &[name, c] : order) {
    Agg t;
    u64 d = 0;
    for (auto &w : ws) {
      Agg &a = w.agg[c];
      t.count += a.count, t.ids += a.ids, t.sum += a.sum, t.nb += a.nb, t.tb += a.tb;
      t.lo = min(t.lo, a.lo), t.hi = max(t.hi, a.hi), d += w.counts[c];
    }
    if (t.count)
      out += name + " " + to_string(t.count) + " " + to_string(t.ids) + " " + money(t.sum) + " " + money(t.lo) + " " +
             money(t.hi) + " " + to_string(d) + " " + to_string(t.nb) + " " + to_string(t.tb) + "\n";
  }
  return out;
}

int main() {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0); // huge pages would back sparse tables
  fd_reg = open("/data/regions.csv", O_RDONLY), fd_rec = open("/data/records.csv", O_RDONLY);
  struct stat st;
  if (fd_reg < 0 || fd_rec < 0) return 1;
  fstat(fd_reg, &st), n_reg = st.st_size, fstat(fd_rec, &st), n_rec = st.st_size;
  // If the fast path gives up: one chunk, and a table that can hold a name per record.
  string out = run(17, 1 << 20);
  if (out.empty()) out = run(22, max<size_t>(n_rec, 1));
  int fd = open("/out/aggregates.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (out.empty() || fd < 0 || write(fd, out.data(), out.size()) != (ssize_t)out.size()) return 1;
  _exit(0);
}
