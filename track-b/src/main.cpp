// Track B (index), C++ version. Standard library plus x86 intrinsics and Linux system calls only.
//
// Design: the queries are parsed in 12 pieces at once and every distinct term is numbered in a small hash table
// (one 64-byte line per bucket: seven 44-bit-tag entries and an overflow word). A 1% sample of the corpus gives
// document frequencies: BOOL terms in over 1/32 of the documents become bitsets, the other BOOL terms posting
// lists, and each phrase gets its rarest term as a trigger. The corpus is scanned in 48 chunks, each indexed and
// answered on its own: an AVX2 pass finds the tokens (the document id on every line is masked off), a second loop
// looks the hashes up, a third records which terms each document contains, and phrases are checked around their
// trigger terms. Every BOOL query is then evaluated on the chunk's documents, strictly left to right.
#include <fcntl.h>
#include <immintrin.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <barrier>
#include <cstdint>
#include <cstring>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#ifdef SD_PHASES
#include <x86intrin.h>
#include <cstdio>
#include <ctime>
#endif
namespace {
using u8 = uint8_t;
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;

constexpr int kThreads = 12, kChunks = 48, kSamples = 12, kDocBits = 14;
constexpr u32 kCap = 1 << 15, kFlush = 384, kAhead = 16, kLines = 1 << 14;
constexpr u64 kTag = ~0xFFFFFULL, kPay = 0x3FFFF, kTop = 1ULL << 63;
constexpr u64 kOnes = 0x0101010101010101ULL;

// The bytes of a token's first and second word that belong to it, by its length (all of them from 16 on).
struct Masks {
  u64 m[32][2];
  constexpr Masks() : m() {
    for (int len = 0; len < 32; len++) {
      m[len][0] = len >= 8 ? ~0ULL : (1ULL << 8 * len) - 1;
      m[len][1] = len >= 16 ? ~0ULL : len <= 8 ? 0 : (1ULL << 8 * (len - 8)) - 1;
    }
  }
};
constexpr Masks kMasks;
#define kMask kMasks.m

#ifdef SD_PHASES
double now() { timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec * 1e-6; }
double g_ph[16];
u64 g_tc[16];
u64 g_cy[kThreads][8];
thread_local int t_tid;
#define PH(i) if (tid == 0) g_ph[i] = now(), g_tc[i] = __rdtsc();
#define CY(i) g_cy[tid][i] += __rdtsc();
#define CYS(i) g_cy[tid][i] -= __rdtsc();
#define CYT(i) g_cy[t_tid][i] += __rdtsc();
#define CYTS(i) g_cy[t_tid][i] -= __rdtsc();
#else
#define PH(i)
#define CY(i)
#define CYS(i)
#define CYT(i)
#define CYTS(i)
#endif

struct File {
  const char* p;
  size_t n;
};

// Maps a file read-only with a zero page after it, so that reads past the end are safe.
File map_file(const char* path) {
  int fd = open(path, O_RDONLY);
  struct stat st;
  fstat(fd, &st);
  size_t n = st.st_size;
  char* r = (char*)mmap(nullptr, n + 8192, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  mmap(r, n, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0);
  return {r, n};
}

File g_q, g_d;
u32 g_ndocs;

inline u64 load64(const char* p) {
  u64 x;
  memcpy(&x, p, 8);
  return x;
}
inline u64 fold(u64 a, u64 b) {
  unsigned __int128 m = (unsigned __int128)a * b;
  return (u64)m ^ (u64)(m >> 64);
}
constexpr u64 K0 = 0xA0761D6478BD642FULL, K1 = 0xE7037ED1A0B428DBULL, K2 = 0x8EBC6AF09C88C6E3ULL;

// The hash of a token of up to 16 lower-cased bytes, given as two zero-padded words.
inline u64 hash16(u64 w0, u64 w1) { return fold(w0 ^ K0, w1 ^ K1) | kTop; }

// The same hash for any length (the queries, and the rare corpus tokens over 16 bytes).
u64 hash_bytes(const char* s, size_t len, bool lower) {
  char b[32] = {};
  memcpy(b, s, std::min<size_t>(len, 32));
  if (lower)
    for (size_t i = 0; i < len && i < 32; i++) b[i] |= 0x20;
  u64 w[4];
  memcpy(w, b, 32);
  u64 h = fold(w[0] ^ K0, w[1] ^ K1);
  if (len > 16) h = fold(h ^ w[2], w[3] ^ K2);
  if (len > 32) h = fold(h, len * K2);  // never equal to a term of at most 32 bytes
  return h | kTop;
}

// ---- The term table. Bucket = 64 bytes: seven entries (44-bit tag of the hash | 18-bit payload) and a word that is
// non-zero if an entry of this bucket had to go to the overflow table. The bucket comes from the hash's low bits.
alignas(64) u64 g_tab[kLines * 8 + 8];
u64 g_ovf[8192];

u64 ovf_find(u64 h) {
  for (u32 i = (h >> 20) & 8191;; i = (i + 1) & 8191) {
    u64 e = g_ovf[i];
    if (!e || ((e ^ h) & kTag) == 0) return e;
  }
}

// The entry of the term with hash h, or 0. If its bucket overflowed and the term is not in it, `slow` is set to 1 and the
// caller has to ask ovf_find (kept out of the loops: a branch on this is hard to predict and costs a lot).
inline u64 lookup(u64 h, u32& slow) {
  const u64* l = g_tab + (h & (kLines - 1)) * 8;
  const __m256i t = _mm256_set1_epi64x(h & kTag), m = _mm256_set1_epi64x(kTag);
  __m256i a = _mm256_cmpeq_epi64(_mm256_and_si256(_mm256_load_si256((const __m256i*)l), m), t);
  __m256i b = _mm256_cmpeq_epi64(_mm256_and_si256(_mm256_load_si256((const __m256i*)(l + 4)), m), t);
  u32 hit = (_mm256_movemask_pd(_mm256_castsi256_pd(a)) | _mm256_movemask_pd(_mm256_castsi256_pd(b)) << 4) & 0x7F;
  u64 e = l[_tzcnt_u32(hit | 0x80)] & -(u64)(hit != 0);
  slow = (hit == 0) & (l[7] != 0);
  return e;
}
inline u64 lookup(u64 h) {
  u32 slow;
  u64 e = lookup(h, slow);
  return slow ? ovf_find(h) : e;
}

// Finds the term with hash h or adds it (any thread, at the same time), and returns its provisional id: the term's
// place in the table plus one (the overflow table's places come after the buckets').
constexpr u32 kOvfBase = kLines * 8, kMaxProv = kOvfBase + 8192;
bool claim(u64* slot, u64 h, u32 id, u32& out) {
  u64 e = 0, want = (h & kTag) | id;
  if (__atomic_compare_exchange_n(slot, &e, want, false, __ATOMIC_RELAXED, __ATOMIC_RELAXED)) return out = id, true;
  return ((e ^ h) & kTag) == 0 ? (out = e & kPay, true) : false;
}
u32 intern(u64 h) {
  u64* l = g_tab + (h & (kLines - 1)) * 8;
  u32 id = 0;
  for (int s = 0; s < 7; s++)
    if (claim(l + s, h, (h & (kLines - 1)) * 8 + s + 1, id)) return id;
  __atomic_store_n(l + 7, 1, __ATOMIC_RELAXED);
  for (u32 i = (h >> 20) & 8191;; i = (i + 1) & 8191)
    if (claim(g_ovf + i, h, kOvfBase + i + 1, id)) return id;
}
u8 g_used[kMaxProv + 1];  // by provisional id: bit 0 = in a BOOL query, bit 1 = in a phrase

// ---- Queries.
struct Query {
  u32 t[8];
  u8 ops[8];
  u8 n, phrase, has_or;
};
std::unique_ptr<Query[]> g_queries;
std::vector<u32> g_bool;  // the numbers of the BOOL queries
u32 g_nq;

std::vector<Query> g_piece[kThreads];
size_t g_psize[kThreads];

// Boundary i of n equal parts of f, moved to the start of the next line.
u64 cut(const File& f, int i, int n) {
  if (i == 0) return 0;
  if (i == n) return f.n;
  u64 at = f.n * i / n;
  at -= at > 0;
  return (const char*)memchr(f.p + at, '\n', f.n - at) - f.p + 1;
}

void parse(int tid, std::vector<Query>& out) {
  const char *p = g_q.p + cut(g_q, tid, kThreads), *end = g_q.p + cut(g_q, tid + 1, kThreads);
  while (p < end) {
    const char* e = (const char*)memchr(p, '\n', end - p);
    Query q = {};
    q.phrase = p[0] == 'P';
    p += q.phrase ? 8 : 5;  // "PHRASE \"" or "BOOL "
    for (;;) {
      const char* s = p;
      while (p < e && *p != ' ' && *p != '"') p++;
      u32 id = q.t[q.n++] = intern(hash_bytes(s, p - s, false));
      if (!(g_used[id] & 1 + q.phrase)) __atomic_fetch_or(&g_used[id], 1 + q.phrase, __ATOMIC_RELAXED);
      if (p >= e || *p == '"') break;
      p++;
      if (!q.phrase) {
        q.ops[q.n - 1] = *p;
        q.has_or |= *p == 'O';
        while (*p != ' ') p++;
        p++;
      }
    }
    out.push_back(q);
    p = e + 1;
  }
}

// ---- The result of a set operation on one chunk's documents: a sorted list of chunk-local ids, or a bitset.
struct Opnd {
  const u16* l;
  const u64* b;
  u32 n;
};

inline bool has(const u64* b, u32 d) { return b[d >> 6] >> (d & 63) & 1; }

u32 combine(u8 op, const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  u32 i = 0, j = 0, n = 0;
  while (i < na && j < nb) {
    if (a[i] < b[j]) {
      out[n] = a[i++];
      n += op != 'A';
    } else if (a[i] > b[j]) {
      out[n] = b[j++];
      n += op == 'O';
    } else {
      out[n] = a[i++];
      j++;
      n += op != 'N';
    }
  }
  if (op != 'A') {
    memcpy(out + n, a + i, (na - i) * 2);
    n += na - i;
  }
  if (op == 'O') {
    memcpy(out + n, b + j, (nb - j) * 2);
    n += nb - j;
  }
  return n;
}

struct Run {
  u16 *list, *tmp;
  u64* bits;
  u32 n, wl;
  bool dense;

  void start(const Opnd& x) {
    dense = x.b != nullptr;
    n = x.n;
    if (dense)
      memcpy(bits, x.b, wl * 8);
    else
      memcpy(list, x.l, n * 2);
  }
  void apply(u8 op, const Opnd& x) {
    if (!dense) {
      if (n == 0 && op != 'O') return;
      if (!x.b) {
        n = combine(op, list, n, x.l, x.n, tmp);
        std::swap(list, tmp);
      } else if (op != 'O') {
        u32 k = 0;
        for (u32 i = 0; i < n; i++) {
          list[k] = list[i];
          k += has(x.b, list[i]) == (op == 'A');
        }
        n = k;
      } else {
        memcpy(bits, x.b, wl * 8);
        for (u32 i = 0; i < n; i++) bits[list[i] >> 6] |= 1ULL << (list[i] & 63);
        dense = true;
      }
    } else if (!x.b) {
      if (op == 'A') {
        n = 0;
        for (u32 i = 0; i < x.n; i++) {
          list[n] = x.l[i];
          n += has(bits, x.l[i]);
        }
        dense = false;
      } else {
        for (u32 i = 0; i < x.n; i++) {
          u64 bit = 1ULL << (x.l[i] & 63);
          u64& w = bits[x.l[i] >> 6];
          w = op == 'O' ? w | bit : w & ~bit;
        }
      }
    } else {
      for (u32 i = 0; i < wl; i++) {
        u64 y = x.b[i];
        bits[i] = op == 'A' ? bits[i] & y : op == 'O' ? bits[i] | y : bits[i] & ~y;
      }
    }
  }
  // The set's size and the sum of its (chunk-local) ids.
  void size_sum(u64& c, u64& s) const {
    c = s = 0;
    if (!dense) {
      c = n;
      for (u32 i = 0; i < n; i++) s += list[i];
      return;
    }
    static const u64 kHas[6] = {0xAAAAAAAAAAAAAAAAULL, 0xCCCCCCCCCCCCCCCCULL, 0xF0F0F0F0F0F0F0F0ULL,
                                0xFF00FF00FF00FF00ULL, 0xFFFF0000FFFF0000ULL, 0xFFFFFFFF00000000ULL};
    for (u32 i = 0; i < wl; i++) {
      u64 w = bits[i], k = __builtin_popcountll(w), within = 0;
      for (int j = 0; j < 6; j++) within += (u64)__builtin_popcountll(w & kHas[j]) << j;
      c += k;
      s += i * 64 * k + within;
    }
  }
  // Up to ten smallest ids (chunk-local), ascending.
  u32 smallest(u32* out) const {
    u32 k = 0;
    if (!dense) {
      for (; k < n && k < 10; k++) out[k] = list[k];
      return k;
    }
    for (u32 i = 0; i < wl && k < 10; i++)
      for (u64 w = bits[i]; w && k < 10; w &= w - 1) out[k++] = i * 64 + _tzcnt_u64(w);
    return k;
  }
};

// ---- Results, by query: each thread adds its chunks' counts and sums into its own arrays (added up at the end); the
// ten smallest ids so far (and in the eleventh slot how many there are) are shared.
u32* g_first;
std::mutex g_mu[64];

// Adds one chunk's answer for query q: the chunk's documents are base + the ids in `run`.
void report(u32 q, const Run& run, u32 base, u32* count, u64* sum) {
  u64 c, s;
  CYTS(5) run.size_sum(c, s);
  CYT(5)
  if (!c) return;
  u32 cand[20], k = 0, m = 0;
  count[q] += c;
  sum[q] += s + c * base;
  u32* f = g_first + q * 11;
  // No lock when the ten smallest are known and all below this chunk (they only ever get smaller).
  if (__atomic_load_n(&f[10], __ATOMIC_RELAXED) == 10 && __atomic_load_n(&f[9], __ATOMIC_RELAXED) <= base) return;
  std::lock_guard<std::mutex> lock(g_mu[q & 63]);
  if (f[10] == 10 && f[9] <= base) return;
  for (; k < f[10]; k++) cand[k] = f[k];
  m = run.smallest(cand + k);
  for (u32 i = 0; i < m; i++) cand[k + i] += base;
  k += m;
  std::sort(cand, cand + k);
  f[10] = std::min<u32>(k, 10);
  memcpy(f, cand, f[10] * 4);
}

// ---- The classes of the terms, fixed after the sample. Ids: 0 = not a query term; 1..g_nd = BOOL terms with a
// bitset; then the other BOOL terms (g_ns of them, which get postings); then the terms that only phrases use.
u32 g_nd, g_ns, g_nt;
std::vector<u32> g_tstart, g_trig;  // by term: the phrases it triggers, as query << 3 | its place in the phrase

// ---- One thread's buffers.
struct Worker {
  u64* h;
  u32 *ids, *tp, *dend, *uniq, *slow;
  u16* stamp;
  u32 *df, *start, *pairs, pcap, *count;
  u64* sum;
  u16* post;
  u32 postcap;
  u64* rows;
  size_t rowscap;
  std::vector<u32> found;
  Run run;
};

template <class T>
T* alloc(size_t n) {
  return (T*)calloc(n ? n : 1, sizeof(T));
}

// Splits the text of b[from..to] (whole lines) into tokens and indexes them document by document. In sample mode
// only the document frequency of every term is counted; otherwise the chunk's bitset rows, pairs (term, document) and
// phrase matches are recorded. The first line's number is first_doc; local document numbers count from `base`.
template <bool kSample>
void scan(Worker& w, u64 from, u64 to, u32 first_doc, u32 base, u32 wl) {
  const char* d = g_d.p;
  u32 nt = 0, nd = 0, done = 0, np = 0;
  u64 carry_a = 0, carry_nl = 1, in_id = 0, open = ~0ULL;
  auto flush = [&]() {
    CYTS(2) u32 end = w.dend[nd - 1], nslow = 0;
    // Look up every token (id, and bit 17 if the term triggers phrases); the lines are prefetched a few tokens ahead.
    for (u32 i = 0; i < end; i++) {
      __builtin_prefetch(g_tab + (w.h[i + kAhead] & (kLines - 1)) * 8);
      u32 slow;
      w.ids[i] = lookup(w.h[i], slow) & kPay;
      w.slow[nslow] = i;
      nslow += slow;
    }
    for (u32 k = 0; k < nslow; k++) w.ids[w.slow[k]] = ovf_find(w.h[w.slow[k]]) & kPay;
    CYT(2) CYTS(3) u32 a = 0;
    for (u32 k = 0; k < nd; k++) {
      u32 b = w.dend[k], local = done + k, nu = 0, nh = 0;
      u16 cur = local + 1;
      w.stamp[0] = cur;
      // The distinct terms of the document, in order of first occurrence (a miss is id 0, which counts as seen).
      for (u32 i = a; i < b; i++) {
        u32 x = w.ids[i], id = x & (kSample ? kPay : 0x1FFFF);
        u16 prev = w.stamp[id];
        w.stamp[id] = cur;
        w.uniq[nu] = id;
        nu += prev != cur;
        w.tp[nh] = i;
        nh += !kSample & x >> 17;
      }
      if constexpr (kSample) {
        for (u32 j = 0; j < nu; j++) w.df[w.uniq[j]]++;
      } else {
        if (np + nu + 1 > w.pcap) {
          w.pcap *= 2;
          w.pairs = (u32*)realloc(w.pairs, w.pcap * 4);
        }
        u64 bit = 1ULL << (local & 63);
        for (u32 j = 0; j < nu; j++) {
          u32 id = w.uniq[j], sid = id - g_nd - 1;
          w.rows[(id <= g_nd ? id : 0) * wl + (local >> 6)] |= bit;
          w.pairs[np] = sid << kDocBits | local;
          np += sid < g_ns;
        }
        // Phrases whose trigger term occurs in this document.
        for (u32 ti = 0; ti < nh; ti++) {
          u32 i = w.tp[ti], t = w.ids[i] & 0x1FFFF;
          for (u32 x = g_tstart[t]; x < g_tstart[t + 1]; x++) {
            u32 e = g_trig[x], from_i = i - (e & 7);
            const Query& q = g_queries[e >> 3];
            if (from_i < a || from_i > b || b - from_i < q.n) continue;
            u32 j = 0;
            while (j < q.n && (w.ids[from_i + j] & 0x1FFFF) == q.t[j]) j++;
            if (j == q.n) w.found.push_back((e >> 3) << kDocBits | local);
          }
        }
      }
      a = b;
    }
    CYT(3)
    // Keep the tokens of the unfinished document.
    memmove(w.h, w.h + end, (nt - end) * 8);
    nt -= end;
    done += nd;
    nd = 0;
  };
  auto emit = [&](u64 s, u32 len) {
    const char* q = d + s;
    u64 w0 = (load64(q) | kOnes * 0x20) & kMask[len & 31][0], w1 = (load64(q + 8) | kOnes * 0x20) & kMask[len & 31][1];
    w.h[nt++] = len > 16 ? hash_bytes(q, len, true) : hash16(w0, w1);
  };
  const __m256i k20 = _mm256_set1_epi8(0x20), k60 = _mm256_set1_epi8(0x60), k7B = _mm256_set1_epi8(0x7B),
                k2F = _mm256_set1_epi8(0x2F), k3A = _mm256_set1_epi8(0x3A), k0A = _mm256_set1_epi8(10),
                k09 = _mm256_set1_epi8(9);
  for (u64 at = from; at < to; at += 64) {
    // Masks for 64 bytes: alphanumeric, newline, tab.
    u64 alnum = 0, nl = 0, tab = 0;
    for (int half = 0; half < 2; half++) {
      __m256i v = _mm256_loadu_si256((const __m256i*)(d + at + 32 * half));
      __m256i lo = _mm256_or_si256(v, k20);
      __m256i letter = _mm256_and_si256(_mm256_cmpgt_epi8(lo, k60), _mm256_cmpgt_epi8(k7B, lo));
      __m256i digit = _mm256_and_si256(_mm256_cmpgt_epi8(v, k2F), _mm256_cmpgt_epi8(k3A, v));
      alnum |= (u64)(u32)_mm256_movemask_epi8(_mm256_or_si256(letter, digit)) << (32 * half);
      nl |= (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, k0A)) << (32 * half);
      tab |= (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, k09)) << (32 * half);
    }
    if (to - at < 64) {
      u64 valid = (1ULL << (to - at)) - 1;
      alnum &= valid, nl &= valid, tab &= valid;
    }
    // The document id (from the start of a line to its tab) is not a token: mask it off.
    u64 toggle = (nl << 1 | carry_nl) | tab;
    u64 idm = _mm_cvtsi128_si64(_mm_clmulepi64_si128(_mm_set_epi64x(0, toggle), _mm_set1_epi8(-1), 0));
    idm ^= -in_id;
    in_id = idm >> 63;
    carry_nl = nl >> 63;
    alnum &= ~idm;
    u64 prev = alnum << 1 | carry_a;
    u64 starts = alnum & ~prev, ends = ~alnum & prev;
    carry_a = alnum >> 63;
    u32 nt0 = nt;
    // The k-th end closes the k-th start, or a token still open from the previous block.
    u64 e = ends, s = starts;
    if (open != ~0ULL && e) {
      emit(open, at + _tzcnt_u64(e) - open);
      e &= e - 1;
      open = ~0ULL;
    }
    while (e) {
      u64 st = at + _tzcnt_u64(s), en = at + _tzcnt_u64(e);
      s &= s - 1;
      e &= e - 1;
      emit(st, en - st);
    }
    if (s) open = at + _tzcnt_u64(s);
    // Document ends: the number of tokens that ended up to the newline.
    for (u64 n = nl; n; n &= n - 1) {
      u32 pos = _tzcnt_u64(n);
      w.dend[nd++] = nt0 + __builtin_popcountll(ends & (2ULL << pos) - 1);
    }
    if (nt >= kFlush || nd > 100) {
      if (nd) flush();
    }
  }
  if (nd) flush();
  if constexpr (!kSample) {
    // The chunk's postings by term (a counting sort of the pairs, which are in document order).
    CYTS(4)
    u32* start = w.start;
    memset(start, 0, (g_ns + 1) * 4);
    for (u32 i = 0; i < np; i++) start[w.pairs[i] >> kDocBits]++;
    for (u32 i = 1; i <= g_ns; i++) start[i] += start[i - 1];
    if (np > w.postcap) {
      w.postcap = np * 2;
      w.post = (u16*)realloc(w.post, w.postcap * 2);
    }
    for (u32 i = np; i-- > 0;) w.post[--start[w.pairs[i] >> kDocBits]] = w.pairs[i] & ((1 << kDocBits) - 1);
    CYT(4)
  }
}

