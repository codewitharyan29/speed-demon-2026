#include <bits/stdc++.h>
#include <fcntl.h>
#include <immintrin.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;
using Ov = std::vector<std::pair<u32, u64>>; // (edge index, weight) if >= 2047
constexpr u32 NIL = ~0u, RING = 4096, POOL = 65536, T = 12;
constexpr u32 CHUNKS = 48;
constexpr auto rx = std::memory_order_relaxed;
static u32 n, m, *first, *adj; // CSR (renumbered): offsets and packed edges
struct Sp {                    // an edge beyond the fourth of its node
  u32 u, e;
  u64 w;
};
static Sp *rsp;

// Sorts spilled edges by node: two counting passes over 11 bits each.
static void sort_sp(Sp *a, size_t k) {
  std::vector<Sp> t(k);
  for (int sh = 0; sh < 22; sh += 11) {
    u32 c[2049] = {};
    for (size_t i = 0; i < k; i++)
      c[(a[i].u >> sh & 2047) + 1]++;
    std::partial_sum(c, c + 2049, c);
    for (size_t i = 0; i < k; i++)
      t[c[a[i].u >> sh & 2047]++] = a[i];
    std::copy(t.begin(), t.end(), a);
  }
}
static Ov ov;     // true weights of edges packed as 2047
static u32 *re4;  // the graph with every edge reversed, four slots
static u16 *rcnt; // per node, like the input; here: edges per node
static size_t rn; // and the reversed edges beyond the fourth
static Ov rov;    // true weights of reversed edges packed as 2047

