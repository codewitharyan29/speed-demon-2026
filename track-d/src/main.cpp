#include <fcntl.h>
#include <immintrin.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include <algorithm>
#include <atomic>
#include <barrier>
#include <bit>
#include <charconv>
#include <cstdint>
#include <cstring>
#include <queue>
#include <string>
#include <thread>
#include <vector>
using u16 = uint16_t;
using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;
using Ov = std::vector<std::pair<u32, u64>>;  // (edge index, true weight) of edges with weight >= 2047
constexpr u32 NIL = ~0u, RING = 4096, CAP = 65536, POOL = 65536, T = 12, QT = 12, CHUNKS = 48;
constexpr auto rx = std::memory_order_relaxed;

static void* anon(size_t bytes) {
  void* p = mmap(nullptr, bytes + 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  return p;
}

static const char* map(const char* path, size_t& len) {
  int fd = open(path, O_RDONLY);
  struct stat st;
  fstat(fd, &st);
  len = st.st_size;
  const void* p = mmap(nullptr, len, PROT_READ, MAP_SHARED, fd, 0);
  close(fd);
  return (const char*)p;
}

static u32 num(const char*& p) {
  while (*p < '0') p++;
  u32 x = 0;
  while (*p >= '0') x = x * 10 + (*p++ - '0');
  return x;
}

// Just past the next newline, eight bytes at a time.
static const char* next_line(const char* p, const char* end) {
  for (; p + 8 <= end; p += 8) {
    u64 w;
    memcpy(&w, p, 8);
    w ^= 0x0A0A0A0A0A0A0A0AULL;
    u64 z = (w - 0x0101010101010101ULL) & ~w & 0x8080808080808080ULL;
    if (z) return p + (std::countr_zero(z) >> 3) + 1;
  }
  while (p < end && *p != '\n') p++;
  return p + 1;
}

template <class F>
static void par(u32 k, F f) {
  std::vector<std::thread> th;
  for (u32 i = 0; i < k; i++) th.emplace_back(f, i);
  for (auto& t : th) t.join();
}

static u64 heavy(const Ov& ov, u32 edge) {
  return std::lower_bound(ov.begin(), ov.end(), std::make_pair(edge, (u64)0))->second;
}

// Monotone priority queue: a ring of 4096 buckets (keys never span more than 4095), a linked
// list per bucket in a pooled array, and a two-level bitmap to jump to the next non-empty bucket.
struct Dial {
  u32 head[RING], free_ = NIL, used = 0, at = 0, len = 0;
  u64 bits[RING / 64] = {}, top = 0, *pool;
  explicit Dial(u64* p) : pool(p) { memset(head, 0xFF, sizeof head); }
  void push(u32 key, u32 node) {
    u32 b = key & (RING - 1), h = head[b], i = free_ != NIL ? free_ : used++;
    if (h == NIL) bits[b >> 6] |= 1ULL << (b & 63), top |= 1ULL << (b >> 6);
    if (free_ != NIL) free_ = pool[i] >> 32;
    pool[i] = node | (u64)h << 32;
    head[b] = i;
    len++;
  }
  bool pop(u32& key, u32& node) {
    if (!len) return false;
    for (;;) {
      u32 b = at & (RING - 1), h = head[b], w = b >> 6;
      if (h != NIL) {
        u64 e = pool[h];
        node = (u32)e, key = at, head[b] = e >> 32, pool[h] = (u64)free_ << 32, free_ = h, len--;
        if (head[b] == NIL && !(bits[w] &= ~(1ULL << (b & 63)))) top &= ~(1ULL << w);
        return true;
      }
      if (u64 rest = bits[w] >> (b & 63)) {
        at += std::countr_zero(rest);
      } else {
        u32 nw = (w + 1 + std::countr_zero(std::rotr(top, (w + 1) & 63))) & 63;
        at += ((nw << 6 | std::countr_zero(bits[nw])) - b) & (RING - 1);
      }
    }
  }
  void clear() {
    for (; top; top &= top - 1)
      for (u32 w = std::countr_zero(top); bits[w]; bits[w] &= bits[w] - 1) head[w << 6 | std::countr_zero(bits[w])] = NIL;
    free_ = NIL, used = at = len = 0;
  }
};

struct Graph {
  const u32 *first, *adj;
  const Ov* ov;
  u32 n;
};

// Exact 64-bit Dijkstra for the few queries whose answer does not fit the fast path.
static i64 slow(const Graph& g, u32 s, u32 t) {
  u64* inv = (u64*)anon(8ull * g.n);  // ~distance, 0 = unseen
  std::priority_queue<std::pair<u64, u32>, std::vector<std::pair<u64, u32>>, std::greater<>> pq;
  i64 ans = -1;
  inv[s] = ~0ULL, pq.push({0, s});
  while (!pq.empty()) {
    auto [d, u] = pq.top();
    pq.pop();
    if (~d < inv[u]) continue;
    if (u == t) {
      ans = d;
      break;
    }
    for (u32 i = g.first[u]; i < g.first[u + 1]; i++) {
      u32 v = g.adj[i] >> 11, w = g.adj[i] & 2047;
      u64 nd = d + (w == 2047 ? heavy(*g.ov, i) : w);
      if (~nd > inv[v]) inv[v] = ~nd, pq.push({nd, v});
    }
  }
  munmap(inv, 8ull * g.n + 4096);
  return ans;
}

// 16-bit distances, stored inverted so that untouched (zero) memory means "unseen".
static i64 fast(const Graph& g, u32 s, u32 t, u16* dist, u32* touched, Dial& q) {
  u32 d, u, count = 1;
  i64 ans = -1;
  bool big = false, bad = false;
  dist[s] = 0xFFFF, touched[0] = s, q.push(0, s);
  while (q.pop(d, u)) {
    if ((u16)~d < dist[u]) continue;
    if (u == t) {
      ans = d;
      break;
    }
    u64 range;
    memcpy(&range, g.first + u, 8);
    _mm_prefetch((const char*)(g.adj + (u32)range), _MM_HINT_T0);
    for (u32 i = (u32)range; i < range >> 32; i++) {
      u32 e = g.adj[i], v = e >> 11, w = e & 2047;
      if (__builtin_expect(w == 2047, 0)) {
        u64 hw = heavy(*g.ov, i);
        if (hw > 4095) {  // beyond the ring: exact unless below 60000, where it could matter
          (hw < 60000 ? bad : big) = true;
          continue;
        }
        w = hw;
      }
      u32 nd = d + w;
      if (__builtin_expect(nd >= 60000, 0)) {
        big = true;
        continue;
      }
      u16 inv = ~(u16)nd, cur = dist[v];
      if (inv > cur) {
        dist[v] = inv;
        if (!cur) touched[count++ & (CAP - 1)] = v;
        q.push(nd, v);
        _mm_prefetch((const char*)(g.first + v), _MM_HINT_T0);
      }
    }
  }
  q.clear();
  if (count > CAP) memset(dist, 0, 2ull * g.n);
  else
    for (u32 i = 0; i < count; i++) dist[touched[i]] = 0;
  return bad || (ans < 0 && big) ? slow(g, s, t) : ans;
}

int main() {
  size_t glen;
  const char* file = map("/data/graph.txt", glen);
  const char* body = file;
  const u32 n = num(body), m = num(body);
  const size_t blen = glen - (body - file);
  size_t cut[CHUNKS + 1] = {0, blen};
  for (u32 c = 1; c < CHUNKS; c++) {
    size_t q = blen * c / CHUNKS;
    while (q < blen && body[q] != '\n') q++;
    cut[c] = std::min(q + 1, blen);
  }
  cut[CHUNKS] = blen;

  // s0: edge counts, then CSR offsets in the file's ids. e0: packed edges (v << 11 | weight).
  u32 *s0 = (u32*)anon(4ull * (n + 2)), *e0 = (u32*)anon(4ull * m), *nid = (u32*)anon(4ull * n);
  u32 *first = (u32*)anon(4ull * (n + 2)), *adj = (u32*)anon(4ull * m);
  u64 *vis = (u64*)anon(8ull * (n / 64 + 1)), *has_in = (u64*)anon(8ull * (n / 64 + 1));
  std::atomic<size_t> job[2] = {0, 0}, seed = 0, rank = 0;
  std::barrier sync(T);
  Ov ovs, fov, ov0[T], ov1[T];

  par(T, [&](u32 tid) {
    auto lines = [&](int pass, auto f) {
      for (size_t c; (c = job[pass].fetch_add(1, rx)) < CHUNKS;)
        for (const char *q = body + cut[c], *e = body + cut[c + 1]; q < e;) f(q, e);
    };
    lines(0, [&](const char*& q, const char* e) {
      std::atomic_ref<u32>(s0[num(q) + 2]).fetch_add(1, rx);
      q = next_line(q, e);
    });
    sync.arrive_and_wait();  // LAP:pass1
    if (!tid)
      for (u32 i = 2; i < n + 2; i++) s0[i] += s0[i - 1];
    sync.arrive_and_wait();  // LAP:prefix1
    lines(1, [&](const char*& q, const char* e) {
      u32 u = num(q), v = num(q), w = num(q);
      q = next_line(q, e);
      u32 slot = std::atomic_ref<u32>(s0[u + 1]).fetch_add(1, rx);
      e0[slot] = v << 11 | std::min(w, 2047u);
      if (w >= 2047) ov0[tid].push_back({slot, w});
      if (!(has_in[v >> 6] >> (v & 63) & 1)) std::atomic_ref<u64>(has_in[v >> 6]).fetch_or(1ULL << (v & 63), rx);
    });
    sync.arrive_and_wait();  // LAP:pass2
    if (!tid) {
      madvise((void*)file, glen, MADV_DONTNEED);
      for (auto& v : ov0) ovs.insert(ovs.end(), v.begin(), v.end());
      std::sort(ovs.begin(), ovs.end());
    }
    // Renumber by breadth-first regions of up to 8192 nodes, so a search touches nearby memory.
    auto claim = [&](u32 v) {
      std::atomic_ref<u64> cell(vis[v >> 6]);
      u64 bit = 1ULL << (v & 63);
      return !(cell.load(rx) & bit) && !(cell.fetch_or(bit, rx) & bit);
    };
    u32 qb[8192];
    for (size_t blk; (blk = seed.fetch_add(1024, rx)) < n;)
      for (u32 r = blk; r < std::min<size_t>(blk + 1024, n); r++) {
        if (!claim(r)) continue;
        u32 h = 0, t = 1;
        for (qb[0] = r; h < t && t < 8192; h++)
          for (u32 i = s0[qb[h]]; i < s0[qb[h] + 1] && t < 8192; i++)
            if (u32 v = e0[i] >> 11; claim(v)) qb[t++] = v;
        size_t base = rank.fetch_add(t, rx);
        for (u32 k = 0; k < t; k++) nid[qb[k]] = base + k, first[base + k + 1] = s0[qb[k] + 1] - s0[qb[k]];
      }
    sync.arrive_and_wait();  // LAP:bfs
    if (!tid)
      for (u32 i = 1; i <= n; i++) first[i] += first[i - 1];
    sync.arrive_and_wait();  // LAP:prefix2
    for (size_t u = (size_t)n * tid / T; u < (size_t)n * (tid + 1) / T; u++) {
      u32 dst = first[nid[u]];
      for (u32 i = s0[u]; i < s0[u + 1]; i++, dst++) {
        _mm_prefetch((const char*)(nid + (e0[i + 8] >> 11)), _MM_HINT_T0);
        u32 e = e0[i];
        adj[dst] = nid[e >> 11] << 11 | (e & 2047);
        if ((e & 2047) == 2047) ov1[tid].push_back({dst, heavy(ovs, i)});
      }
    }
  });  // LAP:build
  for (auto& v : ov1) fov.insert(fov.end(), v.begin(), v.end());
  std::sort(fov.begin(), fov.end());

  // Answer what needs no search; the rest become (source, target) pairs in the new ids.
  struct Query {
    u32 s, t, idx;
  };
  std::vector<Query> work;
  std::vector<i64> ans;
  size_t qlen;
  const char *qp = map("/data/queries.txt", qlen), *qe = qp + qlen;
  for (;;) {
    while (qp < qe && *qp < '0') qp++;
    if (qp >= qe) break;
    u32 s = num(qp), t = num(qp), s2 = nid[s], t2 = nid[t];
    ans.push_back(s == t ? 0 : -1);
    if (s != t && (has_in[t >> 6] >> (t & 63) & 1) && first[s2 + 1] > first[s2]) work.push_back({s2, t2, (u32)ans.size() - 1});
  }
  munmap(s0, 4ull * (n + 2) + 4096), munmap(e0, 4ull * m + 4096), munmap(nid, 4ull * n + 4096);
  std::sort(work.begin(), work.end(), [](const Query& a, const Query& b) { return a.s < b.s; });  // LAP:prep

  // Each thread starts on its own slice of the sorted queries, then helps with the others.
  struct alignas(64) Cursor {
    std::atomic<size_t> next;
  } cur[QT];
  for (u32 k = 0; k < QT; k++) cur[k].next = k * work.size() / QT;
  Graph g{first, adj, &fov, n};
  par(QT, [&](u32 tid) {
    u32* touched = (u32*)anon(4ull * CAP + 8ull * POOL + 2ull * n);  // touched | queue pool | distances
    u64* pool = (u64*)(touched + CAP);
    u16* dist = (u16*)(pool + POOL);
    Dial q(pool);
    for (u32 k = 0; k < QT; k++) {
      u32 who = (tid + k) % QT;
      for (size_t j, end = (who + 1) * work.size() / QT; (j = cur[who].next.fetch_add(1, rx)) < end;)
        ans[work[j].idx] = fast(g, work[j].s, work[j].t, dist, touched, q);
    }
  });  // LAP:queries

  std::string out;
  char buf[24];
  for (i64 a : ans) out.append(buf, std::to_chars(buf, buf + 24, a).ptr).push_back('\n');
  int fd = open("/out/distances.txt", O_WRONLY | O_CREAT | O_TRUNC, 0666);
  write(fd, out.data(), out.size());
  _exit(0);
}
