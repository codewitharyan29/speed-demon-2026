"""Print the top of the live board for a track, our standing, and the time needed to beat the leader.

usage: python tools/board_watch.py [--track a] [--team NAME] [--top 5] [--margin 1.03]
Our team is found by matching board run commits against this repo's git history, or by --team (substring).
"""
import argparse, json, subprocess, urllib.request

URL = "https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json"


def K(t, m_mb, s, d=1):
    return (32.05 / t) ** 0.5 * (227.98 / m_mb) ** 0.2 * (56 / max(s, 56)) ** 0.15 / d


def t_needed(k_target, m_mb, s):
    return 32.05 / (k_target / ((227.98 / m_mb) ** 0.2 * (56 / max(s, 56)) ** 0.15)) ** 2


def our_commits():
    out = subprocess.run(["git", "log", "--all", "--format=%H"], capture_output=True, text=True).stdout
    return set(out.split())


def row(team, best):
    return (f"{team[:24]:<24} {','.join(best['lang'])[:8]:<8} {best['K']:>7.2f} {best['points']:>7.2f} "
            f"{best['T_s'] * 1000:>8.2f} {best['M_bytes'] / 1e6:>7.2f} {best['S']:>5}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--track", default="a")
    ap.add_argument("--team")
    ap.add_argument("--top", type=int, default=5)
    ap.add_argument("--margin", type=float, default=1.03)
    a = ap.parse_args()
    req = urllib.request.Request(URL, headers={"User-Agent": "speed-demon-board-watch/1.0"})
    for attempt in range(4):  # the feed is rewritten every few seconds; a read can land mid-update
        try:
            board = json.loads(urllib.request.urlopen(req, timeout=30).read())
            break
        except ValueError:
            if attempt == 3:
                raise
    ranked = sorted(((t["team"], t["tracks"][a.track]["best"], t) for t in board["teams"]
                     if t["tracks"].get(a.track, {}).get("best") and t["tracks"][a.track]["best"].get("ok")),
                    key=lambda r: -r[1]["K"])
    head = f"{'team':<24} {'lang':<8} {'K':>7} {'points':>7} {'T_ms':>8} {'M_MB':>7} {'S':>5}"
    print(f"board generated {board['generated_at']}, track {a.track}, {len(ranked)} scored teams")
    print(head)
    for team, best, _ in ranked[:a.top]:
        print(row(team, best))

    commits = our_commits()
    mine = None
    for i, (team, best, t) in enumerate(ranked):
        runs = [t["tracks"][a.track].get(k) or {} for k in ("best", "latest")]
        if (a.team and a.team.lower() in team.lower()) or (not a.team and any(r.get("commit") in commits for r in runs)):
            mine = (i + 1, team, best, t)
            break
    leader = ranked[0][1]
    if not mine:
        print("\nour team not found (no run commit in git history; pass --team)")
    else:
        rank, team, best, t = mine
        print(f"\nours: #{rank} of {len(ranked)}\n{head}\n{row(team, best)}")
        latest = t["tracks"][a.track].get("latest") or {}
        print(f"latest run: {latest.get('commit', '')[:7]} stage={latest.get('stage')} T={latest.get('T_s')} K={latest.get('K')}")
        print(f"gap to leader: {leader['K'] - best['K']:+.2f} K ({best['K'] / leader['K'] * 100:.0f}% of leader)")
        m, s = best["M_bytes"] / 1e6, best["S"]
        print(f"at our M={m:.1f} MB, S={s}: K={leader['K'] * a.margin:.2f} (leader x {a.margin}) needs T <= "
              f"{t_needed(leader['K'] * a.margin, m, s) * 1000:.2f} ms")
    lm, ls = leader["M_bytes"] / 1e6, leader["S"]
    print(f"leader {ranked[0][0]}: K={leader['K']:.2f} at T={leader['T_s'] * 1000:.1f} ms, M={lm:.1f} MB, S={ls}")
    for mm, ss in ((19.5, 353), (15, 300), (13, 250)):
        print(f"  for K={leader['K'] * a.margin:.1f} at M={mm} MB, S={ss}: T <= {t_needed(leader['K'] * a.margin, mm, ss) * 1000:.2f} ms")


if __name__ == "__main__":
    main()