// Zeroed memory. Transparent huge pages are off: the sparse structures would
// be charged in whole 2 MB pieces.
template <class P = u32> static P *anon(size_t count) {
  void *p =
      mmap(0, count * sizeof(P) + 4096, 3, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  return madvise(p, count * sizeof(P) + 4096, MADV_NOHUGEPAGE), (P *)p;
}

static const char *map(const char *path, size_t &len) {
  struct stat st;
  int fd = open(path, O_RDONLY);
  fstat(fd, &st);
  return (const char *)mmap(0, len = st.st_size, 1, MAP_SHARED, fd, 0);
}

// Reads digits and the one separator after them.
static u32 num(const char *&p) {
  u32 x = 0;
  while (*p >= '0')
    x = x * 10 + *p++ - '0';
  return p++, x;
}

// Just past the next newline, eight bytes at a time.
static const char *next_line(const char *p, const char *e) {
  for (u64 w, z; p + 8 <= e; p += 8) {
    memcpy(&w, p, 8), w ^= 0x0A0A0A0A0A0A0A0AULL;
    if ((z = (w - 0x0101010101010101ULL) & ~w & 0x8080808080808080ULL))
      return p + (std::countr_zero(z) >> 3) + 1;
  }
  return (const char *)memchr(p, '\n', e - p) + 1;
}

template <class F> static void par(F f) {
  std::vector<std::jthread> th;
  for (u32 i = 0; i < T; i++)
    th.emplace_back(f, i);
}

static u64 heavy(const Ov &o, u32 edge) {
  return std::lower_bound(o.begin(), o.end(), std::pair<u32, u64>{edge, 0})
      ->second;
}

// Monotone priority queue: a ring of 4096 buckets (keys never span more than
// 4095), a linked list per bucket in a pooled array, and a two-level bitmap to
// jump to the next non-empty bucket.
struct Dial {
  u32 head[RING], fr = NIL, used = 0, at = 0;
  u64 bits[RING / 64] = {}, top = 0, *pool;
  Dial(u64 *p) : pool(p) { memset(head, 0xFF, sizeof head); }
  void push(u32 b, u32 node) {
    u32 h = head[b], i = fr != NIL ? fr : used++;
    if (h == NIL)
      bits[b / 64] |= 1ULL << b % 64, top |= 1ULL << b / 64;
    fr = fr != NIL ? pool[i] >> 32 : fr;
    pool[i] = node | (u64)h << 32, head[b] = i;
  }
  bool pop(u32 &b, u32 &node) {
    for (; top;) {
      u32 h = head[at], w = at / 64;
      if (h != NIL) {
        u64 e = pool[h];
        node = e, b = at, head[at] = e >> 32, pool[h] = (u64)fr << 32, fr = h;
        if (head[at] == NIL && !(bits[w] &= ~(1ULL << at % 64)))
          top &= ~(1ULL << w);
        return 1;
      }
      if (u64 rest = bits[w] >> at % 64)
        at += std::countr_zero(rest);
      else {
        u32 nw = std::countr_zero(top & -2ULL << w);
        at = nw * 64 + std::countr_zero(bits[nw]);
      }
    }
    return 0;
  }
  void clear() {
    for (; top; top &= top - 1)
      for (u32 w = std::countr_zero(top); bits[w]; bits[w] &= bits[w] - 1)
        head[w << 6 | std::countr_zero(bits[w])] = NIL;
    fr = NIL, used = at = 0;
  }
};

// Exact 64-bit Dijkstra (a max-heap on ~distance) for the rare long answers.
static long slow(u32 s, u32 t) {
  auto inv = anon<u64>(n); // ~distance, 0 = unseen
  std::priority_queue<std::pair<u64, u32>> pq;
  long ans = -1;
  for (inv[s] = ~0ULL, pq.push({~0ULL, s}); !pq.empty();) {
    auto [k, u] = pq.top();
    pq.pop();
    if (k < inv[u])
      continue;
    if (u == t) {
      ans = ~k;
      break;
    }
    for (u32 i = first[u]; i < first[u + 1]; i++) {
      u32 v = adj[i] >> 11, w = adj[i] & 2047;
      u64 nk = k - (w == 2047 ? heavy(ov, i) : w);
      if (nk > inv[v])
        inv[v] = nk, pq.push({nk, v});
    }
  }
  return munmap(inv, n * 8ull + 4096), ans;
}

// Distances of both directions, 16 bits each and stored inverted (0 = unseen):
// the forward one in the low half of a word, the backward one in the high half.
// Words live in 512-entry pages that are allocated on the first write; page 0
// stays zero and backs every unseen page.
struct Dist {
  u32 *pt, *own, np = 1, *pool;
  u32 get(u32 v) { return pool[pt[v >> 9] << 9 | v & 511]; }
  void set(u32 v, u32 x) {
    u32 &p = pt[v >> 9];
    if (!p)
      own[np] = v >> 9, p = np++;
    pool[p << 9 | v & 511] = x;
  }
  void clear() {
    for (u32 i = 1; i < np; i++)
      memset(pool + (i << 9), 0, 2048), pt[own[i]] = 0;
    np = 1;
  }
};

// One direction of the search: its own graph, queue and lower bound. The
// forward graph is a CSR (first, adj); the reverse one has four slots per node
// (adj, cnt) and the edges beyond the fourth in sp.
struct Side {
  const u32 *first, *adj;
  const u16 *cnt;
  const Sp *sp;
  size_t nsp;
  const Ov *ov;
  Dial q;
  u32 low = 0; // no node with a smaller distance is left to settle
};

// Relaxes the edges of node u, settled at distance d, and records any meeting
// with the other direction in mu. SH is 0 for the forward half of a word.
template <int SH>
static void expand(Side &S, Dist &D, u32 u, u32 d, u32 &mu, bool &big) {
  auto relax = [&](u32 e, u64 wt) {
    u32 v = e >> 11, x = D.get(v);
    u64 nd = d + wt;
    if (__builtin_expect(nd >= 60000, 0)) {
      big = 1; // too long for 16 bits
      return;
    }
    u16 inv = ~(u16)nd, mine = x >> SH, other = x >> (16 - SH);
    if (inv > mine) {
      D.set(v, x & ~(0xFFFFu << SH) | inv << SH);
      S.q.push(nd >> 4, v);
      other && (mu = std::min(mu, (u32)nd + (u16)~other), 1);
      if constexpr (SH)
        _mm_prefetch((const char *)(S.adj + 4 * (size_t)v), _MM_HINT_T0),
            _mm_prefetch((const char *)(S.cnt + v), _MM_HINT_T0);
      else
        _mm_prefetch((const char *)(S.first + v), _MM_HINT_T0);
    }
  };
  u32 lo, hi;
  if constexpr (SH)
    lo = 4 * u, hi = lo + std::min<u32>(S.cnt[u], 4);
  else {
    u64 r;
    memcpy(&r, S.first + u, 8);
    lo = r, hi = r >> 32;
  }
  for (u32 i = lo; i < hi; i++) {
    u32 e = S.adj[i], w = e & 2047;
    relax(e, w == 2047 ? heavy(*S.ov, i) : w);
  }
  if constexpr (SH)
    if (S.cnt[u] > 4)
      for (auto *a =
               std::lower_bound(S.sp, S.sp + S.nsp, u,
                                [](const Sp &x, u32 v) { return x.u < v; });
           a < S.sp + S.nsp && a->u == u; a++)
        relax(a->e, a->w);
}

static long fast(u32 s, u32 t, Dist &D, Side &F, Side &R) {
  u32 b, u, mu = ~0u;
  bool big = 0;
  D.set(s, 0xFFFF), D.set(t, D.get(t) | 0xFFFF0000u);
  F.q.push(0, s), R.q.push(0, t), F.low = R.low = 0;
  for (;;) {
    Side &S = F.low <= R.low ? F : R; // grow the side that is behind
    if (!S.q.pop(b, u))
      break;
    S.low = b << 4;
    if (F.low + R.low >= mu)
      break; // no path through an unsettled node can beat mu
    u32 x = D.get(u), d = (u16) ~(&S == &F ? x : x >> 16);
    if (d >> 4 != b)
      continue;
    (&S == &F ? expand<0> : expand<16>)(S, D, u, d, mu, big);
  }
  F.q.clear(), R.q.clear(), D.clear();
  long ans = mu < 60000 || (mu != ~0u && !big) ? (long)mu : -1;
  return ans < 0 && big ? slow(s, t) : ans;
}

int main() {
  size_t glen, qlen, cut[CHUNKS + 1] = {};
  const char *file = map("/data/graph.txt", glen), *body = file;
  n = num(body), m = num(body);
  const size_t blen = glen - (body - file);
  for (u32 c = 1; c <= CHUNKS; c++) {
    size_t q = blen * c / CHUNKS;
    auto nl = (const char *)memchr(body + q, '\n', blen - q);
    cut[c] = nl ? nl + 1 - body : blen;
  }
  // e4: the first four edges of every node (target << 11 | weight), cnt: how
  // many edges every node has, nid: the new id of every node. Edges beyond the
  // fourth (about 1%) go to a spill list.
  size_t cells = 5 * (size_t)n + n / 2 + 64;
  auto e4 = anon(cells), nid = e4 + 4 * (size_t)n + 16;
  auto cnt = (u16 *)(nid + n + 16);
  auto sp = anon<Sp>(m + 16);
  auto vis = anon<u64>(2 * (n / 64 + 1)), has_in = vis + n / 64 + 1;
  first = anon(n + 2), adj = anon(m);
  std::atomic<size_t> job = 0, seed = 0, rank = 0, spn = 0;
  std::barrier sync(T);
  Ov ovs, ov0[T], ov1[T];
  par([&](u32 tid) {
    for (size_t c; (c = job.fetch_add(1, rx)) < CHUNKS;)
      for (auto q = body + cut[c], e = body + cut[c + 1]; q < e;) {
        u32 U[16], V[16], W[16], k = 0;
        for (; k < 16 && q < e; k++)
          U[k] = num(q), V[k] = num(q), W[k] = num(q),
          __builtin_prefetch(cnt + U[k], 1),
          __builtin_prefetch(e4 + 4 * (size_t)U[k], 1);
        for (u32 j = 0; j < k; j++) {
          u32 u = U[j], v = V[j], w = W[j], e = v << 11 | std::min(w, 2047u);
          u32 slot = std::atomic_ref<u16>(cnt[u]).fetch_add(1, rx);
          if (slot < 4)
            e4[4 * (size_t)u + slot] = e,
                               w >= 2047 &&
                                   (ov0[tid].push_back({4 * u + slot, w}), 1);
          else
            sp[spn.fetch_add(1, rx)] = {u, e, w};
          if (!(has_in[v / 64] >> v % 64 & 1))
            std::atomic_ref<u64>(has_in[v / 64]).fetch_or(1ULL << v % 64, rx);
        }
      }
    sync.arrive_and_wait(); // LAP:parse
    if (!tid) {
      for (auto &v : ov0)
        ovs.insert(ovs.end(), v.begin(), v.end());
      std::sort(ovs.begin(), ovs.end());
      sort_sp(sp, spn);
    }
    // Renumber by breadth-first regions of up to 8192 nodes, so a search
    // touches nearby memory. (The spill edges are left out: only the order of
    // the regions depends on them.)
    auto claim = [&](u32 v) {
      std::atomic_ref<u64> cell(vis[v / 64]);
      u64 bit = 1ULL << v % 64;
      return !(cell.load(rx) & bit) && !(cell.fetch_or(bit, rx) & bit);
    };
    u32 qb[8192];
    for (size_t blk; (blk = seed.fetch_add(1024, rx)) < n;)
      for (u32 r = blk; r < std::min<size_t>(blk + 1024, n); r++) {
        if (!claim(r))
          continue;
        u32 h = 0, t = 1;
        for (qb[0] = r; h < t && t < 8192; h++) {
          h + 4 < t && (__builtin_prefetch(e4 + 4 * (size_t)qb[h + 4]), 1);
          for (u32 c = 0, x = qb[h]; c < std::min<u32>(cnt[x], 4) && t < 8192;
               c++)
            if (u32 v = e4[4 * (size_t)x + c] >> 11; claim(v))
              qb[t++] = v, __builtin_prefetch(e4 + 4 * (size_t)v),
              __builtin_prefetch(cnt + v);
        }
        size_t base = rank.fetch_add(t, rx);
        for (u32 k = 0; k < t; k++)
          nid[qb[k]] = base + k, first[base + k + 1] = cnt[qb[k]];
      }
    sync.arrive_and_wait(); // LAP:bfs
    tid || (std::partial_sum(first, first + n + 1, first), 1);
    sync.arrive_and_wait(); // LAP:prefix2
    size_t nsp = spn;
    for (size_t u = (size_t)n * tid / T; u < (size_t)n * (tid + 1) / T; u++) {
      u32 dst = first[nid[u]], cu = cnt[u];
      for (u32 c = 0; c < 4; c++)
        _mm_prefetch((const char *)(nid + (e4[4 * (u + 3) + c] >> 11)),
                     _MM_HINT_T0);
      for (u32 c = 0; c < std::min(cu, 4u); c++, dst++) {
        u32 e = e4[4 * u + c];
        adj[dst] = nid[e >> 11] << 11 | (e & 2047);
        (e & 2047) == 2047 &&
            (ov1[tid].push_back({dst, heavy(ovs, (u32)(4 * u + c))}), 1);
      }
      if (cu > 4)
        for (auto *a = std::lower_bound(
                 sp, sp + nsp, u,
                 [](const Sp &x, size_t v) { return x.u < v; });
             a < sp + nsp && a->u == u; a++, dst++) {
          adj[dst] = nid[a->e >> 11] << 11 | (a->e & 2047);
          (a->e & 2047) == 2047 && (ov1[tid].push_back({dst, a->w}), 1);
        }
    }
  }); // LAP:build
  for (auto &v : ov1)
    ov.insert(ov.end(), v.begin(), v.end());
  std::sort(ov.begin(), ov.end());

  // Answer what needs no search; the rest become (source, target, index)
  // triples in the new ids, sorted by source.
  std::vector<std::array<u32, 3>> work;
  std::vector<long> ans;
  const char *qp = map("/data/queries.txt", qlen), *qe = qp + qlen;
  while (qp < qe) {
    u32 s = num(qp), t = num(qp), a = nid[s], b = nid[t];
    ans.push_back(s == t ? 0 : -1);
    if (s != t && (has_in[t / 64] >> t % 64 & 1) && first[a + 1] > first[a])
      work.push_back({a, b, (u32)ans.size() - 1});
  }
  munmap(e4, cells * 4 + 4096);
  // The reverse graph in the final ids, four slots per node like the input.
  re4 = anon(4 * (size_t)n + 64), rcnt = anon<u16>(n + 8);
  rsp = anon<Sp>(m + 16);
  std::atomic<size_t> rcount = 0;
  Ov rov1[T];
  par([&](u32 tid) {
    size_t lo = (size_t)n * tid / T, hi = (size_t)n * (tid + 1) / T;
    for (size_t u = lo; u < hi; u++)
      for (u32 i = first[u]; i < first[u + 1]; i++) {
        u32 pv = adj[i + 16] >> 11;
        __builtin_prefetch(rcnt + pv, 1),
            __builtin_prefetch(re4 + 4 * (size_t)pv, 1);
        u32 e = adj[i], v = e >> 11, w = e & 2047, r = u << 11 | w;
        u32 slot = std::atomic_ref<u16>(rcnt[v]).fetch_add(1, rx);
        u64 hw = w == 2047 ? heavy(ov, i) : w;
        if (slot < 4)
          re4[4 * (size_t)v + slot] = r,
                              w == 2047 &&
                                  (rov1[tid].push_back({4 * v + slot, hw}), 1);
        else
          rsp[rcount.fetch_add(1, rx)] = {v, r, hw};
      }
  }); // LAP:reverse
  rn = rcount;
  sort_sp(rsp, rn);
  for (auto &v : rov1)
    rov.insert(rov.end(), v.begin(), v.end());
  std::sort(rov.begin(), rov.end());
  std::sort(work.begin(), work.end()); // LAP:prep

  // Each thread starts on its own slice of the queries, then helps with the
  // others.
  std::atomic<size_t> cur[T * 8]; // one cache line per slice
  for (u32 k = 0; k < T; k++)
    cur[k * 8] = k * work.size() / T;
  par([&](u32 tid) {
    u32 P = (n >> 9) + 2;                                  // pages of 512 nodes
    auto pool = anon<u64>(2 * POOL + 257 * (size_t)P + 2); // queues | pages
    auto pt = (u32 *)(pool + 2 * POOL);
    Dist D{pt, pt + P, 1, pt + 2 * P};
    Side F{first, adj, 0, 0, 0, &ov, Dial(pool)},
        R{0, re4, rcnt, rsp, rn, &rov, Dial(pool + POOL)};
    for (u32 k = 0; k < T; k++)
      for (size_t j, who = (tid + k) % T, end = (who + 1) * work.size() / T;
           (j = cur[who * 8].fetch_add(1, rx)) < end;)
        ans[work[j][2]] = fast(work[j][0], work[j][1], D, F, R);
  }); // LAP:queries

  std::string out;
  char buf[24];
  for (long a : ans)
    out.append(buf, std::to_chars(buf, buf + 24, a).ptr).push_back('\n');
  int fd = open("/out/distances.txt", O_WRONLY | O_CREAT | O_TRUNC, 0666);
  write(fd, out.data(), out.size());
  _exit(0);
}
