"""usage: python tools/board_c.py [commit-prefix]  -- waits for the board's Track C run of a commit (default HEAD) and prints it."""
import json, subprocess, sys, time, urllib.request
h = sys.argv[1] if len(sys.argv) > 1 else subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
for _ in range(60):
    try:
        b = json.loads(urllib.request.urlopen(urllib.request.Request("https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json", headers={"User-Agent": "x"}), timeout=30).read())
        for t in b["teams"]:
            c = t["tracks"].get("c", {}).get("latest")
            if c and c.get("commit", "").startswith(h):
                print(h, {k: c.get(k) for k in ("run_id", "stage", "T_s", "M_bytes", "S", "K", "failure")}); sys.exit()
    except Exception as e:
        pass
    time.sleep(30)
print("no run yet for", h)
