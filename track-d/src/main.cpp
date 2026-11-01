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
constexpr u32 NIL = ~0u, RING = 4096, CAP = 65536, POOL = 65536, T = 12;
constexpr u32 CHUNKS = 48;
constexpr auto rx = std::memory_order_relaxed;
static u32 n, m, *first, *adj; // CSR (renumbered): offsets and packed edges
static Ov ov;                  // true weights of edges packed as 2047

template <class P = u32> static P *anon(size_t count) {
  return (P *)mmap(0, count * sizeof(P) + 4096, 3, MAP_PRIVATE | MAP_ANONYMOUS,
                   -1, 0);
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
  u32 head[RING], fr = NIL, used = 0, at = 0, len = 0;
  u64 bits[RING / 64] = {}, top = 0, *pool;
  Dial(u64 *p) : pool(p) { memset(head, 0xFF, sizeof head); }
  void push(u32 key, u32 node) {
    u32 b = key % RING, h = head[b], i = fr != NIL ? fr : used++;
    if (h == NIL)
      bits[b / 64] |= 1ULL << b % 64, top |= 1ULL << b / 64;
    fr = fr != NIL ? pool[i] >> 32 : fr;
    pool[i] = node | (u64)h << 32, head[b] = i, len++;
  }
  bool pop(u32 &key, u32 &node) {
    for (; len;) {
      u32 b = at % RING, h = head[b], w = b / 64;
      if (h != NIL) {
        u64 e = pool[h];
        node = e, key = at, head[b] = e >> 32, pool[h] = (u64)fr << 32;
        fr = h, len--;
        if (head[b] == NIL && !(bits[w] &= ~(1ULL << b % 64)))
          top &= ~(1ULL << w);
        return 1;
      }
      if (u64 rest = bits[w] >> b % 64)
        at += std::countr_zero(rest);
      else {
        u32 nw = (w + 1 + std::countr_zero(std::rotr(top, (w + 1) & 63))) & 63;
        at += ((nw << 6 | std::countr_zero(bits[nw])) - b) % RING;
      }
    }
    return 0;
  }
  void clear() {
    for (; top; top &= top - 1)
      for (u32 w = std::countr_zero(top); bits[w]; bits[w] &= bits[w] - 1)
        head[w << 6 | std::countr_zero(bits[w])] = NIL;
    fr = NIL, used = at = len = 0;
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

// 16-bit distances, stored inverted so that untouched (zero) memory means
// "unseen".
static long fast(u32 s, u32 t, u16 *dist, u32 *touched, Dial &q) {
  u32 d, u, count = 1;
  long ans = -1;
  bool big = 0, bad = 0;
  dist[s] = 0xFFFF, touched[0] = s, q.push(0, s);
  while (q.pop(d, u)) {
    if ((u16)~d < dist[u])
      continue;
    if (u == t) {
      ans = d;
      break;
    }
    u64 r;
    memcpy(&r, first + u, 8);
    for (u32 i = (u32)r; i < r >> 32; i++) {
      u32 e = adj[i], v = e >> 11, w = e & 2047;
      u64 nd = d + (w == 2047 ? heavy(ov, i) : w);
      if (__builtin_expect(nd >= 60000 || nd - d > 4095, 0)) {
        (nd >= 60000 ? big : bad) = 1; // too long for 16 bits, or the ring
        continue;
      }
      u16 inv = ~(u16)nd, cur = dist[v];
      if (inv > cur) {
        dist[v] = inv;
        if (!cur)
          touched[count++ % CAP] = v;
        q.push(nd, v);
        _mm_prefetch((const char *)(first + v), _MM_HINT_T0);
      }
    }
  }
  q.clear();
  if (count > CAP)
    memset(dist, 0, n * 2ull);
  else
    for (u32 i = 0; i < count; i++)
      dist[touched[i]] = 0;
  return bad || (ans < 0 && big) ? slow(s, t) : ans;
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
  // s0: edge counts, then offsets in the file's ids; e0: packed edges
  // (target << 11 | weight); nid: new id of each node.
  auto s0 = anon(2 * n + m + 2), e0 = s0 + n + 2, nid = e0 + m;
  auto vis = anon<u64>(2 * (n / 64 + 1)), has_in = vis + n / 64 + 1;
  first = anon(n + 2), adj = anon(m);
  std::atomic<size_t> job[2] = {0, 0}, seed = 0, rank = 0;
  std::barrier sync(T);
  Ov ovs, ov0[T], ov1[T];
  par([&](u32 tid) {
    auto lines = [&](int pass, auto f) {
      for (size_t c; (c = job[pass].fetch_add(1, rx)) < CHUNKS;)
        for (auto q = body + cut[c], e = body + cut[c + 1]; q < e;)
          f(q, e);
    };
    lines(0, [&](auto &q, auto e) {
      u32 U[16], k = 0;
      for (; k < 16 && q < e; k++)
        U[k] = num(q), q = next_line(q, e),
        __builtin_prefetch(s0 + U[k] + 2, 1);
      while (k--)
        std::atomic_ref<u32>(s0[U[k] + 2]).fetch_add(1, rx);
    });
    sync.arrive_and_wait(); // LAP:pass1
    tid || (std::partial_sum(s0 + 1, s0 + n + 2, s0 + 1), 1);
    sync.arrive_and_wait(); // LAP:prefix1
    lines(1, [&](auto &q, auto e) {
      u32 U[16], V[16], W[16], k = 0;
      for (; k < 16 && q < e; k++)
        U[k] = num(q), V[k] = num(q), W[k] = num(q),
        __builtin_prefetch(s0 + U[k] + 1, 1);
      for (u32 j = 0; j < k; j++) {
        u32 u = U[j], v = V[j], w = W[j];
        u32 slot = std::atomic_ref<u32>(s0[u + 1]).fetch_add(1, rx);
        e0[slot] = v << 11 | std::min(w, 2047u);
        w >= 2047 && (ov0[tid].push_back({slot, w}), 1);
        if (!(has_in[v / 64] >> v % 64 & 1))
          std::atomic_ref<u64>(has_in[v / 64]).fetch_or(1ULL << v % 64, rx);
      }
    });
    sync.arrive_and_wait(); // LAP:pass2
    if (!tid) {
      for (auto &v : ov0)
        ovs.insert(ovs.end(), v.begin(), v.end());
      std::sort(ovs.begin(), ovs.end());
    }
    // Renumber by breadth-first regions of up to 8192 nodes, so a search
    // touches nearby memory.
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
          h + 4 < t && (__builtin_prefetch(e0 + s0[qb[h + 4]]), 1);
          for (u32 i = s0[qb[h]]; i < s0[qb[h] + 1] && t < 8192; i++)
            if (u32 v = e0[i] >> 11; claim(v))
              qb[t++] = v, __builtin_prefetch(s0 + v);
        }
        size_t base = rank.fetch_add(t, rx);
        for (u32 k = 0; k < t; k++)
          nid[qb[k]] = base + k,
          first[base + k + 1] = s0[qb[k] + 1] - s0[qb[k]];
      }
    sync.arrive_and_wait(); // LAP:bfs
    tid || (std::partial_sum(first, first + n + 1, first), 1);
    sync.arrive_and_wait(); // LAP:prefix2
    for (size_t u = (size_t)n * tid / T; u < (size_t)n * (tid + 1) / T; u++) {
      u32 dst = first[nid[u]];
      for (u32 i = s0[u]; i < s0[u + 1]; i++, dst++) {
        _mm_prefetch((const char *)(nid + (e0[i + 8] >> 11)), _MM_HINT_T0);
        u32 e = e0[i];
        adj[dst] = nid[e >> 11] << 11 | (e & 2047);
        (e & 2047) == 2047 && (ov1[tid].push_back({dst, heavy(ovs, i)}), 1);
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
  munmap(s0, (2 * n + m + 2) * 4ull + 4096);
  std::sort(work.begin(), work.end()); // LAP:prep

  // Each thread starts on its own slice of the queries, then helps with the
  // others.
  std::atomic<size_t> cur[T * 8]; // one cache line per slice
  for (u32 k = 0; k < T; k++)
    cur[k * 8] = k * work.size() / T;
  par([&](u32 tid) {
    auto touched = anon(CAP + 2 * POOL + n / 2 + 1); // touched | pool | dist
    auto pool = (u64 *)(touched + CAP);
    Dial q(pool);
    for (u32 k = 0; k < T; k++)
      for (size_t j, who = (tid + k) % T, end = (who + 1) * work.size() / T;
           (j = cur[who * 8].fetch_add(1, rx)) < end;)
        ans[work[j][2]] =
            fast(work[j][0], work[j][1], (u16 *)(pool + POOL), touched, q);
  }); // LAP:queries

  std::string out;
  char buf[24];
  for (long a : ans)
    out.append(buf, std::to_chars(buf, buf + 24, a).ptr).push_back('\n');
  int fd = open("/out/distances.txt", O_WRONLY | O_CREAT | O_TRUNC, 0666);
  write(fd, out.data(), out.size());
  _exit(0);
}
