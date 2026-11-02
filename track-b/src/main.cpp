// Track B (index), C++23 version. Standard library, x86 SIMD intrinsics, and Linux syscalls only.
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
#include <thread>

namespace {
using u8 = uint8_t;
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;

constexpr int kThreads = 12, kChunks = 48, kSamples = 12, kDocBits = 14;
constexpr u32 kCap = 1 << 14, kFlush = 384, kAhead = 16, kLines = 1 << 14;
constexpr u32 kOvfSize = 1 << 13, kOvfMask = kOvfSize - 1;
constexpr u32 kOvfBase = kLines * 8, kMaxProv = kOvfBase + kOvfSize;
constexpr u32 kMaxPairs = 300000, kMaxFound = 1 << 15;
constexpr u64 kTag = ~0x7FFFFFFFULL, kPay = 0x3FFFFFFFULL, kOvf = 1ULL << 30, kTop = 1ULL << 63;
constexpr u64 kOnes = 0x0101010101010101ULL;

struct File {
  const char* p;
  size_t n;
};

File map_file(const char* path) {
  int fd = open(path, O_RDONLY);
  struct stat st;
  fstat(fd, &st);
  size_t n = st.st_size;
  char* r = static_cast<char*>(
      mmap(nullptr, n + 8192, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  if (n > 0) mmap(r, n, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0);
  close(fd);
  return {r, n};
}

u8* g_arena;
std::atomic<size_t> g_aoff{0};

template <class T>
T* arena_alloc(size_t n, bool zero = false) {
  size_t bytes = ((n ? n : 1) * sizeof(T) + 63) & ~size_t(63);
  size_t off = g_aoff.fetch_add(bytes, std::memory_order_relaxed);
  T* ptr = reinterpret_cast<T*>(g_arena + off);
  if (zero) memset(ptr, 0, bytes);
  return ptr;
}

File g_q, g_d;
u32 g_ndocs;

inline u64 load64(const char* p) {
  u64 x;
  memcpy(&x, p, 8);
  return x;
}

inline u64 fold(u64 a, u64 b) {
  unsigned __int128 m = static_cast<unsigned __int128>(a) * b;
  return static_cast<u64>(m) ^ static_cast<u64>(m >> 64);
}

constexpr u64 K0 = 0xA0761D6478BD642FULL, K1 = 0xE7037ED1A0B428DBULL;
constexpr u64 K2 = 0x8EBC6AF09C88C6E3ULL;

inline u64 hash16(u64 w0, u64 w1) { return fold(w0 ^ K0, w1 ^ K1) | kTop; }

__attribute__((noinline)) u64 hash_bytes(const char* s, size_t len, bool lower) {
  char b[32] = {};
  size_t clen = std::min<size_t>(len, 32);
  memcpy(b, s, clen);
  if (lower) {
    for (size_t i = 0; i < clen; i++) b[i] |= 0x20;
  }
  u64 w[4];
  memcpy(w, b, 32);
  u64 h = fold(w[0] ^ K0, w[1] ^ K1);
  if (len > 16) h = fold(h ^ w[2], w[3] ^ K2);
  if (len > 32) h = fold(h, len * K2);
  return h | kTop;
}

alignas(64) u64 g_tab[kLines * 8 + 8];
alignas(64) u64 g_ovf[kOvfSize];
u8* g_used;

inline u32 line_of(u64 h) { return static_cast<u32>(h >> 17) & (kLines - 1); }

inline u64 ovf_find(u64 h) {
  for (u32 i = static_cast<u32>(h >> 31) & kOvfMask;; i = (i + 1) & kOvfMask) {
    u64 e = g_ovf[i];
    if (!e || ((e ^ h) & kTag) == 0) return e;
  }
}

inline u64 lookup(u64 h, u32& slow) {
  const u64* l = g_tab + line_of(h) * 8;
  const __m256i t = _mm256_set1_epi64x(h & kTag), m = _mm256_set1_epi64x(kTag);
  __m256i a = _mm256_cmpeq_epi64(
      _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l)), m), t);
  __m256i b = _mm256_cmpeq_epi64(
      _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l + 4)), m), t);
  u32 hit = _mm256_movemask_pd(_mm256_castsi256_pd(a)) |
            (_mm256_movemask_pd(_mm256_castsi256_pd(b)) << 4);
  u64 e = l[_tzcnt_u32(hit) & 7] & -static_cast<u64>(hit != 0);
  slow = (hit == 0) & static_cast<u32>((l[(h >> 31) & 7] >> 30) & 1);
  return e;
}

inline bool claim(u64* slot, u64 h, u32 id, u32& out) {
  u64 e = 0, want = (h & kTag) | id;
  if (__atomic_compare_exchange_n(slot, &e, want, false, __ATOMIC_RELAXED,
                                  __ATOMIC_RELAXED)) {
    out = id;
    return true;
  }
  if (((e ^ h) & kTag) == 0) {
    out = static_cast<u32>(e & 0x3FFFF);
    return true;
  }
  return false;
}

u32 intern(u64 h) {
  u32 ln = line_of(h);
  u64* l = g_tab + ln * 8;
  u32 id = 0;
  for (int s = 0; s < 8; s++) {
    if (claim(l + s, h, ln * 8 + s + 1, id)) return id;
  }
  __atomic_fetch_or(&l[(h >> 31) & 7], kOvf, __ATOMIC_RELAXED);
  for (u32 i = static_cast<u32>(h >> 31) & kOvfMask;; i = (i + 1) & kOvfMask) {
    if (claim(g_ovf + i, h, kOvfBase + i + 1, id)) return id;
  }
}

struct Query {
  u32 t[8];
  u32 gen[4];
  u32 qid;
  u32 must;
  u8 ops[8];
  u8 n, ng, phrase, has_or;
};

Query* g_queries;
u32* g_qstart;
u32* g_qlist;
u32* g_tstart;
u64* g_trig;
u16* g_tmap;
u64* g_has_q;
u32 g_nq;

Query* g_piece[kThreads];
u32 g_psize[kThreads];

u64 cut(const File& f, int i, int n) {
  if (i == 0 || f.n == 0) return 0;
  if (i == n) return f.n;
  u64 at = f.n * i / n;
  if (at > 0) at--;
  return static_cast<const char*>(memchr(f.p + at, '\n', f.n - at)) - f.p + 1;
}

