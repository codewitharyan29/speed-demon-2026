#include <bits/stdc++.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
using u8 = uint8_t;
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;
using Ov = std::vector<std::pair<u32, u64>>;
constexpr u32 RING = 4096, POOL = 1 << 13, T = 12, CHUNKS = 48, MSP = 1 << 17;
constexpr auto rx = std::memory_order_relaxed;
static u32 n, m, *first, *adj, *rfirst, *radj, *e4;
struct Sp {
  u32 u, e;
  u64 w;
};
static Ov ov, rov;

template <class P = u32> static P *anon(size_t count) {
  void *p =
      mmap(0, count * sizeof(P) + 4096, 3, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  return madvise(p, count * sizeof(P) + 4096, MADV_NOHUGEPAGE), (P *)p;
}

static const char *map(const char *path, size_t &len) {
  struct stat st;
  int fd = open(path, O_RDONLY);
  fstat(fd, &st), len = st.st_size;
  char *p = (char *)mmap(0, len + 4096, 1, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  return (const char *)mmap(p, len, 1, MAP_SHARED | MAP_FIXED, fd, 0);
}

static u64 num(const char *&p) {
  u64 w;
  memcpy(&w, p, 8);
  u64 sep = ~w & 0x1010101010101010ULL;
  if (__builtin_expect(!sep, 0)) {
    u64 x = 0;
    while (*p >= '0')
      x = x * 10 + *p++ - '0';
    return p++, x;
  }
  u32 len = std::countr_zero(sep) >> 3;
  p += len + 1, w <<= 64 - 8 * len;
  w = (w & 0x0F0F0F0F0F0F0F0FULL) * 2561 >> 8;
  w = (w & 0x00FF00FF00FF00FFULL) * 6553601 >> 16;
  return (w & 0x0000FFFF0000FFFFULL) * 42949672960001ULL >> 32;
}

static u64 heavy(const Ov &o, u32 edge) {
  return std::lower_bound(o.begin(), o.end(), std::pair<u32, u64>{edge, 0})
      ->second;
}

struct Dial {
  u16 head[RING] = {}, fr = 0, used = 0;
  u32 at = 0;
  u64 bits[RING / 64] = {}, top = 0, *__restrict pool;
  Dial(u64 *p) : pool(p) {}
  void push(u32 b, u32 node) {
    u16 h = head[b], i = fr ? fr : ++used;
    !h && (bits[b >> 6] |= 1ULL << (b & 63), top |= 1ULL << (b >> 6));
    fr = fr ? pool[i] >> 32 : 0;
    pool[i] = node | (u64)h << 32, head[b] = i;
  }
  bool pop(u32 &b, u32 &node) {
    if (!top)
      return 0;
    if (!head[at]) {
      u32 w = at >> 6;
      if (!(bits[w] &= ~(1ULL << (at & 63))) && !(top &= ~(1ULL << w)))
        return 0;
      w = std::countr_zero(top), at = w << 6 | std::countr_zero(bits[w]);
    }
    u64 h = head[at], e = pool[h];
    node = e, b = at, head[at] = e >> 32, pool[h] = (u64)fr << 32, fr = h;
    return 1;
  }
  void clear() {
    for (; top; top &= top - 1)
      for (u32 w = std::countr_zero(top); bits[w]; bits[w] &= bits[w] - 1)
        head[w << 6 | std::countr_zero(bits[w])] = 0;
    fr = used = at = 0;
  }
};

struct Dist {
  u16 *__restrict pt, *__restrict own, np = 1;
  u32 *__restrict pool;
  u32 get(u32 v) { return pool[(u32)pt[v >> 7] << 7 | (v & 127)]; }
  void set(u32 v, u32 x) {
    u16 &p = pt[v >> 7];
    !p && (own[np] = v >> 7, p = np++, 1);
    pool[(u32)p << 7 | (v & 127)] = x;
  }
  void clear() {
    for (u32 i = 1; i < np; i++)
      memset(pool + (i << 7), 0, 512), pt[own[i]] = 0;
    np = 1;
  }
};

static std::mutex smtx;
static long slow(u32 s, u32 t) {
  std::lock_guard g(smtx);
  u32 P = (n >> 7) + 2, np = 1;
  auto mem = anon<u64>(P * 257ULL);
  auto cell = [&](u32 v) {
    u16 &p = ((u16 *)mem)[v >> 7];
    !p && (p = np++, 1);
    return mem + P + ((size_t)p << 8 | (v & 127) << 1);
  };
  std::priority_queue<std::pair<u64, u32>> pq[2];
  u64 mu = cell(s)[0] = cell(t)[1] = ~0ULL, low[2] = {};
  pq[0].push({~0ULL, s}), pq[1].push({~0ULL, t});
  while (!pq[0].empty() && !pq[1].empty()) {
    u32 d = pq[0].size() > pq[1].size();
    auto [k, u] = pq[d].top();
    pq[d].pop();
    if ((low[d] = ~k) + low[d ^ 1] >= mu)
      break;
    if (k < cell(u)[d])
      continue;
    const u32 *F = d ? rfirst : first, *A = d ? radj : adj;
    for (u32 i = F[u]; i < F[u + 1]; i++) {
      u32 v = A[i] >> 11, w = A[i] & 2047;
      u64 nd = ~k + (w == 2047 ? heavy(d ? rov : ov, i) : w), nk = ~nd,
          *vp = cell(v);
      if (nk > vp[d])
        vp[d] = nk, pq[d].push({nk, v}),
        vp[d ^ 1] && (mu = std::min(mu, nd + ~vp[d ^ 1]), 1);
    }
  }
  return munmap(mem, P * 2056ULL + 4096), mu == ~0ULL ? -1 : (long)mu;
}

struct Side {
  const u32 *__restrict first, *__restrict adj;
  const Ov *ov;
  Dial q;
  u32 low = 0;
};

static long fast(u32 s, u32 t, Dist &D, Side &F, Side &R) {
  u32 b, u, mu = ~0u, big = 0;
  D.set(s, 0xFFFF), D.set(t, D.get(t) | 0xFFFF0000u);
  F.q.push(0, s), R.q.push(0, t), F.low = R.low = 0;
  auto step = [&]<int SH>(Side &S, u32 olow) {
    if (!S.q.pop(b, u) || (S.low = b << 4, S.low + olow >= mu))
      return 0;
    if (u16 nh = S.q.head[b])
      __builtin_prefetch(S.adj + S.first[(u32)S.q.pool[nh]]);
    u32 d = (u16) ~(D.get(u) >> SH);
    if (d >> 4 == b) {
      u64 r;
      memcpy(&r, S.first + u, 8);
      for (u32 i = r, hi = r >> 32; i < hi; i++) {
        u32 e = S.adj[i], v = e >> 11, w = e & 2047, x = D.get(v);
        u64 nd = d + (__builtin_expect(w == 2047, 0) ? heavy(*S.ov, i) : w);
        if (__builtin_expect(nd >= 60000, 0)) {
          big = 1;
          continue;
        }
        u16 inv = ~(u16)nd, mine = x >> SH, other = x >> (16 - SH);
        if (inv > mine)
          D.set(v, (x & ~(0xFFFFu << SH)) | (u32)inv << SH),
              S.q.push(nd >> 4, v),
              other && (mu = std::min(mu, (u32)nd + (u16)~other), 1),
              __builtin_prefetch(S.first + v);
      }
    }
    return 1;
  };
  while (F.low <= R.low ? step.operator()<0>(F, R.low)
                        : step.operator()<16>(R, F.low))
    ;
  F.q.clear(), R.q.clear(), D.clear();
  long ans = mu < 60000 || (mu != ~0u && !big) ? (long)mu : -1;
  return ans < 0 && big ? slow(s, t) : ans;
}

int main() {
  size_t glen, qlen;
  const char *file = map("/data/graph.txt", glen), *body = file;
  n = num(body), m = num(body);
  size_t blen = file + glen - 1 - body;
  auto cut = [&](size_t c) {
    return c ? (const char *)memchr(body + blen * c / CHUNKS, '\n', blen) + 1
             : body;
  };
  size_t keep = (((size_t)n + m + 2) * 4 + 4095) & ~4095ULL;
  size_t e4b = std::max<size_t>(keep, (4 * (size_t)n * 4 + 4095) & ~4095ULL);
  size_t tot = e4b / 4 + (n + 3) / 4 + (n / 64 + 2) * 2 + 8 * MSP + 1024;
  first = anon(n + 2), adj = anon(m), rfirst = e4 = anon(tot),
  radj = e4 + n + 2;
  auto cnt = (u8 *)e4 + e4b;
  auto vis = (u64 *)(((uintptr_t)(cnt + n + 15)) & ~7ULL);
  auto sp = (Sp *)(vis + n / 64 + 2), spt = sp + MSP;
  std::atomic<size_t> job = 0, seed = 0, rank = 0, spn = 0, cur[T * 8];
  std::barrier sync(T);
  Ov ovs, ov0[T];
  std::vector<std::array<u32, 3>> work;
  std::vector<long> ans;
  auto worker = [&](u32 tid) {
    for (size_t c; (c = job.fetch_add(1, rx)) < CHUNKS;)
      for (auto q = cut(c), e = cut(c + 1); q < e;) {
        u32 U[16], V[16], k = 0;
        u64 W[16];
        for (; k < 16 && q < e; k++)
          U[k] = num(q), V[k] = num(q), W[k] = num(q),
          __builtin_prefetch(cnt + U[k], 1),
          __builtin_prefetch(e4 + 4 * (size_t)U[k], 1);
        for (u32 j = 0; j < k; j++) {
          u64 u = U[j], w = W[j], e = V[j] << 11 | std::min<u64>(w, 2047);
          u32 s = std::atomic_ref<u8>(cnt[u]).fetch_add(1, rx);
          if (s < 4)
            e4[4 * u + s] = e,
            w >= 2047 && (ov0[tid].push_back({4 * u + s, w}), 1);
          else
            sp[spn.fetch_add(1, rx)] = {(u32)u, (u32)e, w};
        }
      }
    sync.arrive_and_wait();
    if (!tid) {
      for (auto &v : ov0)
        ovs.insert(ovs.end(), v.begin(), v.end()), v.clear();
      std::sort(ovs.begin(), ovs.end());
      for (int sh = 0; sh < 22; sh += 11) {
        u32 c[2049] = {};
        for (size_t i = 0; i < spn; i++)
          c[(sp[i].u >> sh & 2047) + 1]++;
        std::partial_sum(c, c + 2049, c);
        for (size_t i = 0; i < spn; i++)
          spt[c[sp[i].u >> sh & 2047]++] = sp[i];
        std::swap(sp, spt);
      }
    }
    sync.arrive_and_wait();
    auto claim = [&](u32 v) {
      std::atomic_ref<u64> cell(vis[v / 64]);
      u64 bit = 1ULL << v % 64;
      return !(cell.load(rx) & bit) && !(cell.fetch_or(bit, rx) & bit);
    };
    size_t nsp = spn, nlo = (size_t)n * tid / T,
           nhi = (size_t)n * (tid + 1) / T;
    u32 *qb = (u32 *)spt + 8192 * tid;
    for (size_t blk; (blk = seed.fetch_add(1024, rx)) < n;)
      for (u32 r = blk; r < std::min<size_t>(blk + 1024, n); r++) {
        if (!claim(r))
          continue;
        u32 h = 0, t = 1, edges = 0;
        for (qb[0] = r; h < t && t < 8192; h++) {
          h + 4 < t && (__builtin_prefetch(e4 + 4 * (size_t)qb[h + 4]), 1);
          for (u32 c = 0, x = qb[h], lim = std::min<u32>(cnt[x], 4);
               c < lim && t < 8192; c++)
            if (u32 v = e4[4 * (size_t)x + c] >> 11; claim(v))
              qb[t++] = v, __builtin_prefetch(e4 + 4 * (size_t)v),
              __builtin_prefetch(cnt + v);
        }
        for (u32 k = 0; k < t; k++)
          edges += cnt[qb[k]];
        size_t at = rank.fetch_add((size_t)t << 32 | edges, rx);
        u32 id = at >> 32, dst = (u32)at;
        for (u32 k = 0; k < t; k++, id++) {
          u32 x = qb[k], cx = cnt[x];
          for (u32 c = 0; c < std::min(cx, 4u); c++, dst++)
            adj[dst] = e4[4 * (size_t)x + c],
            (adj[dst] & 2047) == 2047 &&
                (ov0[tid].push_back({dst, heavy(ovs, 4 * x + c)}), 1);
          e4[4 * (size_t)x] = id | ((u32)cx << 24);
          if (cx > 4)
            for (auto *a = std::lower_bound(
                     sp, sp + nsp, x,
                     [](const Sp &y, u32 v) { return y.u < v; });
                 a < sp + nsp && a->u == x; a++, dst++)
              adj[dst] = a->e,
              (a->e & 2047) == 2047 && (ov0[tid].push_back({dst, a->w}), 1);
        }
      }
    sync.arrive_and_wait();
    !tid &&munmap((char *)e4 + e4b, tot * 4 + 4096 - e4b);
    for (size_t x = nlo; x < nhi; x++)
      first[x] = e4[4 * x];
    sync.arrive_and_wait();
    !tid &&e4b > keep &&munmap((char *)e4 + keep, e4b - keep);
    memset(rfirst + nlo, 0, (nhi - nlo + (tid == T - 1 ? 2 : 0)) * 4);
    for (size_t x = nlo; x < nhi; x++)
      radj[first[x] & 0xFFFFFF] = first[x] >> 24, first[x] &= 0xFFFFFF;
    sync.arrive_and_wait();
    for (size_t i = (size_t)m * tid / T; i < (size_t)m * (tid + 1) / T; i++) {
      __builtin_prefetch(first + (adj[i + 16] >> 11));
      u32 e = adj[i], v = first[e >> 11];
      adj[i] = v << 11 | (e & 2047),
      std::atomic_ref(rfirst[v + 2]).fetch_add(1, rx);
    }
    sync.arrive_and_wait();
    if (!tid) {
      for (auto &v : ov0)
        ov.insert(ov.end(), v.begin(), v.end()), v.clear();
      std::sort(ov.begin(), ov.end());
      const char *qp = map("/data/queries.txt", qlen), *qe = qp + qlen;
      while (qp < qe) {
        u32 s = num(qp), t = num(qp), a = first[s], b = first[t];
        ans.push_back(s == t ? 0 : -1);
        if (s != t && rfirst[b + 2] && radj[a])
          work.push_back({a, b, (u32)ans.size() - 1});
      }
      std::partial_sum(radj, radj + n, first + 1), first[0] = 0;
      std::partial_sum(rfirst + 1, rfirst + n + 2, rfirst + 1);
    }
    sync.arrive_and_wait();
    for (size_t u = nlo; u < nhi; u++)
      for (u32 i = first[u]; i < first[u + 1]; i++) {
        __builtin_prefetch(rfirst + (adj[i + 16] >> 11) + 1, 1);
        u32 e = adj[i], v = e >> 11, w = e & 2047;
        u32 dst = std::atomic_ref(rfirst[v + 1]).fetch_add(1, rx);
        radj[dst] = u << 11 | w;
        w == 2047 && (ov0[tid].push_back({dst, heavy(ov, i)}), 1);
      }
    sync.arrive_and_wait();
    if (!tid) {
      for (auto &v : ov0)
        rov.insert(rov.end(), v.begin(), v.end());
      std::sort(rov.begin(), rov.end()), std::sort(work.begin(), work.end());
      for (u32 k = 0; k < T; k++)
        cur[k * 8] = k * work.size() / T;
    }
    sync.arrive_and_wait();
    u32 P = (n >> 7) + 2;
    auto pool = anon<u64>(2 * POOL + 65 * (size_t)P + 2);
    auto pt = (u16 *)(pool + 2 * POOL);
    Dist D{pt, pt + P, 1, (u32 *)(pt + 2 * P)};
    Side F{first, adj, &ov, Dial(pool)},
        R{rfirst, radj, &rov, Dial(pool + POOL)};
    for (u32 k = 0; k < T; k++)
      for (size_t j, w = (tid + k) % T, end = (w + 1) * work.size() / T;
           (j = cur[w * 8].fetch_add(1, rx)) < end;)
        ans[work[j][2]] = fast(work[j][0], work[j][1], D, F, R);
    sync.arrive_and_wait();
    if (!tid) {
      std::string out;
      char buf[24];
      for (long a : ans)
        out.append(buf, std::to_chars(buf, buf + 24, a).ptr).push_back('\n');
      int fd = open("/out/distances.txt", O_WRONLY | O_CREAT | O_TRUNC, 0666);
      write(fd, out.data(), out.size()), _exit(0);
    }
  };
  std::vector<std::jthread> th;
  for (u32 i = 0; i < T; i++)
    th.emplace_back(worker, i);
}