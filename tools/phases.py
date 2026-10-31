"""Build a throw-away copy of a track with stderr phase timers and print them.
usage: python tools/phases.py <d|b> "anchor1=label1" "anchor2=label2" ...   (a timer is inserted BEFORE the line containing anchor)"""
import os, re, shutil, subprocess, sys, tempfile
T = sys.argv.pop(1)
TAG = os.environ.get("SD_TAG", "prof")  # set a different SD_TAG per concurrent session
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
tmp = os.path.join(tempfile.gettempdir(), f"sd{TAG}"); shutil.rmtree(tmp, ignore_errors=True)
shutil.copytree(os.path.join(root, f"track-{T}"), tmp, ignore=shutil.ignore_patterns("target"))
p = os.path.join(tmp, "src", "main.rs"); s = open(p).read().replace("\r\n", "\n")
RSS = 'let rss = std::fs::read_to_string("/proc/self/status").unwrap().lines().filter(|l| l.starts_with("VmRSS")).map(|l| l.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() / 1024).next().unwrap();'
s = s.replace("fn main() {\n", "fn main() {\n    let t0 = std::time::Instant::now();\n    let lap = |m: &str| { " + RSS + " eprintln!(\"{m:>8} {:?} rss {rss} MB\", t0.elapsed()) };\n", 1)
for a in sys.argv[1:]:
    anchor, label = a.rsplit("=", 1)
    s = s.replace(anchor, f'lap("{label}");\n    {anchor}', 1)
open(p, "w", newline="\n").write(s)
subprocess.run(["docker", "build", "-q", "--network=none", "-t", f"sd-{TAG}", tmp], check=True, stdout=subprocess.DEVNULL)
for _ in range(3):
    subprocess.run(["docker", "run", "--rm", "--cpuset-cpus=" + os.environ.get("CPUS", "0-11"), "--mount", f"type=volume,src=sddata,dst=/data,volume-subpath={T},readonly",
                    "-v", os.path.join(root, ".out", f"new-{T}") + ":/out", f"sd-{TAG}"], env={**os.environ, "MSYS_NO_PATHCONV": "1"})
    print()