void parse(int tid, Query* out, u32& count) {
  const char* p = g_q.p + cut(g_q, tid, kThreads);
  const char* end = g_q.p + cut(g_q, tid + 1, kThreads);
  u32 cnt = 0;
  while (p < end) {
    const char* e = static_cast<const char*>(memchr(p, '\n', end - p));
    Query q = {};
    q.phrase = (p[0] == 'P');
    p += q.phrase ? 8 : 5;
    u8 bit = static_cast<u8>(1 + q.phrase);
    for (;;) {
      const char* s = p;
      while (p < e && *p != ' ' && *p != '"') p++;
      u32 len = static_cast<u32>(p - s);
      u64 h = (len > 16)
                  ? hash_bytes(s, len, true)
                  : hash16(_bzhi_u64(load64(s) | (kOnes * 0x20), len << 3),
                           _bzhi_u64(load64(s + 8) | (kOnes * 0x20),
                                     len > 8 ? (len - 8) << 3 : 0));
      u32 id = q.t[q.n++] = intern(h);
      if (!(g_used[id] & bit)) __atomic_fetch_or(&g_used[id], bit, __ATOMIC_RELAXED);
      if (p >= e || *p == '"') break;
      p++;
      if (!q.phrase) {
        q.ops[q.n - 1] = static_cast<u8>(*p);
        q.has_or |= (*p == 'O');
        while (*p != ' ') p++;
        p++;
      }
    }
    out[cnt++] = q;
    p = e + 1;
  }
  count = cnt;
}

struct Opnd {
  const u16* l;
  const u64* b;
  u32 n;
};

inline bool has(const u64* b, u32 d) { return (b[d >> 6] >> (d & 63)) & 1; }

inline u32 intersect_lists(const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  if (!na || !nb) return 0;
  if (a[na - 1] < b[0] || b[nb - 1] < a[0]) return 0;
  u32 i = 0, j = 0, n = 0;
  while (i < na && j < nb) {
    if (a[i] == b[j]) {
      out[n++] = a[i++];
      j++;
    } else if (a[i] < b[j]) {
      i++;
    } else {
      j++;
    }
  }
  return n;
}

inline u32 diff_lists(const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  if (!na) return 0;
  if (!nb || a[na - 1] < b[0] || b[nb - 1] < a[0]) {
    memcpy(out, a, na * 2);
    return na;
  }
  u32 i = 0, j = 0, n = 0;
  while (i < na && j < nb) {
    if (a[i] < b[j]) {
      out[n++] = a[i++];
    } else if (a[i] > b[j]) {
      j++;
    } else {
      i++;
      j++;
    }
  }
  if (i < na) {
    memcpy(out + n, a + i, (na - i) * 2);
    n += na - i;
  }
  return n;
}

inline u32 union_lists(const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  if (!na) { memcpy(out, b, nb * 2); return nb; }
  if (!nb) { memcpy(out, a, na * 2); return na; }
  u32 i = 0, j = 0, n = 0;
  while (i < na && j < nb) {
    if (a[i] < b[j]) {
      out[n++] = a[i++];
    } else if (a[i] > b[j]) {
      out[n++] = b[j++];
    } else {
      out[n++] = a[i++];
      j++;
    }
  }
  if (i < na) {
    memcpy(out + n, a + i, (na - i) * 2);
    n += na - i;
  }
  if (j < nb) {
    memcpy(out + n, b + j, (nb - j) * 2);
    n += nb - j;
  }
  return n;
}

inline u32 combine(u8 op, const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  if (op == 'A') return intersect_lists(a, na, b, nb, out);
  if (op == 'N') return diff_lists(a, na, b, nb, out);
  return union_lists(a, na, b, nb, out);
}

struct Run {
  u16 *list, *tmp;
  u64* bits;
  const u16* cur_l;
  const u64* cur_b;
  u32 n, wl;
  bool dense;
  void start(const Opnd& x) {
    dense = (x.b != nullptr);
    n = x.n;
    cur_l = x.l;
    cur_b = x.b;
  }
  void apply(u8 op, const Opnd& x) {
    if (!x.b && !x.n) {
      if (op == 'A') {
        dense = false;
        n = 0;
      }
      return;
    }
    if (!dense) {
      if (n == 0) {
        if (op == 'O') start(x);
        return;
      }
      if (!x.b) {
        n = combine(op, cur_l, n, x.l, x.n, tmp);
        std::swap(list, tmp);
        cur_l = list;
      } else if (op != 'O') {
        u32 k = 0;
        if (op == 'A') {
          for (u32 i = 0; i < n; i++) {
            u16 d = cur_l[i];
            if (has(x.b, d)) list[k++] = d;
          }
        } else {
          for (u32 i = 0; i < n; i++) {
            u16 d = cur_l[i];
            if (!has(x.b, d)) list[k++] = d;
          }
        }
        n = k;
        cur_l = list;
      } else {
        memcpy(bits, x.b, wl * 8);
        for (u32 i = 0; i < n; i++) bits[cur_l[i] >> 6] |= 1ULL << (cur_l[i] & 63);
        cur_b = bits;
        dense = true;
      }
    } else if (!x.b) {
      if (op == 'A') {
        n = 0;
        for (u32 i = 0; i < x.n; i++) {
          u16 d = x.l[i];
          if (has(cur_b, d)) list[n++] = d;
        }
        cur_l = list;
        dense = false;
      } else {
        if (cur_b != bits) {
          memcpy(bits, cur_b, wl * 8);
          cur_b = bits;
        }
        if (op == 'O') {
          for (u32 i = 0; i < x.n; i++) bits[x.l[i] >> 6] |= 1ULL << (x.l[i] & 63);
        } else {
          for (u32 i = 0; i < x.n; i++) bits[x.l[i] >> 6] &= ~(1ULL << (x.l[i] & 63));
        }
      }
    } else {
      if (op == 'A') {
        for (u32 i = 0; i < wl; i += 4) {
          __m256i vb = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(cur_b + i));
          __m256i vx = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(x.b + i));
          _mm256_storeu_si256(reinterpret_cast<__m256i*>(bits + i), _mm256_and_si256(vb, vx));
        }
      } else if (op == 'O') {
        for (u32 i = 0; i < wl; i += 4) {
          __m256i vb = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(cur_b + i));
          __m256i vx = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(x.b + i));
          _mm256_storeu_si256(reinterpret_cast<__m256i*>(bits + i), _mm256_or_si256(vb, vx));
        }
      } else {
        for (u32 i = 0; i < wl; i += 4) {
          __m256i vb = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(cur_b + i));
          __m256i vx = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(x.b + i));
          _mm256_storeu_si256(reinterpret_cast<__m256i*>(bits + i), _mm256_andnot_si256(vx, vb));
        }
      }
      cur_b = bits;
    }
  }
  void size_sum(u64& c, u64& s) const {
    c = s = 0;
    if (!dense) {
      c = n;
      for (u32 i = 0; i < n; i++) s += cur_l[i];
      return;
    }
    static const u64 kH[6] = {0xAAAAAAAAAAAAAAAAULL, 0xCCCCCCCCCCCCCCCCULL, 0xF0F0F0F0F0F0F0F0ULL,
                              0xFF00FF00FF00FF00ULL, 0xFFFF0000FFFF0000ULL, 0xFFFFFFFF00000000ULL};
    for (u32 i = 0; i < wl; i++) {
      u64 w = cur_b[i], k = __builtin_popcountll(w), within = 0;
      for (int j = 0; j < 6; j++) within += (u64)__builtin_popcountll(w & kH[j]) << j;
      c += k;
      s += i * 64 * k + within;
    }
  }
  u32 smallest(u32* out, u32 base) const {
    u32 k = 0;
    if (!dense) {
      for (; k < n && k < 10; k++) out[k] = cur_l[k] + base;
      return k;
    }
    for (u32 i = 0; i < wl && k < 10; i++) {
      for (u64 w = cur_b[i]; w && k < 10; w &= w - 1)
        out[k++] = i * 64 + _tzcnt_u64(w) + base;
    }
    return k;
  }
};

