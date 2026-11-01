# usage: python3 -I tools/c_board_analysis.py board.json  -- Track C table from the public board feed
# (https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json), each team's best scored run, and the scoring
# calculator of harness/scoring.py (Tref 14.44 s, Mref 367 MB, Sref 108, D 1).
import json, sys
b = json.load(open(sys.argv[1]))
K = lambda T, M, S, D=1: (14.44 / T) ** .5 * min(367e6 / M, 50) ** .2 * min(108 / S, 1) ** .15 * (1 / D) ** .15
rows = []
for t in b["teams"]:
    c = t["tracks"].get("c") or {}
    be = c.get("best")
    if be and be.get("K"):
        rows.append((be["K"], t["team"], be["T_s"] * 1e3, be["M_bytes"] / 1e6, be["S"], be["D"], "/".join(be.get("lang") or []), be["commit"][:7]))
rows.sort(reverse=True)
print(f"feed generated {b['generated_at']}, {len(rows)} teams with a scored Track C run")
print(f"{'#':>3} {'K':>6} {'T ms':>6} {'M MB':>6} {'S':>4}  lang  team")
for i, r in enumerate(rows[:20]):
    print(f"{i+1:3} {r[0]:6.2f} {r[2]:6.2f} {r[3]:6.2f} {r[4]:4}  {r[6]:5} {r[1]}")
ours = next(r for r in rows if r[1] == "Bharat Labs")
print("ours:", ours)
print("\nT (ms) needed for a target K at given M and S:")
for M, S in [(13.8, 629), (12.0, 629), (10.0, 629), (10.0, 450), (9.3, 451), (8.5, 350)]:
    need = lambda k: 14440 * ((367 / M) ** .2 * min(108 / S, 1) ** .15 / k) ** 2
    print(f"  M {M:5.1f} MB S {S}: " + "  ".join(f"K{k}: {need(k):5.1f}" for k in (35, 40, 45, 50, 60, 70, 80)))
print("\nsensitivity at our point (T 36, M 13.8, S 629): dK/K per unit")
T, M, S = 36, 13.8, 629
k0 = K(T / 1e3, M * 1e6, S)
for name, k1 in [("-1 ms T", K((T - 1) / 1e3, M * 1e6, S)), ("-1 MB M", K(T / 1e3, (M - 1) * 1e6, S)), ("-10 lines S", K(T / 1e3, M * 1e6, S - 10))]:
    print(f"  {name}: {100 * (k1 / k0 - 1):+.2f}%")
