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
#include <cstdlib>
#include <cstring>
#include <memory>
#include <thread>
#include <vector>

namespace {

using u8 = uint8_t;
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;

constexpr int kThreads = 12, kWaves = 2, kChunks = 24, kSamples = 12;
constexpr int kDocBits = 15;

constexpr u32 kCap = 1 << 14, kFlush = 384, kAhead = 16, kHalf = 12288;
constexpr u32 kBuckets = kHalf * 2, kProvMask = (1 << 17) - 1;
constexpr u32 kMaxProv = 1 << 17, kBatch = 512;

constexpr u64 kTag = ~0x1FFFFULL, kPay = 0x1FFFFULL, kTop = 1ULL << 17;
constexpr u64 kOnes = 0x0101010101010101ULL;

struct CharTable {
  u8 low[256];
  constexpr CharTable() : low() {
    for (int c = '0'; c <= '9'; c++) low[c] = c;
    for (int c = 'a'; c <= 'z'; c++) low[c] = low[c - 32] = c;
  }
} constexpr kChar;

struct File {
  const char* p;
  size_t n;
};

File map_file(const char* path) {
  int fd = open(path, O_RDONLY);
  struct stat st;
  fstat(fd, &st);
  size_t n = st.st_size;
  char* r = (char*)mmap(nullptr, n + 8192, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (n > 0)
    mmap(r, n, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0);
  close(fd);
  return {r, n};
}

File g_q, g_d;
u32 g_ndocs;

template <class T>
T* alloc(size_t n) { return (T*)calloc(n ? n : 1, sizeof(T)); }

inline u64 load64(const char* p) { u64 x; memcpy(&x, p, 8); return x; }

inline u64 fold(u64 a, u64 b) {
  unsigned __int128 m = (unsigned __int128)a * b;
  return (u64)m ^ (u64)(m >> 64);
}

constexpr u64 K0 = 0xA0761D6478BD642FULL, K1 = 0xE7037ED1A0B428DBULL;
constexpr u64 K2 = 0x8EBC6AF09C88C6E3ULL;
inline u64 hash16(u64 w0, u64 w1) { return fold(w0 ^ K0, w1 ^ K1) | kTop; }

u64 hash_bytes(const char* s, size_t len, bool lower) {
  char b[32] = {};
  size_t clen = std::min<size_t>(len, 32);
  memcpy(b, s, clen);
  if (lower) {
    for (size_t i = 0; i < clen; i++)
      b[i] |= 0x20;
  }
  u64 w[4];
  memcpy(w, b, 32);
  u64 h = fold(w[0] ^ K0, w[1] ^ K1);
  if (len > 16)
    h = fold(h ^ w[2], w[3] ^ K2);
  if (len > 32)
    h = fold(h, len * K2);
  return h | kTop;
}

// 768 KB L2-resident 2-choice 4-way AVX2 Cuckoo hash table (24,576 x 32B).
alignas(64) u64 g_tab[kBuckets * 4];
u64* g_prov;
u8* g_used;

inline u32 b1_of(u64 h) { return ((u64)(u32)(h >> 17) * kHalf) >> 32; }
inline u32 b2_of(u64 h) { return kHalf + (((h >> 32) * kHalf) >> 32); }

inline u32 lookup(u64 h) {
  u32 b1 = b1_of(h), b2 = b2_of(h);
  __m256i v_sig = _mm256_set1_epi64x(h & kTag), v_mask = _mm256_set1_epi64x(kTag);
  __m256i vec1 = _mm256_load_si256((const __m256i*)(g_tab + b1 * 4));
  __m256i vec2 = _mm256_load_si256((const __m256i*)(g_tab + b2 * 4));
  __m256i cmp1 = _mm256_cmpeq_epi64(_mm256_and_si256(vec1, v_mask), v_sig);
  __m256i cmp2 = _mm256_cmpeq_epi64(_mm256_and_si256(vec2, v_mask), v_sig);
  __m256i hit = _mm256_or_si256(_mm256_and_si256(cmp1, vec1),
                                _mm256_and_si256(cmp2, vec2));
  __m128i lohi = _mm_or_si128(_mm256_castsi256_si128(hit),
                              _mm256_extracti128_si256(hit, 1));
  u64 res = _mm_cvtsi128_si64(_mm_or_si128(lohi, _mm_bsrli_si128(lohi, 8)));
  return static_cast<u32>(res & kPay);
}

u32 intern(u64 h) {
  for (u32 i = (u32)(h >> 18) & kProvMask;; i = (i + 1) & kProvMask) {
    u64 e = __atomic_load_n(&g_prov[i], __ATOMIC_RELAXED);
    if (!e) {
      u32 id = i + 1;
      u64 want = (h & kTag) | id;
      if (__atomic_compare_exchange_n(&g_prov[i], &e, want, false, __ATOMIC_RELAXED, __ATOMIC_RELAXED))
        return id;
    }
    if (((e ^ h) & kTag) == 0)
      return static_cast<u32>(e & kPay);
  }
}

void cuckoo_insert(u64 entry) {
  u32 b1 = b1_of(entry), b2 = b2_of(entry);
  int c1 = 0, c2 = 0;
  for (int s = 0; s < 4; s++) {
    c1 += (g_tab[b1 * 4 + s] != 0);
    c2 += (g_tab[b2 * 4 + s] != 0);
  }
  if (c1 <= c2 && c1 < 4) { g_tab[b1 * 4 + c1] = entry; return; }
  if (c2 < 4) { g_tab[b2 * 4 + c2] = entry; return; }
  u32 cur_b = (entry & 1) ? b1 : b2;
  u64 cur_e = entry;
  for (u32 step = 1; step <= 512; step++) {
    u32 s = (step * 3 + static_cast<u32>(cur_e)) & 3;
    u64 evicted = g_tab[cur_b * 4 + s];
    g_tab[cur_b * 4 + s] = cur_e;
    u32 eb1 = b1_of(evicted), eb2 = b2_of(evicted);
    cur_b = (cur_b == eb1) ? eb2 : eb1;
    cur_e = evicted;
    for (int j = 0; j < 4; j++) {
      if (!g_tab[cur_b * 4 + j]) {
        g_tab[cur_b * 4 + j] = cur_e;
        return;
      }
    }
  }
}

struct Query {
  u32 t[8];
  u32 must;
  u32 qoff;
  u8 ops[8];
  u8 tlen[8];
  u8 n, ut, best, phrase, has_or;
};

std::unique_ptr<Query[]> g_queries;
u32 g_nq;
std::vector<Query> g_piece[kThreads];
size_t g_psize[kThreads];

u64 cut(const File& f, int i, int n) {
  if (i == 0 || f.n == 0)
    return 0;
  if (i == n)
    return f.n;
  u64 at = f.n * i / n;
  if (at > 0)
    at--;
  return (const char*)memchr(f.p + at, '\n', f.n - at) - f.p + 1;
}

void parse(int tid, std::vector<Query>& out) {
  const char* p = g_q.p + cut(g_q, tid, kThreads);
  const char* end = g_q.p + cut(g_q, tid + 1, kThreads);
  while (p < end) {
    const char* e = (const char*)memchr(p, '\n', end - p);
    Query q = {};
    q.phrase = (p[0] == 'P');
    p += q.phrase ? 8 : 5;
    q.qoff = static_cast<u32>(p - g_q.p);
    for (;;) {
      const char* s = p;
      while (p < e && *p != ' ' && *p != '"')
        p++;
      u32 len = p - s, id = q.t[q.n] = intern(hash_bytes(s, len, false));
      q.tlen[q.n++] = len;
      if (!g_used[id])
        __atomic_store_n(&g_used[id], 1, __ATOMIC_RELAXED);
      if (p >= e || *p == '"')
        break;
      p++;
      if (!q.phrase) {
        q.ops[q.n - 1] = static_cast<u8>(*p);
        q.has_or |= (*p == 'O');
        while (*p != ' ')
          p++;
        p++;
      }
    }
    out.push_back(q);
    p = e + 1;
  }
}

bool verify_phrase(const Query& q, const char* lstart, const char* doc_end) {
  const char* text = lstart;
  while (text < doc_end && *text != '\t')
    text++;
  if (text < doc_end)
    text++;
  const char* toks[8];
  const char* qp = g_q.p + q.qoff;
  for (u32 j = 0; j < q.n; j++) {
    toks[j] = qp;
    qp += q.tlen[j] + 1;
  }
  u32 best = q.best, alen = q.tlen[best];
  const char* anchor = toks[best];
  if (static_cast<u32>(doc_end - text) < alen)
    return false;
  const u8* low = kChar.low;
  const __m256i vf = _mm256_set1_epi8(anchor[0] | 0x20);
  const __m256i vl = _mm256_set1_epi8(anchor[alen - 1] | 0x20);
  const __m256i k20 = _mm256_set1_epi8(0x20);
  const char* limit = doc_end - alen + 1;
  for (const char* cur = text; cur < limit; cur += 32) {
    __m256i v0 = _mm256_or_si256(_mm256_loadu_si256((const __m256i*)cur), k20);
    __m256i v1 = _mm256_or_si256(
        _mm256_loadu_si256((const __m256i*)(cur + alen - 1)), k20);
    u32 hits = _mm256_movemask_epi8(
        _mm256_and_si256(_mm256_cmpeq_epi8(v0, vf), _mm256_cmpeq_epi8(v1, vl)));
    if (cur + 32 > limit) hits &= (1U << (limit - cur)) - 1;
    while (hits) {
      const char* pos = cur + _tzcnt_u32(hits);
      hits &= hits - 1;
      if ((pos > text && low[(u8)pos[-1]]) || (pos + alen < doc_end && low[(u8)pos[alen]]))
        continue;
      u32 k = 0;
      while (k < alen && low[(u8)pos[k]] == (u8)(anchor[k] | 0x20)) k++;
      if (k != alen) continue;
      const char* fwd = pos + alen;
      bool ok = true;
      for (u32 j = best + 1; j < q.n; j++) {
        while (fwd < doc_end && !low[(u8)*fwd]) fwd++;
        u32 tl = q.tlen[j];
        const char* tp = toks[j];
        if (static_cast<u32>(doc_end - fwd) < tl ||
            (fwd + tl < doc_end && low[static_cast<u8>(fwd[tl])])) {
          ok = false; break;
        }
        u32 m = 0;
        while (m < tl &&
               low[static_cast<u8>(fwd[m])] == static_cast<u8>(tp[m] | 0x20)) {
          m++;
        }
        if (m != tl) {
          ok = false; break;
        }
        fwd += tl;
      }
      if (!ok) continue;
      const char* bwd = pos;
      for (int j = static_cast<int>(best) - 1; j >= 0; j--) {
        while (bwd > text && !low[(u8)bwd[-1]]) bwd--;
        u32 tl = q.tlen[j];
        const char* tp = toks[j];
        if (static_cast<u32>(bwd - text) < tl ||
            (bwd - tl > text &&
             low[static_cast<u8>(bwd[-static_cast<int>(tl) - 1])])) {
          ok = false; break;
        }
        const char* st = bwd - tl;
        u32 m = 0;
        while (m < tl &&
               low[static_cast<u8>(st[m])] == static_cast<u8>(tp[m] | 0x20)) {
          m++;
        }
        if (m != tl) {
          ok = false; break;
        }
        bwd = st;
      }
      if (ok) return true;
    }
  }
  return false;
}

struct Opnd {
  const u16* l;
  const u64* b;
  u32 n;
};

inline bool has(const u64* b, u32 d) { return (b[d >> 6] >> (d & 63)) & 1; }

u32 gallop(u8 op, const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  if (op == 'A' && na > nb)
    std::swap(a, b), std::swap(na, nb);
  u32 n = 0, j = 0;
  for (u32 i = 0; i < na; i++) {
    u16 target = a[i];
    if (j < nb && b[j] < target) {
      u32 step = 1;
      while (j + step < nb && b[j + step] < target)
        step <<= 1;
      u32 lo = j + (step >> 1), hi = std::min(j + step, nb - 1);
      while (lo < hi) {
        u32 mid = (lo + hi) >> 1;
        if (b[mid] < target)
          lo = mid + 1;
        else
          hi = mid;
      }
      j = lo;
    }
    bool eq = (j < nb && b[j] == target);
    out[n] = target;
    n += (op == 'A') ? eq : !eq;
    j += eq;
  }
  return n;
}

u32 combine(u8 op, const u16* a, u32 na, const u16* b, u32 nb, u16* out) {
  if ((op == 'A' && (na * 8 < nb || nb * 8 < na)) || (op == 'N' && na * 8 < nb))
    return gallop(op, a, na, b, nb, out);
  u32 i = 0, j = 0, n = 0;
  while (i < na && j < nb) {
    if (a[i] < b[j]) {
      out[n] = a[i++];
      n += (op != 'A');
    } else if (a[i] > b[j]) {
      out[n] = b[j++];
      n += (op == 'O');
    } else {
      out[n] = a[i++];
      j++;
      n += (op != 'N');
    }
  }
  if (op != 'A' && i < na) {
    memcpy(out + n, a + i, (na - i) * 2);
    n += na - i;
  }
  if (op == 'O' && j < nb) {
    memcpy(out + n, b + j, (nb - j) * 2);
    n += nb - j;
  }
  return n;
}

struct Run {
  u16 *list, *tmp;
  u64* bits;
  const u16* cur_l;
  const u64* cur_b;
  u32 n, wl;
  bool dense;

  void start(const Opnd& x) {
    dense = x.b != nullptr, n = x.n, cur_l = x.l, cur_b = x.b;
  }

  void apply(u8 op, const Opnd& x) {
    if (!x.b && !x.n) {
      if (op == 'A')
        dense = false, n = 0;
      return;
    }
    if (!dense) {
      if (n == 0) {
        if (op == 'O')
          start(x);
        return;
      }
      if (!x.b) {
        n = combine(op, cur_l, n, x.l, x.n, tmp);
        std::swap(list, tmp), cur_l = list;
      } else if (op != 'O') {
        u32 k = 0;
        for (u32 i = 0; i < n; i++) {
          u16 d = cur_l[i];
          list[k] = d;
          k += (has(x.b, d) == (op == 'A'));
        }
        n = k;
        cur_l = list;
      } else {
        memcpy(bits, x.b, wl * 8);
        for (u32 i = 0; i < n; i++)
          bits[cur_l[i] >> 6] |= 1ULL << (cur_l[i] & 63);
        cur_b = bits, dense = true;
      }
    } else if (!x.b) {
      if (op == 'A') {
        n = 0;
        for (u32 i = 0; i < x.n; i++) {
          list[n] = x.l[i];
          n += has(cur_b, x.l[i]);
        }
        cur_l = list, dense = false;
      } else {
        if (cur_b != bits)
          memcpy(bits, cur_b, wl * 8), cur_b = bits;
        if (op == 'O') {
          for (u32 i = 0; i < x.n; i++)
            bits[x.l[i] >> 6] |= 1ULL << (x.l[i] & 63);
        } else {
          for (u32 i = 0; i < x.n; i++)
            bits[x.l[i] >> 6] &= ~(1ULL << (x.l[i] & 63));
        }
      }
    } else {
      if (op == 'A') {
        for (u32 i = 0; i < wl; i++)
          bits[i] = cur_b[i] & x.b[i];
      } else if (op == 'O') {
        for (u32 i = 0; i < wl; i++)
          bits[i] = cur_b[i] | x.b[i];
      } else {
        for (u32 i = 0; i < wl; i++)
          bits[i] = cur_b[i] & ~x.b[i];
      }
      cur_b = bits;
    }
  }

  void size_sum(u64& c, u64& s) const {
    c = 0;
    s = 0;
    if (!dense) {
      c = n;
      for (u32 i = 0; i < n; i++)
        s += cur_l[i];
      return;
    }
    const __m256i m0f = _mm256_set1_epi8(0x0F), zero = _mm256_setzero_si256();
    const __m256i pop_lut = _mm256_setr_epi8(0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4, 0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4);
    const __m256i slo_lut = _mm256_setr_epi8(0, 0, 1, 1, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 0, 0, 1, 1, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6);
    const __m256i shi_lut = _mm256_setr_epi8(0, 4, 5, 9, 6, 10, 11, 15, 7, 11, 12, 16, 13, 17, 18, 22, 0, 4, 5, 9, 6, 10, 11, 15, 7, 11, 12, 16, 13, 17, 18, 22);
    const __m256i bidx = _mm256_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);
    __m256i acc_c = zero, acc_s = zero, acc_bw = zero, vi = zero;
    __m256i vstep = _mm256_set1_epi64x(256);
    u32 i = 0;
    for (; i + 4 <= wl; i += 4) {
      __m256i v = _mm256_loadu_si256((const __m256i*)(cur_b + i));
      __m256i lo = _mm256_and_si256(v, m0f);
      __m256i hi = _mm256_and_si256(_mm256_srli_epi16(v, 4), m0f);
      __m256i cb = _mm256_add_epi8(_mm256_shuffle_epi8(pop_lut, lo), _mm256_shuffle_epi8(pop_lut, hi));
      __m256i sb = _mm256_add_epi8(_mm256_shuffle_epi8(slo_lut, lo), _mm256_shuffle_epi8(shi_lut, hi));
      __m256i cw = _mm256_sad_epu8(cb, zero);
      acc_c = _mm256_add_epi64(acc_c, cw);
      __m256i sw = _mm256_add_epi64(_mm256_sad_epu8(sb, zero),
                                    _mm256_mul_epu32(cw, vi));
      acc_s = _mm256_add_epi64(acc_s, sw);
      acc_bw = _mm256_add_epi16(acc_bw, _mm256_maddubs_epi16(cb, bidx));
      vi = _mm256_add_epi64(vi, vstep);
    }
    __m256i bw32 = _mm256_madd_epi16(acc_bw, _mm256_set1_epi16(1));
    alignas(32) u64 tc[4], ts[4];
    alignas(32) u32 tbw[8];
    _mm256_store_si256((__m256i*)tc, acc_c);
    _mm256_store_si256((__m256i*)ts, acc_s);
    _mm256_store_si256((__m256i*)tbw, bw32);
    c = tc[0] + tc[1] + tc[2] + tc[3];
    u64 bw_sum = static_cast<u64>(tbw[0]) + tbw[1] + tbw[2] + tbw[3] +
                 tbw[4] + tbw[5] + tbw[6] + tbw[7];
    s = (ts[0] + ts[1] + ts[2] + ts[3]) + (bw_sum << 3);
    for (; i < wl; i++) {
      for (u64 w = cur_b[i]; w; w &= w - 1) {
        c++;
        s += i * 64 + _tzcnt_u64(w);
      }
    }
  }

  u32 smallest(u32* out, u32 limit) const {
    u32 k = 0;
    if (!dense) {
      for (; k < n && k < limit; k++)
        out[k] = cur_l[k];
      return k;
    }
    for (u32 i = 0; i < wl && k < limit; i++) {
      for (u64 w = cur_b[i]; w && k < limit; w &= w - 1)
        out[k++] = i * 64 + _tzcnt_u64(w);
    }
    return k;
  }
};

u32* g_first;
u64* g_pack;
inline void report_local(u32 q, const Run& run, u32 base) {
  if (!run.dense && !run.n) return;
  u64 c, s;
  run.size_sum(c, s);
  if (!c) return;
  g_pack[q] += c | ((s + c * base) << 20);
  u32* f = g_first + q * 11;
  if (f[10] < 10) {
    u32 m = run.smallest(f + f[10], 10 - f[10]);
    for (u32 i = 0; i < m; i++)
      f[f[10] + i] += base;
    f[10] += m;
  }
}
u32 g_nd, g_ns, g_nt;
struct Worker {
  u64* h;
  u32 *dend, *uniq, *doc_off;
  u16* stamp;
  u32 *df, *start, *pairs, pcap;
  u16* post;
  u32 postcap;
  u64* rows;
  size_t rowscap;
  u64 from, to;
  u32 base, wl;
  Run run;
};
template <bool kSample>
void scan(Worker& w, u64 from, u64 to, u32 wl) {
  const char* d = g_d.p;
  u32 nt = 0, nd = 0, done = 0, np = 0;
  u64 carry_a = 0, carry_nl = 1, in_id = 0, open = ~0ULL;
  if constexpr (!kSample)
    w.doc_off[0] = 0;
  auto flush = [&]() {
    u32 end = w.dend[nd - 1];
    u32 a = 0;
    for (u32 k = 0; k < nd; k++) {
      u32 b = w.dend[k], local = done + k, nu = 0;
      u16 cur = static_cast<u16>(local + 1);
      w.stamp[0] = cur;
      for (u32 i = a; i < b; i++) {
        u64 nh = w.h[i + kAhead];
        __builtin_prefetch(g_tab + b1_of(nh) * 4);
        __builtin_prefetch(g_tab + b2_of(nh) * 4);
        u32 id = lookup(w.h[i]);
        u16 prev = w.stamp[id];
        w.stamp[id] = cur;
        w.uniq[nu] = id;
        nu += (prev != cur);
      }
      if constexpr (kSample) {
        for (u32 j = 0; j < nu; j++)
          w.df[w.uniq[j]]++;
      } else {
        if (np + nu + 1 > w.pcap) {
          w.pcap = std::max(w.pcap + (w.pcap >> 1), np + nu + 16384);
          w.pairs = static_cast<u32*>(realloc(w.pairs, w.pcap * 4));
        }
        u64 bit = 1ULL << (local & 63);
        for (u32 j = 0; j < nu; j++) {
          u32 id = w.uniq[j];
          w.rows[(id <= g_nd ? id : 0) * wl + (local >> 6)] |= bit;
          w.pairs[np] = ((id - g_nd - 1) << kDocBits) | local;
          np += (id > g_nd);
        }
      }
      a = b;
    }
    memmove(w.h, w.h + end, (nt - end) * 8);
    nt -= end, done += nd, nd = 0;
  };
  auto emit = [&](u64 s, u32 len) {
    const char* q = d + s;
    u64 w0 = _bzhi_u64(load64(q) | (kOnes * 0x20), len << 3);
    u64 w1 = _bzhi_u64(load64(q + 8) | (kOnes * 0x20),
                       len > 8 ? (len - 8) << 3 : 0);
    w.h[nt++] = (len > 16) ? hash_bytes(q, len, true) : hash16(w0, w1);
  };
  const __m256i k20 = _mm256_set1_epi8(0x20), k60 = _mm256_set1_epi8(0x60);
  const __m256i k7B = _mm256_set1_epi8(0x7B), k2F = _mm256_set1_epi8(0x2F);
  const __m256i k3A = _mm256_set1_epi8(0x3A), k0A = _mm256_set1_epi8(10);
  const __m256i k09 = _mm256_set1_epi8(9);
  for (u64 at = from; at < to; at += 64) {
    u64 alnum = 0, nl = 0;
    for (int half = 0; half < 2; half++) {
      __m256i v = _mm256_loadu_si256(
          reinterpret_cast<const __m256i*>(d + at + 32 * half));
      __m256i lo = _mm256_or_si256(v, k20);
      __m256i letter = _mm256_and_si256(_mm256_cmpgt_epi8(lo, k60),
                                        _mm256_cmpgt_epi8(k7B, lo));
      __m256i digit = _mm256_and_si256(_mm256_cmpgt_epi8(v, k2F),
                                       _mm256_cmpgt_epi8(k3A, v));
      alnum |= static_cast<u64>(static_cast<u32>(
                   _mm256_movemask_epi8(_mm256_or_si256(letter, digit))))
               << (32 * half);
      u32 m_nl = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, k0A));
      nl |= static_cast<u64>(m_nl) << (32 * half);
    }
    if (to - at < 64) {
      u64 valid = (1ULL << (to - at)) - 1;
      alnum &= valid;
      nl &= valid;
    }
    if (__builtin_expect((nl | carry_nl | in_id) != 0, 0)) {
      __m256i v0 =
          _mm256_loadu_si256(reinterpret_cast<const __m256i*>(d + at));
      __m256i v1 =
          _mm256_loadu_si256(reinterpret_cast<const __m256i*>(d + at + 32));
      u32 t0 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, k09));
      u32 t1 = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, k09));
      u64 tab = t0 | (static_cast<u64>(t1) << 32);
      if (to - at < 64)
        tab &= (1ULL << (to - at)) - 1;
      u64 toggle = ((nl << 1) | carry_nl) | tab;
      u64 idm = _mm_cvtsi128_si64(_mm_clmulepi64_si128(
          _mm_set_epi64x(0, toggle), _mm_set1_epi8(-1), 0));
      idm ^= -in_id, in_id = idm >> 63, carry_nl = nl >> 63, alnum &= ~idm;
    }
    u64 prev = (alnum << 1) | carry_a;
    u64 starts = alnum & ~prev, ends = ~alnum & prev;
    carry_a = alnum >> 63;
    u32 nt0 = nt;
    u64 e = ends, s = starts;
    if (open != ~0ULL && e) {
      emit(open, static_cast<u32>(at + _tzcnt_u64(e) - open));
      e &= e - 1, open = ~0ULL;
    }
    while (e) {
      u64 st = at + _tzcnt_u64(s), en = at + _tzcnt_u64(e);
      s &= s - 1, e &= e - 1;
      emit(st, static_cast<u32>(en - st));
    }
    if (s)
      open = at + _tzcnt_u64(s);
    for (u64 n = nl; n; n &= n - 1) {
      u32 pos = _tzcnt_u64(n);
      w.dend[nd++] = nt0 + __builtin_popcountll(ends & ((2ULL << pos) - 1));
      if constexpr (!kSample)
        w.doc_off[done + nd] = static_cast<u32>(at - from) + pos + 1;
    }
    if (nt >= kFlush || nd > 100) {
      if (nd)
        flush();
    }
  }
  if (nd)
    flush();
  if constexpr (!kSample) {
    u32* start = w.start;
    memset(start, 0, (g_ns + 1) * 4);
    for (u32 i = 0; i < np; i++)
      start[w.pairs[i] >> kDocBits]++;
    for (u32 i = 1; i <= g_ns; i++)
      start[i] += start[i - 1];
    if (np > w.postcap) {
      w.postcap = np + (np >> 3) + 1024;
      w.post = static_cast<u16*>(realloc(w.post, w.postcap * 2));
    }
    for (u32 i = np; i-- > 0;)
      w.post[--start[w.pairs[i] >> kDocBits]] = w.pairs[i] & ((1 << kDocBits) - 1);
  }
}
std::barrier<> g_bar(kThreads);
std::vector<Worker*> g_workers(kThreads);
std::vector<u32> g_dfall, g_fin;
std::atomic<u32> g_eval_next[kWaves];
u64 g_osz[kThreads];
int g_fd;
struct Digits {
  char d[200];
  constexpr Digits() : d() {
    for (int i = 0; i < 100; i++) d[i * 2] = '0' + i / 10, d[i * 2 + 1] = '0' + i % 10;
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
  for (const char* p = g_d.p + at; *p >= '0' && *p <= '9'; p++)
    x = x * 10 + (*p - '0');
  return x;
}
void sort_by_df(u32* arr, u32 n) {
  u32 df[8];
  for (u32 i = 0; i < n; i++)
    df[i] = g_dfall[arr[i]];
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
  std::vector<Query>& pc = g_piece[tid];
  parse(tid, pc);
  g_psize[tid] = pc.size();
  g_bar.arrive_and_wait();
  if (tid == 0) {
    u32 nq = 0;
    for (int t = 0; t < kThreads; t++)
      nq += static_cast<u32>(g_psize[t]);
    g_nq = nq;
    g_queries.reset(new Query[nq ? nq : 1]);
    g_first = alloc<u32>((nq ? nq : 1) * 11), g_pack = alloc<u64>(nq ? nq : 1);
    for (u32 i = 0; i < kMaxProv; i++) {
      if (g_prov[i])
        cuckoo_insert(g_prov[i]);
    }
    free(g_prov);
  }
  g_bar.arrive_and_wait();
  {
    u32 o = 0;
    for (int t = 0; t < tid; t++)
      o += static_cast<u32>(g_psize[t]);
    if (!pc.empty())
      memcpy(&g_queries[o], pc.data(), pc.size() * sizeof(Query));
    std::vector<Query>().swap(pc);
  }
  Worker w = {};
  w.df = alloc<u32>(kMaxProv + 1), w.stamp = alloc<u16>(kMaxProv + 1);
  w.h = alloc<u64>(kCap + 64), w.uniq = alloc<u32>(kCap + 64);
  w.dend = alloc<u32>(256), g_workers[tid] = &w;
  u64 sa = cut(g_d, tid, 96 * kSamples), sb = cut(g_d, tid + 1, 96 * kSamples);
  if (sa < sb)
    scan<true>(w, sa, sb, 0);
  g_bar.arrive_and_wait();
  {
    size_t per = (kMaxProv + kThreads) / kThreads;
    size_t hi = std::min<size_t>(per * (tid + 1), kMaxProv + 1);
    for (size_t i = per * tid; i < hi; i++) {
      u32 sum = 0;
      for (int t = 0; t < kThreads; t++)
        sum += g_workers[t]->df[i];
      g_dfall[i] = sum;
    }
  }
  g_bar.arrive_and_wait();
  if (tid == 0) {
    u64 end = cut(g_d, kSamples, 96 * kSamples);
    u32 seen = (end >= g_d.n) ? g_ndocs : first_doc(end);
    auto dense = [&](u32 p) {
      return g_used[p] && static_cast<u64>(g_dfall[p]) * 32 >= seen;
    };
    u32 nd = 0, ns = 0;
    for (u32 p = 1; p <= kMaxProv; p++) {
      if (g_used[p]) {
        if (dense(p)) {
          nd++;
        } else {
          ns++;
        }
      }
    }
    g_nd = nd, g_ns = ns, g_nt = nd + ns;
    g_fin.assign(kMaxProv + 1, 0);
    u32 a = 0, b = 0;
    for (u32 p = 1; p <= kMaxProv; p++) {
      if (g_used[p])
        g_fin[p] = dense(p) ? (1 + a++) : (nd + 1 + b++);
    }
    free(g_used);
  }
  g_bar.arrive_and_wait();
  {
    u32 total_slots = kBuckets * 4;
    u32 per = (total_slots + kThreads - 1) / kThreads;
    u32 hi = std::min<u32>(per * (tid + 1), total_slots);
    for (u32 i = per * tid; i < hi; i++) {
      u64 e = g_tab[i];
      if (e)
        g_tab[i] = (e & kTag) | g_fin[e & kPay];
    }
    u32 q0 = static_cast<u64>(g_nq) * tid / kThreads;
    u32 q1 = static_cast<u64>(g_nq) * (tid + 1) / kThreads;
    for (u32 q = q0; q < q1; q++) {
      Query& qq = g_queries[q];
      if (!qq.phrase) {
        u32 fo = 0;
        while (fo + 1 < qq.n && qq.ops[fo] != 'O')
          fo++;
        if (fo > 0) {
          u32 pos[8], neg[8], np = 0, nn = 0;
          pos[np++] = qq.t[0];
          for (u32 j = 0; j < fo; j++) {
            if (qq.ops[j] == 'A') {
              pos[np++] = qq.t[j + 1];
            } else {
              neg[nn++] = qq.t[j + 1];
            }
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
        if (qq.has_or) {
          int lo = static_cast<int>(qq.n) - 2;
          while (lo >= 0 && qq.ops[lo] != 'O')
            lo--;
          u32 best_m = 0;
          for (u32 j = static_cast<u32>(lo + 1); j + 1 < qq.n; j++) {
            if (qq.ops[j] == 'A') {
              u32 cand = qq.t[j + 1];
              if (!best_m || g_dfall[cand] < g_dfall[best_m])
                best_m = cand;
            }
          }
          if (best_m)
            qq.must = g_fin[best_m];
        }
        for (u32 j = 0; j < qq.n; j++) qq.t[j] = g_fin[qq.t[j]];
      } else {
        u32 best = 0;
        for (u32 j = 1; j < qq.n; j++) {
          if (g_dfall[qq.t[j]] < g_dfall[qq.t[best]])
            best = j;
        }
        qq.best = static_cast<u8>(best);
        sort_by_df(qq.t, qq.n);
        u32 ut = 0;
        for (u32 j = 0; j < qq.n; j++) {
          u32 f = g_fin[qq.t[j]];
          bool dup = false;
          for (u32 k = 0; k < ut; k++) {
            if (qq.t[k] == f)
              dup = true;
          }
          if (!dup)
            qq.t[ut++] = f;
        }
        qq.ut = static_cast<u8>(ut);
      }
    }
  }
  g_bar.arrive_and_wait();
  if (tid == 0) {
    std::vector<u32>().swap(g_dfall);
    std::vector<u32>().swap(g_fin);
  }
  free(w.df), free(w.stamp);
  w.stamp = alloc<u16>(g_nt + 2), w.start = alloc<u32>(g_ns + 2);
  w.doc_off = alloc<u32>((1 << kDocBits) + 8);
  w.pcap = 1 << 17, w.pairs = alloc<u32>(w.pcap);
  w.postcap = 1 << 17, w.post = alloc<u16>(w.postcap);
  w.rows = nullptr, w.rowscap = 0;
  Run& run = w.run;
  run.list = alloc<u16>((1 << kDocBits) + 64), run.tmp = alloc<u16>((1 << kDocBits) + 64);
  run.bits = alloc<u64>((1 << kDocBits) / 64 + 8);
  for (int wave = 0; wave < kWaves; wave++) {
    int c = wave * kThreads + tid;
    w.from = cut(g_d, c, kChunks), w.to = cut(g_d, c + 1, kChunks);
    if (w.from < w.to) {
      u32 end = (w.to == g_d.n) ? g_ndocs : first_doc(w.to);
      w.base = first_doc(w.from), w.wl = (end - w.base + 63) / 64;
      if (static_cast<size_t>(g_nd + 1) * w.wl > w.rowscap) {
        free(w.rows);
        w.rowscap = static_cast<size_t>(g_nd + 1) * w.wl;
        w.rows = alloc<u64>(w.rowscap);
      } else {
        memset(w.rows, 0, static_cast<size_t>(g_nd + 1) * w.wl * 8);
      }
      memset(w.stamp, 0, (g_nt + 1) * 2);
      scan<false>(w, w.from, w.to, w.wl);
    }
    g_bar.arrive_and_wait();
    for (u32 qb;
         (qb = g_eval_next[wave].fetch_add(kBatch, std::memory_order_relaxed)) <
         g_nq;) {
      u32 qe = std::min(qb + kBatch, g_nq);
      for (int t = 0; t < kThreads; t++) {
        const Worker& cw = *g_workers[t];
        if (cw.from >= cw.to)
          continue;
        u32 base = cw.base, wl = cw.wl;
        const u32* start = cw.start;
        run.wl = wl;
        auto operand = [&](u32 term) {
          if (term <= g_nd)
            return Opnd{nullptr, cw.rows + static_cast<size_t>(term) * wl, 0};
          u32 sid = term - g_nd - 1;
          return Opnd{cw.post + start[sid], nullptr,
                      start[sid + 1] - start[sid]};
        };
        for (u32 q = qb; q < qe; q++) {
          const Query& qq = g_queries[q];
          if (!qq.phrase) {
            Opnd x = operand(qq.t[0]);
            if (!x.b && !x.n && !qq.has_or)
              continue;
            if (qq.must) {
              Opnd m = operand(qq.must);
              if (!m.b && !m.n)
                continue;
            }
            run.start(x);
            for (u32 k = 0; k + 1 < qq.n; k++) {
              run.apply(qq.ops[k], operand(qq.t[k + 1]));
              if (!run.dense && !run.n && !qq.has_or)
                break;
            }
            report_local(q, run, base);
          } else {
            Opnd x0 = operand(qq.t[0]);
            if (!x0.b && !x0.n)
              continue;
            if (qq.ut > 1) {
              Opnd x1 = operand(qq.t[1]);
              if (!x1.b && !x1.n)
                continue;
              run.start(x0);
              run.apply('A', x1);
              for (u32 k = 2; k < qq.ut && (run.dense || run.n); k++)
                run.apply('A', operand(qq.t[k]));
            } else {
              run.start(x0);
            }
            if (!run.dense && !run.n)
              continue;
            if (qq.n == 1) {
              report_local(q, run, base);
              continue;
            }
            const char* chunk_p = g_d.p + cw.from;
            const u32* doff = cw.doc_off;
            u64 cnt = 0, sum = 0;
            u32* f = g_first + q * 11;
            auto check_doc = [&](u32 local) {
              if (verify_phrase(qq, chunk_p + doff[local],
                                chunk_p + doff[local + 1] - 1)) {
                cnt++;
                sum += local;
                if (f[10] < 10)
                  f[f[10]++] = base + local;
              }
            };
            if (!run.dense) {
              for (u32 i = 0; i < run.n; i++)
                check_doc(run.cur_l[i]);
            } else {
              for (u32 i = 0; i < wl; i++) {
                for (u64 wbits = run.cur_b[i]; wbits; wbits &= wbits - 1)
                  check_doc(i * 64 + _tzcnt_u64(wbits));
              }
            }
            if (cnt)
              g_pack[q] += cnt | ((sum + cnt * base) << 20);
          }
        }
      }
    }
    g_bar.arrive_and_wait();
  }
  free(w.rows), free(w.post), free(w.pairs), free(w.start), free(w.stamp);
  free(w.h), free(w.uniq), free(w.dend), free(w.doc_off);
  free(run.list), free(run.tmp), free(run.bits);
  u32 q0 = static_cast<u64>(g_nq) * tid / kThreads;
  u32 q1 = static_cast<u64>(g_nq) * (tid + 1) / kThreads;
  u8* buf = static_cast<u8*>(malloc(static_cast<size_t>(q1 - q0) * 144 + 64));
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
  if (tid == 0)
    g_fd = open("/out/results.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  g_bar.arrive_and_wait();
  off_t at = 0;
  for (int x = 0; x < tid; x++)
    at += g_osz[x];
  for (size_t done = 0; done < g_osz[tid];) {
    ssize_t n = pwrite(g_fd, buf + done, g_osz[tid] - done, at + done);
    if (n <= 0)
      break;
    done += n;
  }
  g_bar.arrive_and_wait();
  if (tid == 0)
    _exit(0);
}
}  // namespace
int main() {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
  g_q = map_file("/data/queries.txt");
  g_d = map_file("/data/docs.txt");
  const char* last =
      static_cast<const char*>(memrchr(g_d.p, '\n', g_d.n - 1));
  g_ndocs = first_doc(last ? last + 1 - g_d.p : 0) + 1;
  g_prov = alloc<u64>(kMaxProv), g_used = alloc<u8>(kMaxProv + 1);
  g_dfall.assign(kMaxProv + 1, 0);
  std::thread th[kThreads - 1];
  for (int i = 0; i < kThreads - 1; i++)
    th[i] = std::thread(worker, i + 1);
  worker(0);
  for (auto& t : th)
    t.join();
}