u32* g_first;
u64* g_pack;
alignas(64) u8 g_lock[4096];

inline void report(u32 q, const Run& run, u32 base) {
  if (!run.dense && !run.n) return;
  u64 c, s;
  run.size_sum(c, s);
  if (!c) return;
  __atomic_fetch_add(&g_pack[q], c | ((s + c * base) << 20), __ATOMIC_RELAXED);
  u32* f = g_first + q * 11;
  if (__atomic_load_n(&f[10], __ATOMIC_RELAXED) == 10 &&
      __atomic_load_n(&f[9], __ATOMIC_RELAXED) <= base)
    return;
  u32 nb[10];
  u32 m = run.smallest(nb, base);
  u8* lck = &g_lock[q & 4095];
  while (__atomic_exchange_n(lck, 1, __ATOMIC_ACQUIRE)) {
    while (__atomic_load_n(lck, __ATOMIC_RELAXED)) _mm_pause();
  }
  u32 nf = f[10];
  if (!(nf == 10 && f[9] <= nb[0])) {
    u32 merged[10];
    u32 i = 0, j = 0, k = 0;
    while (k < 10 && (i < nf || j < m)) {
      if (i < nf && (j >= m || f[i] <= nb[j])) merged[k++] = f[i++];
      else merged[k++] = nb[j++];
    }
    memcpy(f, merged, k * 4);
    __atomic_store_n(&f[9], merged[k - 1], __ATOMIC_RELAXED);
    __atomic_store_n(&f[10], k, __ATOMIC_RELAXED);
  }
  __atomic_store_n(lck, 0, __ATOMIC_RELEASE);
}

u32 g_nd, g_ns, g_nb, g_nt, g_pwords;

struct Worker {
  u64* h;
  u32 *ids, *tp, *dend, *found;
  u16 *stamp, *slow;
  u32 *df, *start, *pairs;
  u16* post;
  u64 *rows, *present;
  Run run;
};