std::barrier<> g_bar(kThreads);
std::vector<Worker*> g_workers(kThreads);
std::vector<u32> g_dfall, g_fin, g_pay;
std::atomic<u32> g_next{0};
u64 g_osz[kThreads];
int g_fd;

inline u8* put(u8* o, u64 x) {
  u8 buf[20];
  int i = 20;
  do {
    buf[--i] = '0' + x % 10;
    x /= 10;
  } while (x);
  memcpy(o, buf + i, 20 - i);
  return o + 20 - i;
}

u32 first_doc(u64 at) {
  u32 x = 0;
  for (const char* p = g_d.p + at; *p >= '0' && *p <= '9'; p++) x = x * 10 + (*p - '0');
  return x;
}

void worker(int tid) {
  // ---- Parse this thread's piece of the queries; every term is numbered as it is found.
  std::vector<Query>& pc = g_piece[tid];
  PH(0)
#ifdef SD_PHASES
  t_tid = tid;
#endif
  parse(tid, pc);
  g_psize[tid] = pc.size();
  g_bar.arrive_and_wait();
  PH(1)
  u32 nq = 0;
  {
    u32 o = 0;
    for (int t = 0; t < tid; t++) o += g_psize[t];
    memcpy(&g_queries[o], pc.data(), pc.size() * sizeof(Query));
    for (int t = 0; t < kThreads; t++) nq += g_psize[t];
    g_nq = nq;
    std::vector<Query>().swap(pc);
  }

  // ---- The sample: document frequencies of the terms in the first 12/1152 of the corpus.
  Worker w = {};
  w.df = alloc<u32>(kMaxProv + 1);
  w.stamp = alloc<u16>(kMaxProv + 1);
  w.h = alloc<u64>(kCap + 64);
  w.ids = alloc<u32>(kCap + 64);
  w.tp = alloc<u32>(kCap + 64);
  w.uniq = alloc<u32>(kCap + 64);
  w.slow = alloc<u32>(kCap + 64);
  w.count = alloc<u32>(nq);
  w.sum = alloc<u64>(nq);
  w.dend = alloc<u32>(256);
  g_workers[tid] = &w;
  u64 sa = cut(g_d, tid, 96 * kSamples), sb = cut(g_d, tid + 1, 96 * kSamples);
  scan<true>(w, sa, sb, first_doc(sa), 0, 0);
  g_bar.arrive_and_wait();
  PH(3)
  {
    size_t per = (kMaxProv + kThreads) / kThreads;
    for (size_t i = per * tid; i < std::min<size_t>(per * (tid + 1), kMaxProv + 1); i++)
      for (int t = 0; t < kThreads; t++) g_dfall[i] += g_workers[t]->df[i];
  }
  g_bar.arrive_and_wait();
  PH(4)

  // ---- Classes, ids and triggers (one thread, a few thousand steps per term).
  if (tid == 0) {
    // The sample's document count: the id of the first line after it.
    u64 end = cut(g_d, kSamples, 96 * kSamples);
    u32 seen = end >= g_d.n ? g_ndocs : first_doc(end);
    auto dense = [&](u32 p) { return (g_used[p] & 1) && (u64)g_dfall[p] * 32 >= seen; };
    u32 nd = 0, ns = 0;
    for (u32 p = 1; p <= kMaxProv; p++) {
      nd += dense(p);
      ns += (g_used[p] & 1) && !dense(p);
    }
    g_nd = nd, g_ns = ns;
    g_fin.assign(kMaxProv + 1, 0);
    u32 a = 0, b = 0, c = 0;
    for (u32 p = 1; p <= kMaxProv; p++)
      if (g_used[p]) g_fin[p] = dense(p) ? 1 + a++ : (g_used[p] & 1) ? nd + 1 + b++ : nd + ns + 1 + c++;
    g_nt = nd + ns + c;
    // Each phrase is checked where its rarest term occurs.
    std::vector<std::pair<u32, u32>> by_term;
    for (u32 q = 0; q < g_nq; q++) {
      const Query& qq = g_queries[q];
      if (!qq.phrase) continue;
      u32 best = 0;
      for (u32 i = 1; i < qq.n; i++)
        if (g_dfall[qq.t[i]] < g_dfall[qq.t[best]]) best = i;
      by_term.push_back({g_fin[qq.t[best]], q << 3 | best});
    }
    g_tstart.assign(g_nt + 2, 0);
    for (auto& e : by_term) g_tstart[e.first + 1]++;
    for (u32 t = 0; t <= g_nt; t++) g_tstart[t + 1] += g_tstart[t];
    g_trig.resize(by_term.size());
    std::vector<u32> fill(g_tstart.begin(), g_tstart.end());
    for (auto& e : by_term) g_trig[fill[e.first]++] = e.second;
    for (u32 q = 0; q < g_nq; q++)
      if (!g_queries[q].phrase) g_bool.push_back(q);
    g_pay.assign(kMaxProv + 1, 0);
    for (u32 p = 1; p <= kMaxProv; p++) {
      u32 f = g_fin[p];
      g_pay[p] = f | (g_tstart[f + 1] > g_tstart[f]) << 17;
    }
  }
  g_bar.arrive_and_wait();
  PH(5)
  // Rewrite the table with the final ids, and the queries.
  {
    u32 per = kLines / kThreads + 1;
    for (u32 l = per * tid; l < std::min<u32>(per * (tid + 1), kLines); l++)
      for (int s = 0; s < 7; s++) {
        u64& e = g_tab[l * 8 + s];
        if (e) e = (e & kTag) | g_pay[e & kPay];
      }
    if (tid == 0)
      for (u64& e : g_ovf)
        if (e) e = (e & kTag) | g_pay[e & kPay];
    u32 qa = g_nq / kThreads * tid, qb = tid == kThreads - 1 ? g_nq : g_nq / kThreads * (tid + 1);
    for (u32 q = qa; q < qb; q++)
      for (u32 j = 0; j < g_queries[q].n; j++) g_queries[q].t[j] = g_fin[g_queries[q].t[j]];
  }
  g_bar.arrive_and_wait();
  PH(6)

  // ---- The scan: chunks of whole lines, each indexed and answered on its own.
  free(w.df);
  w.df = nullptr;
  free(w.stamp);
  w.stamp = alloc<u16>(g_nt + 1);
  w.start = alloc<u32>(g_ns + 2);
  w.pcap = 1 << 16;
  w.pairs = alloc<u32>(w.pcap);
  w.postcap = 1 << 16;
  w.post = alloc<u16>(w.postcap);
  w.rows = nullptr;
  w.rowscap = 0;
  Run& run = w.run;
  run.list = alloc<u16>((1 << kDocBits) + 64);
  run.tmp = alloc<u16>((1 << kDocBits) + 64);
  run.bits = alloc<u64>((1 << kDocBits) / 64 + 8);
  for (;;) {
    int c = g_next.fetch_add(1, std::memory_order_relaxed);
    if (c >= kChunks) break;
    u64 from = cut(g_d, c, kChunks), to = cut(g_d, c + 1, kChunks);
    if (from == to) continue;
    u32 base = first_doc(from), end = to == g_d.n ? g_ndocs : first_doc(to), wl = (end - base + 63) / 64;
    if ((size_t)(g_nd + 1) * wl > w.rowscap) {
      free(w.rows);
      w.rowscap = (size_t)(g_nd + 1) * wl;
      w.rows = alloc<u64>(w.rowscap);
    } else {
      memset(w.rows, 0, (g_nd + 1) * wl * 8);
    }
    memset(w.stamp, 0, (g_nt + 1) * 2);
    w.found.clear();
    run.wl = wl;
    CYS(0) scan<false>(w, from, to, base, base, wl);
    CY(0) CYS(1)
    const u32* start = w.start;
    auto operand = [&](u32 t) {
      if (t <= g_nd) return Opnd{nullptr, w.rows + (size_t)t * wl, 0};
      u32 sid = t - g_nd - 1;
      return Opnd{w.post + start[sid], nullptr, start[sid + 1] - start[sid]};
    };
    // Phrases found while scanning, each document once.
    std::sort(w.found.begin(), w.found.end());
    w.found.erase(std::unique(w.found.begin(), w.found.end()), w.found.end());
    for (size_t i = 0; i < w.found.size();) {
      u32 q = w.found[i] >> kDocBits;
      run.dense = false;
      run.n = 0;
      for (; i < w.found.size() && w.found[i] >> kDocBits == q; i++) run.list[run.n++] = w.found[i] & ((1 << kDocBits) - 1);
      report(q, run, base, w.count, w.sum);
    }
    // Every BOOL query, on this chunk's documents.
    for (u32 q : g_bool) {
      const Query& qq = g_queries[q];
      Opnd x = operand(qq.t[0]);
      if (x.b == nullptr && x.n == 0 && !qq.has_or) continue;
      run.start(x);
      for (u32 k = 0; k + 1 < qq.n; k++) run.apply(qq.ops[k], operand(qq.t[k + 1]));
      report(q, run, base, w.count, w.sum);
    }
    CY(1)
  }
  g_bar.arrive_and_wait();
  PH(7)

  // ---- Output: each thread adds up the counts and sums of a twelfth of the queries, formats them, and writes them at
  // their place in the file.
  u32 q0 = (u64)g_nq * tid / kThreads, q1 = (u64)g_nq * (tid + 1) / kThreads;
  u8 *buf = alloc<u8>((size_t)(q1 - q0) * 144 + 64), *o = buf;
  for (u32 q = q0; q < q1; q++) {
    u32 c = 0;
    u64 sum = 0;
    for (int x = 0; x < kThreads; x++) c += g_workers[x]->count[q], sum += g_workers[x]->sum[q];
    o = put(o, c);
    *o++ = ' ';
    o = put(o, sum);
    const u32* f = g_first + q * 11;
    for (u32 i = 0; i < f[10]; i++) {
      *o++ = ' ';
      o = put(o, f[i]);
    }
    *o++ = '\n';
  }
  g_osz[tid] = o - buf;
  if (tid == 0) g_fd = open("/out/results.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  PH(8)
  g_bar.arrive_and_wait();
  off_t at = 0;
  for (int x = 0; x < tid; x++) at += g_osz[x];
  for (size_t done = 0; done < g_osz[tid];) {
    ssize_t n = pwrite(g_fd, buf + done, g_osz[tid] - done, at + done);
    if (n <= 0) break;
    done += n;
  }
  g_bar.arrive_and_wait();
  if (tid == 0) {
    PH(9)
#ifdef SD_PHASES
    {
      FILE* f = fopen("/proc/self/status", "r");
      char line[256];
      while (f && fgets(line, sizeof line, f))
        if (!strncmp(line, "VmHWM", 5) || !strncmp(line, "VmRSS", 5) || !strncmp(line, "RssAnon", 7) || !strncmp(line, "RssFile", 7)) fputs(line, stderr);
    }
    for (int i = 1; i < 10; i++) fprintf(stderr, "phase %d: %.2f ms (at %.2f)\n", i, g_ph[i] - g_ph[i - 1], g_ph[i] - g_ph[0]);
    double a = 0, b = 0, tpm = (g_tc[9] - g_tc[0]) / (g_ph[9] - g_ph[0]);
    for (int t = 0; t < kThreads; t++) a += g_cy[t][0], b += g_cy[t][1];
    fprintf(stderr, "scan %.1f ms/thread, eval %.1f ms/thread (tsc)\n", a / kThreads / tpm, b / kThreads / tpm);
    double l = 0, r = 0;
    for (int t = 0; t < kThreads; t++) l += g_cy[t][2], r += g_cy[t][3];
    fprintf(stderr, "  lookup loop %.1f ms/thread, record loop %.1f ms/thread (incl. sample)\n", l / kThreads / tpm, r / kThreads / tpm);
    double c4 = 0, c5 = 0;
    for (int t = 0; t < kThreads; t++) c4 += g_cy[t][4], c5 += g_cy[t][5];
    fprintf(stderr, "  counting sort %.1f ms/thread, size_sum %.1f ms/thread\n", c4 / kThreads / tpm, c5 / kThreads / tpm);
#endif
    _exit(0);
  }
}
}  // namespace

int main() {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
  g_q = map_file("/data/queries.txt");
  g_d = map_file("/data/docs.txt");
  const char* last = (const char*)memrchr(g_d.p, '\n', g_d.n - 1);
  g_ndocs = first_doc(last ? last + 1 - g_d.p : 0) + 1;
  size_t maxq = g_q.n / 7 + 16;  // a query takes at least 7 bytes
  g_queries.reset(new Query[maxq]);
  g_first = alloc<u32>(maxq * 11);
  g_dfall.assign(kMaxProv + 1, 0);
  std::thread th[kThreads - 1];
  for (int i = 0; i < kThreads - 1; i++) th[i] = std::thread(worker, i + 1);
  worker(0);
  for (auto& t : th) t.join();
}
