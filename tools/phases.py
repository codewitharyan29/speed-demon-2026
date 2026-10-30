"""Build a throw-away copy of track-d with stderr phase timers and print them.
usage: python tools/phases.py "anchor1=label1" "anchor2=label2" ...   (a timer is inserted BEFORE the line containing anchor)"""
import os, re, shutil, subprocess, sys, tempfile
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
tmp = os.path.join(tempfile.gettempdir(), "sdprof"); shutil.rmtree(tmp, ignore_errors=True)
shutil.copytree(os.path.join(root, "track-d"), tmp, ignore=shutil.ignore_patterns("target"))
p = os.path.join(tmp, "src", "main.rs"); s = open(p).read().replace("\r\n", "\n")
s = s.replace("fn main() {\n", "fn main() {\n    let t0 = std::time::Instant::now();\n    let lap = |m: &str| eprintln!(\"{m:>8} {:?}\", t0.elapsed());\n", 1)
for a in sys.argv[1:]:
    anchor, label = a.rsplit("=", 1)
    s = s.replace(anchor, f'lap("{label}");\n    {anchor}', 1)
open(p, "w", newline="\n").write(s)
subprocess.run(["docker", "build", "-q", "--network=none", "-t", "sd-prof", tmp], check=True, stdout=subprocess.DEVNULL)
for _ in range(3):
    subprocess.run(["docker", "run", "--rm", "--cpuset-cpus=0-11", "--mount", "type=volume,src=sddata,dst=/data,volume-subpath=d,readonly",
                    "-v", os.path.join(root, ".out", "new-d") + ":/out", "sd-prof"], env={**os.environ, "MSYS_NO_PATHCONV": "1"})
    print()