template <bool kSample>
void scan(Worker& w, u64 from, u64 to, u32 wl, u32& nfound) {
  const char* d = g_d.p;
  u32 nt = 0, nd = 0, done = 0, np = 0, nf = 0;
  u64 carry_a = 0, carry_nl = 1, in_id = 0, open = ~0ULL;
  auto flush = [&]() {
    u32 end = w.dend[nd - 1], nslow = 0;
    u32 i = 0;
    for (; i + 4 <= end; i += 4) {
      __builtin_prefetch(g_tab + line_of(w.h[i + kAhead]) * 8);
      __builtin_prefetch(g_tab + line_of(w.h[i + kAhead + 1]) * 8);
      __builtin_prefetch(g_tab + line_of(w.h[i + kAhead + 2]) * 8);
      __builtin_prefetch(g_tab + line_of(w.h[i + kAhead + 3]) * 8);

      u64 h0 = w.h[i], h1 = w.h[i + 1], h2 = w.h[i + 2], h3 = w.h[i + 3];
      const u64* l0 = g_tab + line_of(h0) * 8;
      const u64* l1 = g_tab + line_of(h1) * 8;
      const u64* l2 = g_tab + line_of(h2) * 8;
      const u64* l3 = g_tab + line_of(h3) * 8;

      const __m256i m = _mm256_set1_epi64x(kTag);
      __m256i t0 = _mm256_set1_epi64x(h0 & kTag);
      __m256i t1 = _mm256_set1_epi64x(h1 & kTag);
      __m256i t2 = _mm256_set1_epi64x(h2 & kTag);
      __m256i t3 = _mm256_set1_epi64x(h3 & kTag);

      __m256i a0 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l0)), m), t0);
      __m256i b0 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l0 + 4)), m), t0);
      __m256i a1 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l1)), m), t1);
      __m256i b1 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l1 + 4)), m), t1);
      __m256i a2 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l2)), m), t2);
      __m256i b2 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l2 + 4)), m), t2);
      __m256i a3 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l3)), m), t3);
      __m256i b3 = _mm256_cmpeq_epi64(
          _mm256_and_si256(_mm256_load_si256(reinterpret_cast<const __m256i*>(l3 + 4)), m), t3);

      u32 hit0 = _mm256_movemask_pd(_mm256_castsi256_pd(a0)) |
                 (_mm256_movemask_pd(_mm256_castsi256_pd(b0)) << 4);
      u32 hit1 = _mm256_movemask_pd(_mm256_castsi256_pd(a1)) |
                 (_mm256_movemask_pd(_mm256_castsi256_pd(b1)) << 4);
      u32 hit2 = _mm256_movemask_pd(_mm256_castsi256_pd(a2)) |
                 (_mm256_movemask_pd(_mm256_castsi256_pd(b2)) << 4);
      u32 hit3 = _mm256_movemask_pd(_mm256_castsi256_pd(a3)) |
                 (_mm256_movemask_pd(_mm256_castsi256_pd(b3)) << 4);

      u64 e0 = l0[_tzcnt_u32(hit0) & 7] & -static_cast<u64>(hit0 != 0);
      u64 e1 = l1[_tzcnt_u32(hit1) & 7] & -static_cast<u64>(hit1 != 0);
      u64 e2 = l2[_tzcnt_u32(hit2) & 7] & -static_cast<u64>(hit2 != 0);
      u64 e3 = l3[_tzcnt_u32(hit3) & 7] & -static_cast<u64>(hit3 != 0);

      w.ids[i] = static_cast<u32>(e0 & 0x3FFFF);
      w.ids[i + 1] = static_cast<u32>(e1 & 0x3FFFF);
      w.ids[i + 2] = static_cast<u32>(e2 & 0x3FFFF);
      w.ids[i + 3] = static_cast<u32>(e3 & 0x3FFFF);

      u32 s0 = (hit0 == 0) & static_cast<u32>((l0[(h0 >> 31) & 7] >> 30) & 1);
      u32 s1 = (hit1 == 0) & static_cast<u32>((l1[(h1 >> 31) & 7] >> 30) & 1);
      u32 s2 = (hit2 == 0) & static_cast<u32>((l2[(h2 >> 31) & 7] >> 30) & 1);
      u32 s3 = (hit3 == 0) & static_cast<u32>((l3[(h3 >> 31) & 7] >> 30) & 1);

      if (__builtin_expect((s0 | s1 | s2 | s3) != 0, 0)) {
        if (s0) w.slow[nslow++] = static_cast<u16>(i);
        if (s1) w.slow[nslow++] = static_cast<u16>(i + 1);
        if (s2) w.slow[nslow++] = static_cast<u16>(i + 2);
        if (s3) w.slow[nslow++] = static_cast<u16>(i + 3);
      }
    }
    for (; i < end; i++) {
      u32 slow;
      w.ids[i] = static_cast<u32>(lookup(w.h[i], slow) & 0x3FFFF);
      w.slow[nslow] = static_cast<u16>(i);
      nslow += slow;
    }
    for (u32 k = 0; k < nslow; k++) {
      u32 idx = w.slow[k];
      w.ids[idx] = static_cast<u32>(ovf_find(w.h[idx]) & 0x3FFFF);
    }
    u32 a = 0;
    for (u32 k = 0; k < nd; k++) {
      u32 b = w.dend[k], local = done + k, nh = 0;
      u16 cur = static_cast<u16>(local + 1);
      w.stamp[0] = cur;
      if constexpr (kSample) {
        for (u32 i = a; i < b; i++) {
          u32 id = w.ids[i];
          u16 prev = w.stamp[id];
          if (prev != cur) {
            w.stamp[id] = cur;
            w.df[id]++;
          }
        }
      } else {
        u64 bit = 1ULL << (local & 63);
        for (u32 i = a; i < b; i++) {
          u32 id = w.ids[i];
          u16 prev = w.stamp[id];
          if (prev != cur) {
            w.stamp[id] = cur;
            u32 sid = id - g_nd - 1;
            w.rows[(id <= g_nd ? id : 0) * wl + (local >> 6)] |= bit;
            w.pairs[np] = (sid << kDocBits) | local;
            np += (sid < g_ns);
          }
          u32 trig = g_tmap[id];
          w.tp[nh] = (trig << 14) | i;
          nh += (trig != 0);
        }
        for (u32 ti = 0; ti < nh; ti++) {
          u32 tp = w.tp[ti], i = tp & 0x3FFF, trig = tp >> 14;
          for (u32 x = g_tstart[trig]; x < g_tstart[trig + 1]; x++) {
            u64 e = g_trig[x];
            u32 best = (e >> 16) & 7, qn = (e >> 19) & 15, from_i = i - best;
            if (from_i < a || from_i > b || b - from_i < qn) continue;
            u32 o1 = (e >> 23) & 7, t1 = (e >> 26) & 0x1FFFF;
            if (w.ids[from_i + o1] != t1) continue;
            u32 o2 = (e >> 43) & 7, t2 = (e >> 46) & 0x1FFFF;
            if (w.ids[from_i + o2] != t2) continue;
            u32 qid = static_cast<u32>(e & 0xFFFF);
            if (qn > 3) {
              const Query& q = g_queries[qid];
              u32 j = 0;
              while (j < qn && w.ids[from_i + j] == q.t[j]) j++;
              if (j != qn) continue;
            }
            w.found[nf++] = (qid << kDocBits) | local;
          }
        }
      }
      a = b;
    }
    memmove(w.h, w.h + end, (nt - end) * 8);
    nt -= end;
    done += nd;
    nd = 0;
  };
  auto emit = [&](const char* q, u32 len) {
    if (__builtin_expect(len <= 8, 1)) {
      u64 w0 = _bzhi_u64(load64(q) | (kOnes * 0x20), len << 3);
      w.h[nt++] = fold(w0 ^ K0, K1) | kTop;
    } else if (__builtin_expect(len <= 16, 1)) {
      u64 w0 = load64(q) | (kOnes * 0x20);
      u64 w1 = _bzhi_u64(load64(q + 8) | (kOnes * 0x20), (len - 8) << 3);
      w.h[nt++] = hash16(w0, w1);
    } else {
      w.h[nt++] = hash_bytes(q, len, true);
    }
  };
  const __m256i k20 = _mm256_set1_epi8(0x20), k05 = _mm256_set1_epi8(5);
  const __m256i k101 = _mm256_set1_epi8(101), k70 = _mm256_set1_epi8(70);
  const __m256i k117 = _mm256_set1_epi8(117), k0A = _mm256_set1_epi8(10);
  for (u64 at = from; at < to; at += 64) {
    __m256i v0 = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(d + at));
    __m256i v1 = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(d + at + 32));
    __m256i lo0 = _mm256_or_si256(v0, k20);
    __m256i lo1 = _mm256_or_si256(v1, k20);
    __m256i letter0 = _mm256_cmpgt_epi8(_mm256_add_epi8(lo0, k05), k101);
    __m256i letter1 = _mm256_cmpgt_epi8(_mm256_add_epi8(lo1, k05), k101);
    __m256i digit0 = _mm256_cmpgt_epi8(_mm256_add_epi8(v0, k70), k117);
    __m256i digit1 = _mm256_cmpgt_epi8(_mm256_add_epi8(v1, k70), k117);
    u32 a0 = _mm256_movemask_epi8(_mm256_or_si256(letter0, digit0));
    u32 a1 = _mm256_movemask_epi8(_mm256_or_si256(letter1, digit1));
    u32 n0 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, k0A));
    u32 n1 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, k0A));
    u64 alnum = a0 | (static_cast<u64>(a1) << 32);
    u64 nl = n0 | (static_cast<u64>(n1) << 32);

    if (to - at < 64) {
      u64 valid = (1ULL << (to - at)) - 1;
      alnum &= valid;
      nl &= valid;
    }
    if (__builtin_expect((nl | carry_nl | in_id) != 0, 0)) {
      u32 t0 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, _mm256_set1_epi8(9)));
      u32 t1 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, _mm256_set1_epi8(9)));
      u64 tab = t0 | (static_cast<u64>(t1) << 32);
      if (to - at < 64) tab &= (1ULL << (to - at)) - 1;
      u64 toggle = ((nl << 1) | carry_nl) | tab;
      u64 idm = _mm_cvtsi128_si64(
          _mm_clmulepi64_si128(_mm_set_epi64x(0, toggle), _mm_set1_epi8(-1), 0));
      idm ^= -in_id;
      in_id = idm >> 63;
      carry_nl = nl >> 63;
      alnum &= ~idm;
    }
    u64 prev = (alnum << 1) | carry_a;
    u64 starts = alnum & ~prev, ends = ~alnum & prev;
    carry_a = alnum >> 63;
    u32 nt0 = nt;
    u64 e = ends, s = starts;
    if (open != ~0ULL && e) {
      u32 e_idx = _tzcnt_u64(e);
      emit(d + open, static_cast<u32>(at + e_idx - open));
      e &= e - 1;
      open = ~0ULL;
    }
    const char* block = d + at;
    while (e) {
      u32 s_idx = _tzcnt_u64(s);
      u32 e_idx = _tzcnt_u64(e);
      s &= s - 1;
      e &= e - 1;
      emit(block + s_idx, e_idx - s_idx);
    }
    if (s) open = at + _tzcnt_u64(s);
    if (__builtin_expect(nl != 0, 0)) {
      for (u64 n = nl; n; n &= n - 1) {
        u32 pos = _tzcnt_u64(n);
        w.dend[nd++] = nt0 + __builtin_popcountll(ends & ((2ULL << pos) - 1));
      }
    }
    if (nt >= kFlush || nd > 100) {
      if (nd) flush();
    }
  }
  if (nd) flush();
  nfound = nf;
  if constexpr (!kSample) {
    u32* start = w.start;
    memset(start, 0, (g_ns + 1) * 4);
    for (u32 i = 0; i < np; i++) start[w.pairs[i] >> kDocBits]++;
    u32 acc = 0;
    for (u32 pw = 0; pw < g_pwords; pw++) {
      u64 mask = 0;
      u32 bi = pw * 64, lim = std::min<u32>(bi + 64, g_ns);
      for (u32 i = bi; i < lim; i++) {
        u32 c = start[i];
        mask |= static_cast<u64>(c != 0) << (i & 63);
        acc += c;
        start[i] = acc;
      }
      w.present[pw] = mask;
    }
    start[g_ns] = acc;
    for (u32 i = np; i-- > 0;)
      w.post[--start[w.pairs[i] >> kDocBits]] =
          w.pairs[i] & ((1 << kDocBits) - 1);
  }
}

std::barrier<> g_bar(kThreads);
Worker* g_workers[kThreads];
u32* g_dfall;
u32* g_fin;
std::atomic<u32> g_next{0};
u64 g_osz[kThreads];
int g_fd;

struct Digits {
  char d[200];
  constexpr Digits() : d() {
    for (int i = 0; i < 100; i++) {
      d[i * 2] = '0' + i / 10;
      d[i * 2 + 1] = '0' + i % 10;
    }
  }
} constexpr kDigits;

inline u8* put(u8* o, u64 x) {
  u8 buf[24];
  int i = 24;
  while (x >= 100) {
    u64 r = (x % 100) * 2;
    x /= 100;
    i -= 2;
    memcpy(buf + i, kDigits.d + r, 2);
  }
  if (x < 10) {
    buf[--i] = static_cast<u8>('0' + x);
  } else {
    i -= 2;
    memcpy(buf + i, kDigits.d + x * 2, 2);
  }
  memcpy(o, buf + i, 24 - i);
  return o + 24 - i;
}

u32 first_doc(u64 at) {
  u32 x = 0;
  for (const char* p = g_d.p + at; *p >= '0' && *p <= '9'; p++) x = x * 10 + (*p - '0');
  return x;
}

void sort_by_df(u32* arr, u32 n) {
  u32 df[8];
  for (u32 i = 0; i < n; i++) df[i] = g_dfall[arr[i]];
  for (u32 i = 1; i < n; i++) {
    u32 kt = arr[i], kd = df[i];
    int j = static_cast<int>(i) - 1;
    while (j >= 0 && df[j] > kd) {
      arr[j + 1] = arr[j];
      df[j + 1] = df[j];
      j--;
    }
    arr[j + 1] = kt;
    df[j + 1] = kd;
  }
}

void worker(int tid) {
  u32 max_per_thread = (g_q.n / 25 + kThreads) / kThreads + 512;
  g_piece[tid] = arena_alloc<Query>(max_per_thread);
  parse(tid, g_piece[tid], g_psize[tid]);
  g_bar.arrive_and_wait();
  if (tid == 0) {
    u32 nq = 0;
    for (int t = 0; t < kThreads; t++) nq += g_psize[t];
    g_nq = nq;
  }
  g_bar.arrive_and_wait();

  Worker w = {};
  w.df = arena_alloc<u32>(kMaxProv + 1, true);
  w.stamp = arena_alloc<u16>(kMaxProv + 1, true);
  w.h = arena_alloc<u64>(kCap + 64);
  w.ids = arena_alloc<u32>(kCap + 64);
  w.slow = arena_alloc<u16>(kCap + 64);
  w.dend = arena_alloc<u32>(256);
  g_workers[tid] = &w;

  u64 sa = cut(g_d, tid, 96 * kSamples), sb = cut(g_d, tid + 1, 96 * kSamples);
  u32 dummy_nf = 0;
  if (sa < sb) scan<true>(w, sa, sb, 0, dummy_nf);
  g_bar.arrive_and_wait();

  {
    size_t per = (kMaxProv + 1 + kThreads) / kThreads;
    size_t hi = std::min<size_t>(per * (tid + 1), kMaxProv + 1);
    for (size_t i = per * tid; i < hi; i++) {
      u32 sum = 0;
      for (int t = 0; t < kThreads; t++) sum += g_workers[t]->df[i];
      g_dfall[i] = sum;
    }
  }
  g_bar.arrive_and_wait();

  if (tid == 0) {
    u64 end = cut(g_d, kSamples, 96 * kSamples);
    u32 seen = (end >= g_d.n) ? g_ndocs : first_doc(end);
    auto dense = [&](u32 p) {
      return (g_used[p] & 1) && static_cast<u64>(g_dfall[p]) * 32 >= seen;
    };
    u32 nd = 0, ns = 0, np_only = 0;
    for (u32 p = 1; p <= kMaxProv; p++) {
      if (g_used[p]) {
        if (dense(p)) nd++;
        else if (g_used[p] & 1) ns++;
        else np_only++;
      }
    }
    g_nd = nd;
    g_ns = ns;
    g_nb = nd + ns;
    g_nt = nd + ns + np_only;
    g_pwords = (ns + 63) / 64;
    u32 a = 0, b = 0, c = 0;
    for (u32 p = 1; p <= kMaxProv; p++) {
      if (g_used[p]) {
        g_fin[p] = dense(p) ? (1 + a++)
                            : ((g_used[p] & 1) ? (nd + 1 + b++)
                                               : (nd + ns + 1 + c++));
      }
    }
  }
  g_bar.arrive_and_wait();

  {
    u32 o = 0;
    for (int t = 0; t < tid; t++) o += g_psize[t];
    Query* pc = g_piece[tid];
    u32 cnt = g_psize[tid];
    for (u32 i = 0; i < cnt; i++) {
      Query& qq = pc[i];
      qq.qid = o + i;
      if (!qq.phrase) {
        u32 fo = 0;
        while (fo + 1 < qq.n && qq.ops[fo] != 'O') fo++;
        if (fo > 0) {
          u32 pos[8], neg[8], np = 0, nn = 0;
          pos[np++] = qq.t[0];
          for (u32 j = 0; j < fo; j++) {
            if (qq.ops[j] == 'A') pos[np++] = qq.t[j + 1];
            else neg[nn++] = qq.t[j + 1];
          }
          sort_by_df(pos, np);
          qq.t[0] = pos[0];
          u32 idx = 0;
          for (u32 j = 1; j < np; j++) {
            qq.ops[idx] = 'A';
            qq.t[++idx] = pos[j];
          }
          for (u32 j = 0; j < nn; j++) {
            qq.ops[idx] = 'N';
            qq.t[++idx] = neg[j];
          }
        }
        qq.must = 0;
        u32 best_m = 0;
        if (!qq.has_or) {
          best_m = qq.t[0];
          for (u32 j = 1; j < qq.n; j++) {
            if (qq.ops[j - 1] == 'A') {
              u32 cand = qq.t[j];
              if (g_dfall[cand] < g_dfall[best_m]) best_m = cand;
            }
          }
        } else {
          int lo = static_cast<int>(qq.n) - 2;
          while (lo >= 0 && qq.ops[lo] != 'O') lo--;
          for (u32 j = static_cast<u32>(lo + 1); j + 1 < qq.n; j++) {
            if (qq.ops[j] == 'A') {
              u32 cand = qq.t[j + 1];
              if (!best_m || g_dfall[cand] < g_dfall[best_m]) best_m = cand;
            }
          }
          if (best_m) qq.must = g_fin[best_m];
        }
        if (best_m && g_fin[best_m] > g_nd) {
          qq.gen[0] = g_fin[best_m];
          qq.ng = 1;
        } else if (!qq.has_or) {
          qq.gen[0] = 0;
          qq.ng = 1;
        } else {
          u32 cstarts[8], nc = 0;
          cstarts[nc++] = 0;
          for (u32 j = 0; j + 1 < qq.n; j++) {
            if (qq.ops[j] == 'O') cstarts[nc++] = j + 1;
          }
          bool all_sparse = (nc <= 4);
          u32 gens[4], ng = 0;
          for (u32 c = 0; c < nc && all_sparse; c++) {
            u32 st = cstarts[c];
            u32 best_c = qq.t[st];
            for (u32 k = st; k + 1 < qq.n; k++) {
              if (qq.ops[k] == 'A' && g_dfall[qq.t[k + 1]] < g_dfall[best_c])
                best_c = qq.t[k + 1];
            }
            u32 fc = g_fin[best_c];
            if (fc <= g_nd) {
              all_sparse = false;
              break;
            }
            bool dup = false;
            for (u32 z = 0; z < ng; z++) {
              if (gens[z] == fc) dup = true;
            }
            if (!dup) gens[ng++] = fc;
          }
          if (all_sparse && ng > 0) {
            for (u32 z = 0; z < ng; z++) qq.gen[z] = gens[z];
            qq.ng = static_cast<u8>(ng);
          } else {
            qq.gen[0] = 0;
            qq.ng = 1;
          }
        }
        for (u32 j = 0; j < qq.n; j++) qq.t[j] = g_fin[qq.t[j]];
      }
    }
  }
  g_bar.arrive_and_wait();

  if (tid == 0) {
    u32* tdeg = arena_alloc<u32>(g_nt + 2, true);
    u32 n_qlist = 0, n_trig = 0, q_cursor = 0;
    for (int t = 0; t < kThreads; t++) {
      Query* pc = g_piece[t];
      u32 cnt = g_psize[t];
      for (u32 i = 0; i < cnt; i++) {
        Query& qq = pc[i];
        if (!qq.phrase) {
          n_qlist += qq.ng;
        } else {
          u32 best = 0;
          for (u32 j = 1; j < qq.n; j++) {
            if (g_dfall[qq.t[j]] < g_dfall[qq.t[best]]) best = j;
          }
          tdeg[g_fin[qq.t[best]]]++;
          n_trig++;
        }
      }
    }
    u32 n_tterms = 0;
    for (u32 f = 1; f <= g_nt; f++) {
      if (tdeg[f]) n_tterms++;
    }
    g_aoff.store(0, std::memory_order_relaxed);
    g_queries = arena_alloc<Query>(g_nq);
    g_first = arena_alloc<u32>(g_nq * 11, true);
    g_pack = arena_alloc<u64>(g_nq, true);
    g_qstart = arena_alloc<u32>(g_nb + 2, true);
    g_tstart = arena_alloc<u32>(n_tterms + 2, true);
    g_qlist = arena_alloc<u32>(n_qlist + 4);
    g_trig = arena_alloc<u64>(n_trig + 4);
    g_tmap = arena_alloc<u16>(g_nt + 2, true);
    g_has_q = arena_alloc<u64>(g_pwords + 2, true);
    size_t keep_watermark = g_aoff.load(std::memory_order_relaxed);
    for (int t = 0; t < kThreads; t++) {
      Query* pc = g_piece[t];
      u32 cnt = g_psize[t];
      for (u32 i = 0; i < cnt; i++) {
        Query& qq = pc[i];
        if (!qq.phrase) {
          for (u32 z = 0; z < qq.ng; z++) g_qstart[qq.gen[z] + 1]++;
        }
      }
    }
    u32 cur_t = 0;
    for (u32 f = 1; f <= g_nt; f++) {
      if (tdeg[f]) {
        g_tmap[f] = static_cast<u16>(++cur_t);
        g_tstart[cur_t + 1] = tdeg[f];
      }
    }
    for (u32 i = 0; i <= g_nb; i++) g_qstart[i + 1] += g_qstart[i];
    for (u32 i = 0; i <= n_tterms; i++) g_tstart[i + 1] += g_tstart[i];
    for (u32 sid = 0; sid < g_ns; sid++) {
      u32 term = g_nd + 1 + sid;
      if (g_qstart[term + 1] > g_qstart[term]) {
        g_has_q[sid >> 6] |= 1ULL << (sid & 63);
      }
    }
    u32* qfill = arena_alloc<u32>(g_nb + 2);
    u32* tfill = arena_alloc<u32>(n_tterms + 2);
    memcpy(qfill, g_qstart, (g_nb + 2) * 4);
    memcpy(tfill, g_tstart, (n_tterms + 2) * 4);

    for (int t = 0; t < kThreads; t++) {
      Query* pc = g_piece[t];
      u32 cnt = g_psize[t];
      for (u32 i = 0; i < cnt; i++) {
        Query& qq = pc[i];
        u32 qid = q_cursor++;
        if (!qq.phrase) {
          g_queries[qid] = qq;
          for (u32 z = 0; z < qq.ng; z++) g_qlist[qfill[qq.gen[z]]++] = (qid << 2) | z;
        } else {
          u32 best = 0;
          for (u32 j = 1; j < qq.n; j++) {
            if (g_dfall[qq.t[j]] < g_dfall[qq.t[best]]) best = j;
          }
          u32 o1 = best, o2 = best;
          for (u32 j = 0; j < qq.n; j++) {
            if (j != best && (o1 == best || g_dfall[qq.t[j]] < g_dfall[qq.t[o1]])) o1 = j;
          }
          for (u32 j = 0; j < qq.n; j++) {
            if (j != best && j != o1 &&
                (o2 == best || g_dfall[qq.t[j]] < g_dfall[qq.t[o2]]))
              o2 = j;
          }
          for (u32 j = 0; j < qq.n; j++) qq.t[j] = g_fin[qq.t[j]];
          g_queries[qid] = qq;
          u64 pack = static_cast<u64>(qid) | (static_cast<u64>(best) << 16) |
                     (static_cast<u64>(qq.n) << 19) |
                     (static_cast<u64>(o1) << 23) |
                     (static_cast<u64>(qq.t[o1]) << 26) |
                     (static_cast<u64>(o2) << 43) |
                     (static_cast<u64>(qq.t[o2]) << 46);
          g_trig[tfill[g_tmap[qq.t[best]]]++] = pack;
        }
      }
    }

    // Rewrite 8-slot table slots with final term IDs
    for (u32 l = 0; l < kLines; l++) {
      for (int s = 0; s < 8; s++) {
        u64& e = g_tab[l * 8 + s];
        if (e) {
          u32 prov = static_cast<u32>(e & 0x3FFFF);
          e = (e & (kTag | kOvf)) | g_fin[prov];
        }
      }
    }
    for (u32 i = 0; i < kOvfSize; i++) {
      u64& e = g_ovf[i];
      if (e) {
        u32 prov = static_cast<u32>(e & 0x3FFFF);
        e = (e & kTag) | g_fin[prov];
      }
    }
    g_aoff.store(keep_watermark, std::memory_order_relaxed);
  }
  g_bar.arrive_and_wait();

  u32 max_wl = (g_ndocs / kChunks + 128) / 64 + 4;
  w.h = arena_alloc<u64>(kCap + 64);
  w.ids = arena_alloc<u32>(kCap + 64);
  w.tp = arena_alloc<u32>(kCap + 64);
  w.slow = arena_alloc<u16>(kCap + 64);
  w.dend = arena_alloc<u32>(512);
  w.found = arena_alloc<u32>(kMaxFound);
  w.stamp = arena_alloc<u16>(g_nt + 2);
  w.start = arena_alloc<u32>(g_ns + 2);
  w.present = arena_alloc<u64>(g_pwords + 2);
  w.rows = arena_alloc<u64>(static_cast<size_t>(g_nd + 1) * max_wl);
  Run& run = w.run;
  run.list = arena_alloc<u16>((1 << kDocBits) + 64);
  run.tmp = arena_alloc<u16>((1 << kDocBits) + 64);
  run.bits = arena_alloc<u64>((1 << kDocBits) / 64 + 8);
  g_bar.arrive_and_wait();
  w.pairs = arena_alloc<u32>(kMaxPairs);
  w.post = arena_alloc<u16>(kMaxPairs);

  for (;;) {
    int c = g_next.fetch_add(1, std::memory_order_relaxed);
    if (c >= kChunks) break;
    u64 from = cut(g_d, c, kChunks), to = cut(g_d, c + 1, kChunks);
    if (from >= to) continue;
    u32 base = first_doc(from);
    u32 end = (to == g_d.n) ? g_ndocs : first_doc(to);
    u32 wl = (end - base + 63) / 64;
    memset(w.rows, 0, static_cast<size_t>(g_nd + 1) * wl * 8);
    memset(w.stamp, 0, (g_nt + 1) * 2);
    run.wl = wl;
    u32 nf = 0;
    scan<false>(w, from, to, wl, nf);
    if (nf > 0) {
      std::sort(w.found, w.found + nf);
      nf = static_cast<u32>(std::unique(w.found, w.found + nf) - w.found);
      for (u32 i = 0; i < nf;) {
        u32 q = w.found[i] >> kDocBits;
        u32 start_i = i;
        while (i < nf && (w.found[i] >> kDocBits) == q) {
          run.list[i - start_i] = w.found[i] & ((1 << kDocBits) - 1);
          i++;
        }
        run.dense = false;
        run.n = i - start_i;
        run.cur_l = run.list;
        report(q, run, base);
      }
    }
    const u32* start = w.start;
    const u64* pres = w.present;
    auto has_term = [&](u32 term) -> bool {
      if (term <= g_nd) return true;
      u32 sid = term - g_nd - 1;
      return (pres[sid >> 6] >> (sid & 63)) & 1;
    };
    auto operand = [&](u32 term) {
      if (term <= g_nd) return Opnd{nullptr, w.rows + static_cast<size_t>(term) * wl, 0};
      u32 sid = term - g_nd - 1;
      return Opnd{w.post + start[sid], nullptr, start[sid + 1] - start[sid]};
    };
    auto eval_slice = [&](u32 qi_a, u32 qi_b) {
      for (u32 qi = qi_a; qi < qi_b; qi++) {
        u32 entry = g_qlist[qi];
        u32 q = entry >> 2, r = entry & 3;
        const Query& qq = g_queries[q];
        if (r > 0) {
          bool earlier = false;
          for (u32 z = 0; z < r; z++) {
            if (has_term(qq.gen[z])) {
              earlier = true;
              break;
            }
          }
          if (earlier) continue;
        }
        if (!qq.has_or) {
          bool any_missing = false;
          for (u32 j = 0; j < qq.n; j++) {
            if (j > 0 && qq.ops[j - 1] != 'A') break;
            if (!has_term(qq.t[j])) { any_missing = true; break; }
          }
          if (any_missing) continue;
        } else if (qq.must && !has_term(qq.must)) {
          continue;
        }
        Opnd x = operand(qq.t[0]);
        if (!x.b && !x.n && !qq.has_or) continue;
        run.start(x);
        for (u32 k = 0; k + 1 < qq.n; k++) {
          u8 op = qq.ops[k];
          if (!run.dense && !run.n) {
            if (!qq.has_or) goto next_query;
            if (op != 'O') continue;
          }
          run.apply(op, operand(qq.t[k + 1]));
        }
        report(q, run, base);
      next_query:;
      }
    };
    eval_slice(g_qstart[0], g_qstart[g_nd + 1]);
    for (u32 pw = 0; pw < g_pwords; pw++) {
      for (u64 mask = pres[pw] & g_has_q[pw]; mask; mask &= mask - 1) {
        u32 sid = pw * 64 + _tzcnt_u64(mask);
        u32 term = g_nd + 1 + sid;
        eval_slice(g_qstart[term], g_qstart[term + 1]);
      }
    }
  }
  g_bar.arrive_and_wait();

  u32 q0 = static_cast<u64>(g_nq) * tid / kThreads;
  u32 q1 = static_cast<u64>(g_nq) * (tid + 1) / kThreads;
  u8* buf = reinterpret_cast<u8*>(w.pairs);
  u8* o = buf;
  for (u32 q = q0; q < q1; q++) {
    u64 packed = g_pack[q];
    o = put(o, packed & 0xFFFFFULL);
    *o++ = ' ';
    o = put(o, packed >> 20);
    const u32* f = g_first + q * 11;
    for (u32 i = 0; i < f[10]; i++) {
      *o++ = ' ';
      o = put(o, f[i]);
    }
    *o++ = '\n';
  }
  g_osz[tid] = o - buf;
  if (tid == 0) g_fd = open("/out/results.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  g_bar.arrive_and_wait();
  off_t at = 0;
  for (int x = 0; x < tid; x++) at += g_osz[x];
  for (size_t done = 0; done < g_osz[tid];) {
    ssize_t n = pwrite(g_fd, buf + done, g_osz[tid] - done, at + done);
    if (n <= 0) break;
    done += n;
  }
  g_bar.arrive_and_wait();
  if (tid == 0) _exit(0);
}
}  // namespace

int main() {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
  g_arena = static_cast<u8*>(mmap(nullptr, 64ULL << 20, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
  g_aoff.store(8ULL << 20, std::memory_order_relaxed);
  g_q = map_file("/data/queries.txt");
  g_d = map_file("/data/docs.txt");
  const char* last =
      static_cast<const char*>(memrchr(g_d.p, '\n', g_d.n - 1));
  g_ndocs = first_doc(last ? last + 1 - g_d.p : 0) + 1;
  g_used = arena_alloc<u8>(kMaxProv + 1, true);
  g_dfall = arena_alloc<u32>(kMaxProv + 1, true);
  g_fin = arena_alloc<u32>(kMaxProv + 1, true);
  std::thread th[kThreads - 1];
  for (int i = 0; i < kThreads - 1; i++) th[i] = std::thread(worker, i + 1);
  worker(0);
  for (auto& t : th) t.join();
}
